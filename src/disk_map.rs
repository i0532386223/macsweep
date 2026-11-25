//! Account for allocated blocks. The walk only opens directories and never follows links.
use crate::{safe_fs, safety_log};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, openat, statat};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct Entry {
    pub name: OsString,
    pub bytes: u64,
    pub directory: bool,
}

#[derive(Default)]
pub struct Map {
    pub entries: Vec<Entry>,
    pub total: u64,
    pub skipped: usize,
    pub examined: usize,
}

struct Walk<'a> {
    progress: &'a AtomicUsize,
    cancel: &'a AtomicBool,
    hard_links: HashSet<(u64, u64)>,
    skipped: usize,
}

fn allocated(st: &Stat) -> u64 {
    (st.st_blocks.max(0) as u64).saturating_mul(512)
}

impl Walk<'_> {
    fn count(&mut self, st: &Stat) -> u64 {
        let file = FileType::from_raw_mode(st.st_mode);
        if file == FileType::Directory {
            return allocated(st);
        }
        if file != FileType::RegularFile && file != FileType::Symlink {
            return 0;
        }
        if st.st_nlink > 1 && !self.hard_links.insert((st.st_dev as u64, st.st_ino)) {
            return 0;
        }
        allocated(st)
    }

    fn children(&mut self, dir: &OwnedFd, device: u64, mut visit: impl FnMut(&mut Self, &OsStr, &Stat)) -> std::io::Result<()> {
        for entry in Dir::read_from(dir)? {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    self.skipped += 1;
                    continue;
                }
            };
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            self.progress.fetch_add(1, Ordering::Relaxed);
            match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) if st.st_dev as u64 == device => visit(self, name, &st),
                Ok(_) => {} // A mounted volume has a separate map.
                Err(_) => self.skipped += 1,
            }
        }
        Ok(())
    }

    fn size(&mut self, dir: &OwnedFd, name: &OsStr, st: &Stat, depth: usize) -> u64 {
        let mut size = self.count(st);
        if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
            return size;
        }
        // Limit open descriptors and stack use when a directory tree is exceptionally deep.
        if depth >= 128 {
            self.skipped += 1;
            return size;
        }
        let child = openat(
            dir,
            name,
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        );
        match child {
            Ok(child) if fstat(&child).is_ok_and(|opened| opened.st_dev == st.st_dev && opened.st_ino == st.st_ino) => {
                let read = self.children(&child, st.st_dev as u64, |walk, name, st| {
                    size = size.saturating_add(walk.size(&child, name, st, depth + 1));
                });
                if read.is_err() && !self.cancel.load(Ordering::Relaxed) {
                    self.skipped += 1;
                }
            }
            _ => self.skipped += 1,
        }
        size
    }
}

pub fn scan(root: &Path, progress: &AtomicUsize, cancel: &AtomicBool) -> std::io::Result<Map> {
    safety_log::event(false, "Disk Map: start a metadata scan on one volume");
    let dir = safe_fs::open_dir(root)?;
    let device = fstat(&dir)?.st_dev as u64;
    let mut map = Map::default();
    let mut walk = Walk {
        progress,
        cancel,
        hard_links: HashSet::new(),
        skipped: 0,
    };
    walk.children(&dir, device, |walk, name, st| {
        let bytes = walk.size(&dir, name, st, 0);
        map.total = map.total.saturating_add(bytes);
        map.entries.push(Entry {
            name: name.to_owned(),
            bytes,
            directory: FileType::from_raw_mode(st.st_mode) == FileType::Directory,
        });
    })?;
    if cancel.load(Ordering::Relaxed) {
        return Err(std::io::ErrorKind::Interrupted.into());
    }
    map.entries
        .sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    map.skipped = walk.skipped;
    map.examined = progress.load(Ordering::Relaxed);
    safety_log::event(
        false,
        &format!("Disk Map: scan complete; entries {}, skipped {}", map.examined, map.skipped),
    );
    Ok(map)
}

// Split each rectangle along its longer side, with the area of each tile proportional to its weight.
pub fn tiles(weights: &[u64], rect: eframe::egui::Rect) -> Vec<eframe::egui::Rect> {
    use eframe::egui::{Rect, pos2};
    fn split(weights: &[u64], rect: Rect, total: f64, out: &mut Vec<Rect>) {
        if weights.len() == 1 {
            out.push(rect);
            return;
        }
        let half = total / 2.0;
        let mut left = weights[0] as f64;
        let mut i = 1;
        while i < weights.len() - 1 {
            let next = left + weights[i] as f64;
            if (next - half).abs() > (left - half).abs() {
                break;
            }
            i += 1;
            left = next;
        }
        let fraction = (left / total) as f32;
        let (a, b) = if rect.width() >= rect.height() {
            let x = rect.left() + rect.width() * fraction;
            (
                Rect::from_min_max(rect.min, pos2(x, rect.bottom())),
                Rect::from_min_max(pos2(x, rect.top()), rect.max),
            )
        } else {
            let y = rect.top() + rect.height() * fraction;
            (
                Rect::from_min_max(rect.min, pos2(rect.right(), y)),
                Rect::from_min_max(pos2(rect.left(), y), rect.max),
            )
        };
        split(&weights[..i], a, left, out);
        // Sum independently to retain a small tail next to a very large file.
        split(&weights[i..], b, weights[i..].iter().map(|&n| n as f64).sum(), out);
    }
    let mut result = Vec::new();
    let weights: Vec<_> = weights.iter().copied().filter(|&v| v > 0).collect();
    if !weights.is_empty() && rect.is_positive() {
        split(&weights, rect, weights.iter().map(|&n| n as f64).sum(), &mut result);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Rect, pos2};
    use std::{fs, os::unix::fs::symlink};
    #[test]
    fn map_uses_disk_blocks_and_never_reads_through_links() {
        // Use a private folder. Keep the reference file outside the scanned directory.
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-map-{}", std::process::id()));
        fs::create_dir(&base).unwrap();
        let root = base.join("map");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(base.join("outside"), vec![9; 1 << 20]).unwrap();
        fs::write(root.join("nested/file"), vec![1; 65536]).unwrap();
        fs::hard_link(root.join("nested/file"), root.join("nested/link")).unwrap();
        symlink(&base, root.join("other")).unwrap();
        let sparse = fs::File::create(root.join("sparse")).unwrap();
        sparse.set_len(1 << 30).unwrap();
        let progress = AtomicUsize::new(0);
        let result = scan(&root, &progress, &AtomicBool::new(false)).unwrap();
        use std::os::unix::fs::MetadataExt;
        let blocks = |p: &str| fs::symlink_metadata(root.join(p)).unwrap().blocks() * 512;
        assert_eq!(
            result.total,
            blocks("nested") + blocks("nested/file") + blocks("other") + blocks("sparse")
        );
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.examined, 5);
        assert!(scan(&root.join("other"), &progress, &AtomicBool::new(false)).is_err());
        assert_eq!(
            scan(&root, &progress, &AtomicBool::new(true)).err().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn map_tiles_cover_the_canvas_in_proportion_without_overlap() {
        let rect = Rect::from_min_max(pos2(9., 7.), pos2(409., 307.));
        let weights = [70, 20, 8, 2, 0];
        let parts = tiles(&weights, rect);
        assert_eq!(parts.len(), 4);
        for (i, r) in parts.iter().enumerate() {
            assert!(rect.contains_rect(*r));
            assert!((r.area() / rect.area() - weights[i] as f32 / 100.).abs() < 0.00001);
            for other in &parts[..i] {
                assert!(!r.intersect(*other).is_positive());
            }
        }
        assert!(tiles(&[0], rect).is_empty());
        assert_eq!(tiles(&[4], rect), vec![rect]);
    }
}
