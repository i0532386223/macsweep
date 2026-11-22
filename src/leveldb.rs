//! A tiny, read-only LevelDB reader.
//!
//! Chrome keeps some small indexes in LevelDB, such as which numbered
//! "File System" folder belongs to which website. MacSweep only needs to
//! read them, and must never change a browser's files, so instead of
//! opening the database with a LevelDB library (which recovers and rewrites
//! files on open) this reads the files directly and changes nothing.
//!
//! Supports the write-ahead log (`*.log`) and table files (`*.ldb`, `*.sst`),
//! uncompressed or Snappy-compressed blocks. The newest value of each key
//! wins; deleted keys are dropped.
use std::collections::HashMap;
use std::path::Path;

const BLOCK: usize = 32 * 1024;
const TABLE_MAGIC: u64 = 0xdb47_7524_8b80_fb57;

/// key -> (sequence number, value, or None if deleted)
type Entries = HashMap<Vec<u8>, (u64, Option<Vec<u8>>)>;

fn put(map: &mut Entries, key: &[u8], seq: u64, value: Option<&[u8]>) {
    let newer = map.get(key).is_none_or(|(s, _)| seq >= *s);
    if newer {
        map.insert(key.to_vec(), (seq, value.map(<[u8]>::to_vec)));
    }
}

fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b < 0x80 {
            return Some(v);
        }
    }
    None
}

fn slice<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Option<&'a [u8]> {
    let s = buf.get(*pos..pos.checked_add(len)?)?;
    *pos += len;
    Some(s)
}

fn length_prefixed<'a>(buf: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let n = varint(buf, pos)? as usize;
    slice(buf, pos, n)
}

/// Joins the log's physical records back into whole write batches.
fn log_records(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut pending: Option<Vec<u8>> = None;
    let mut pos = 0;
    while pos + 7 <= data.len() {
        let left_in_block = BLOCK - pos % BLOCK;
        if left_in_block < 7 {
            pos += left_in_block; // block trailer padding
            continue;
        }
        let len = u16::from_le_bytes([data[pos + 4], data[pos + 5]]) as usize;
        let kind = data[pos + 6];
        let start = pos + 7;
        let Some(chunk) = data.get(start..start + len) else { break };
        pos = start + len;
        match kind {
            1 => out.push(chunk.to_vec()),       // full
            2 => pending = Some(chunk.to_vec()), // first
            3 => {
                if let Some(p) = pending.as_mut() {
                    p.extend_from_slice(chunk) // middle
                }
            }
            4 => {
                if let Some(mut p) = pending.take() {
                    p.extend_from_slice(chunk); // last
                    out.push(p);
                }
            }
            _ => {} // zero padding or unknown: skip
        }
    }
    out
}

fn read_batch(batch: &[u8], map: &mut Entries) -> Option<()> {
    let seq = u64::from_le_bytes(batch.get(0..8)?.try_into().ok()?);
    let count = u32::from_le_bytes(batch.get(8..12)?.try_into().ok()?) as u64;
    let mut pos = 12;
    for i in 0..count {
        let tag = *batch.get(pos)?;
        pos += 1;
        let key = length_prefixed(batch, &mut pos)?;
        match tag {
            1 => {
                let value = length_prefixed(batch, &mut pos)?;
                put(map, key, seq + i, Some(value));
            }
            0 => put(map, key, seq + i, None),
            _ => return None,
        }
    }
    Some(())
}

fn read_log(data: &[u8], map: &mut Entries) {
    for batch in log_records(data) {
        let _ = read_batch(&batch, map);
    }
}

/// One block of a table, decompressed.
fn block(file: &[u8], handle: (u64, u64)) -> Option<Vec<u8>> {
    let (off, size) = (handle.0 as usize, handle.1 as usize);
    let raw = file.get(off..off.checked_add(size)?)?;
    match *file.get(off + size)? {
        0 => Some(raw.to_vec()),
        1 => snap::raw::Decoder::new().decompress_vec(raw).ok(),
        _ => None, // a compression MacSweep doesn't know: skip rather than guess
    }
}

/// The (key, value) pairs of a block, undoing LevelDB's prefix sharing.
fn block_entries(b: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let Some(n) = b.len().checked_sub(4).and_then(|i| b.get(i..)) else {
        return out;
    };
    let restarts = u32::from_le_bytes(n.try_into().unwrap_or([0; 4])) as usize;
    let Some(end) = b.len().checked_sub(4 + 4 * restarts) else {
        return out;
    };
    let mut pos = 0;
    let mut key: Vec<u8> = Vec::new();
    while pos < end {
        let parsed = (|| {
            let shared = varint(b, &mut pos)? as usize;
            let unshared = varint(b, &mut pos)? as usize;
            let vlen = varint(b, &mut pos)? as usize;
            let delta = slice(b, &mut pos, unshared)?;
            let value = slice(b, &mut pos, vlen)?;
            key.truncate(shared.min(key.len()));
            key.extend_from_slice(delta);
            Some(value.to_vec())
        })();
        match parsed {
            Some(v) => out.push((key.clone(), v)),
            None => break,
        }
    }
    out
}

fn read_table(file: &[u8], map: &mut Entries) -> Option<()> {
    let footer = file.get(file.len().checked_sub(48)?..)?;
    if u64::from_le_bytes(footer[40..48].try_into().ok()?) != TABLE_MAGIC {
        return None;
    }
    let mut pos = 0;
    let _meta = (varint(footer, &mut pos)?, varint(footer, &mut pos)?);
    let index = (varint(footer, &mut pos)?, varint(footer, &mut pos)?);
    for (_, handle) in block_entries(&block(file, index)?) {
        let mut p = 0;
        let Some(h) = varint(&handle, &mut p).zip(varint(&handle, &mut p)) else {
            continue;
        };
        let Some(data) = block(file, h) else { continue };
        for (ikey, value) in block_entries(&data) {
            // Internal key = user key + 8 bytes of (sequence << 8 | type).
            let Some(split) = ikey.len().checked_sub(8) else { continue };
            let tag = u64::from_le_bytes(ikey[split..].try_into().ok()?);
            let (seq, kind) = (tag >> 8, tag & 0xff);
            put(map, &ikey[..split], seq, (kind == 1).then_some(value.as_slice()));
        }
    }
    Some(())
}

/// Every live key and value in the database folder. Reads only.
pub fn read_all(dir: &Path) -> HashMap<Vec<u8>, Vec<u8>> {
    let mut map = Entries::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let is_table = name.ends_with(".ldb") || name.ends_with(".sst");
        if !(is_table || name.ends_with(".log")) {
            continue;
        }
        let Ok(data) = std::fs::read(e.path()) else { continue };
        if is_table {
            let _ = read_table(&data, &mut map);
        } else {
            read_log(&data, &mut map);
        }
    }
    map.into_iter().filter_map(|(k, (_, v))| v.map(|v| (k, v))).collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn push_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    /// A write batch with (key, Some(value)) puts and (key, None) deletes.
    pub fn batch(seq: u64, ops: &[(&str, Option<&str>)]) -> Vec<u8> {
        let mut b = seq.to_le_bytes().to_vec();
        b.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for (k, v) in ops {
            b.push(u8::from(v.is_some()));
            push_varint(&mut b, k.len() as u64);
            b.extend_from_slice(k.as_bytes());
            if let Some(v) = v {
                push_varint(&mut b, v.len() as u64);
                b.extend_from_slice(v.as_bytes());
            }
        }
        b
    }

    /// Wraps batches as log records, splitting across 32 KB blocks like LevelDB.
    pub fn log(batches: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for b in batches {
            let mut rest: &[u8] = b;
            let mut first = true;
            loop {
                let left = BLOCK - out.len() % BLOCK;
                if left < 7 {
                    out.extend(std::iter::repeat_n(0, left));
                    continue;
                }
                let n = rest.len().min(left - 7);
                let last = n == rest.len();
                let kind = match (first, last) {
                    (true, true) => 1,
                    (true, false) => 2,
                    (false, false) => 3,
                    (false, true) => 4,
                };
                out.extend_from_slice(&[0, 0, 0, 0]); // checksum (not checked)
                out.extend_from_slice(&(n as u16).to_le_bytes());
                out.push(kind);
                out.extend_from_slice(&rest[..n]);
                rest = &rest[n..];
                first = false;
                if last {
                    break;
                }
            }
        }
        out
    }

    fn block_of(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut b = Vec::new();
        for (k, v) in entries {
            push_varint(&mut b, 0);
            push_varint(&mut b, k.len() as u64);
            push_varint(&mut b, v.len() as u64);
            b.extend_from_slice(k);
            b.extend_from_slice(v);
        }
        b.extend_from_slice(&0u32.to_le_bytes()); // one restart point at 0
        b.extend_from_slice(&1u32.to_le_bytes());
        b
    }

    /// A one-data-block table file; `snappy` compresses the data block.
    pub fn table(rows: &[(&str, u64, Option<&str>)], snappy: bool) -> Vec<u8> {
        let entries: Vec<_> = rows
            .iter()
            .map(|(k, seq, v)| {
                let mut ik = k.as_bytes().to_vec();
                ik.extend_from_slice(&((seq << 8) | u64::from(v.is_some())).to_le_bytes());
                (ik, v.unwrap_or("").as_bytes().to_vec())
            })
            .collect();
        let raw = block_of(&entries);
        let (body, kind) = if snappy {
            (snap::raw::Encoder::new().compress_vec(&raw).unwrap(), 1u8)
        } else {
            (raw, 0u8)
        };
        let mut file = body.clone();
        file.push(kind);
        file.extend_from_slice(&[0; 4]);
        let mut handle = Vec::new();
        push_varint(&mut handle, 0);
        push_varint(&mut handle, body.len() as u64);
        let index = block_of(&[(b"zzz".to_vec(), handle)]);
        let index_at = file.len() as u64;
        file.extend_from_slice(&index);
        file.extend_from_slice(&[0; 5]);
        let mut footer = Vec::new();
        push_varint(&mut footer, 0);
        push_varint(&mut footer, 0);
        push_varint(&mut footer, index_at);
        push_varint(&mut footer, index.len() as u64);
        footer.resize(40, 0);
        footer.extend_from_slice(&TABLE_MAGIC.to_le_bytes());
        file.extend_from_slice(&footer);
        file
    }

    fn get(m: &HashMap<Vec<u8>, Vec<u8>>, k: &str) -> Option<String> {
        m.get(k.as_bytes()).map(|v| String::from_utf8_lossy(v).into_owned())
    }

    #[test]
    fn reads_log_newest_value_wins_and_deletes_drop() {
        let dir = std::env::temp_dir().join(format!("ms-ldb-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = "x".repeat(70_000); // spans several 32 KB blocks
        let data = log(&[
            batch(
                1,
                &[("ORIGIN:https_a.com_0", Some("000")), ("ORIGIN:https_b.com_0", Some("001"))],
            ),
            batch(3, &[("ORIGIN:https_a.com_0", Some("002")), ("big", Some(&big))]),
            batch(5, &[("ORIGIN:https_b.com_0", None)]),
        ]);
        std::fs::write(dir.join("000003.log"), data).unwrap();
        let m = read_all(&dir);
        assert_eq!(get(&m, "ORIGIN:https_a.com_0").as_deref(), Some("002"));
        assert_eq!(get(&m, "ORIGIN:https_b.com_0"), None);
        assert_eq!(get(&m, "big").map(|s| s.len()), Some(70_000));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_tables_plain_and_snappy_and_merges_with_log() {
        let dir = std::env::temp_dir().join(format!("ms-ldb-tbl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("000005.ldb"),
            table(
                &[
                    ("ORIGIN:https_a.com_0", 10, Some("004")),
                    ("ORIGIN:https_c.com_0", 11, Some("005")),
                ],
                true,
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("000006.ldb"),
            table(&[("ORIGIN:https_d.com_0", 12, Some("006"))], false),
        )
        .unwrap();
        // The log is newer for c (deleted) and older for a.
        std::fs::write(
            dir.join("000007.log"),
            log(&[
                batch(2, &[("ORIGIN:https_a.com_0", Some("000"))]),
                batch(20, &[("ORIGIN:https_c.com_0", None)]),
            ]),
        )
        .unwrap();
        std::fs::write(dir.join("LOCK"), b"").unwrap(); // other files are ignored
        let m = read_all(&dir);
        assert_eq!(get(&m, "ORIGIN:https_a.com_0").as_deref(), Some("004"));
        assert_eq!(get(&m, "ORIGIN:https_c.com_0"), None);
        assert_eq!(get(&m, "ORIGIN:https_d.com_0").as_deref(), Some("006"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_files_give_nothing_instead_of_crashing() {
        let mut m = Entries::new();
        read_log(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 255, 255], &mut m);
        assert!(read_table(&[0; 10], &mut m).is_none());
        assert!(read_table(&[0xff; 100], &mut m).is_none());
        assert!(m.is_empty());
    }
}
