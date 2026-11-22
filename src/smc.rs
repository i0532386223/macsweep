//! Read-only look at the System Management Controller (SMC) keys that
//! charge limiters use, for `MacSweep --smc-probe`.
//!
//! Tools like AlDente stop charging by *writing* SMC keys from a root
//! helper. Reports say macOS 27 removed or locked those keys, so before
//! MacSweep builds anything like it, this checks what this Mac actually
//! has. It only reads; it never writes a key and needs no password.

/// Keys charge limiters have used, and what they were for.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const KEYS: &[(&str, &str)] = &[
    ("CH0B", "stop charging (older firmware)"),
    ("CH0C", "stop charging (older firmware)"),
    ("CHTE", "stop charging (newer firmware)"),
    ("CH0I", "run on battery while plugged in (discharge)"),
    ("CH0J", "run on battery while plugged in"),
    ("CHIE", "reported as the only one still answering on macOS 27"),
    ("CHWA", "Apple's fixed 80% limit (older macOS)"),
    ("CHLS", "Apple's charge limit setting"),
    ("ACLC", "charger status light"),
    ("BUIC", "battery percentage (for reference)"),
    ("CHNC", "why charging is paused (bit flags)"),
];

#[derive(Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Probe {
    /// (data type, size, bytes)
    Present(String, u32, Vec<u8>),
    Missing,
    Error(String),
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn report(results: &[(&str, &str, Probe)]) -> String {
    let mut out = String::from("SMC charging keys on this Mac (read only, nothing was changed)\n\n");
    for (key, what, r) in results {
        let state = match r {
            Probe::Present(t, size, _) if *size == 0 => format!("present but EMPTY (type {t}, 0 bytes)"),
            Probe::Present(t, size, bytes) => {
                let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                format!("present  type {t}, {size} bytes, value {}", hex.join(" "))
            }
            Probe::Missing => "not found".into(),
            Probe::Error(e) => format!("error: {e}"),
        };
        out.push_str(&format!("{key}  {state}\n      {what}\n"));
    }
    out
}

#[cfg(target_os = "macos")]
mod mac {
    use super::Probe;
    use std::ffi::{c_char, c_void};

    type IoObject = u32;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Vers {
        major: u8,
        minor: u8,
        build: u8,
        reserved: u8,
        release: u16,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct PLimit {
        version: u16,
        length: u16,
        cpu: u32,
        gpu: u32,
        mem: u32,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct KeyInfo {
        size: u32,
        data_type: u32,
        attributes: u8,
    }
    /// Apple's SMCKeyData_t, as used by every open-source SMC tool.
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct KeyData {
        key: u32,
        vers: Vers,
        p_limit: PLimit,
        info: KeyInfo,
        result: u8,
        status: u8,
        data8: u8,
        data32: u32,
        bytes: [u8; 32],
    }
    const _: () = assert!(std::mem::size_of::<KeyData>() == 80);

    const KERNEL_INDEX_SMC: u32 = 2;
    const CMD_READ_BYTES: u8 = 5;
    const CMD_READ_KEYINFO: u8 = 9;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> IoObject;
        fn IOServiceOpen(service: IoObject, task: u32, kind: u32, conn: *mut IoObject) -> i32;
        fn IOServiceClose(conn: IoObject) -> i32;
        fn IOObjectRelease(o: IoObject) -> i32;
        fn IOConnectCallStructMethod(
            conn: IoObject,
            selector: u32,
            input: *const c_void,
            input_size: usize,
            output: *mut c_void,
            output_size: *mut usize,
        ) -> i32;
    }
    unsafe extern "C" {
        static mach_task_self_: u32;
    }

    fn fourcc(s: &str) -> u32 {
        s.bytes().fold(0u32, |a, b| (a << 8) | u32::from(b))
    }
    fn fourcc_str(v: u32) -> String {
        v.to_be_bytes()
            .iter()
            .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' })
            .collect()
    }

    fn call(conn: IoObject, input: &KeyData) -> Result<KeyData, i32> {
        let mut out = KeyData::default();
        let mut size = std::mem::size_of::<KeyData>();
        let kr = unsafe {
            IOConnectCallStructMethod(
                conn,
                KERNEL_INDEX_SMC,
                (input as *const KeyData).cast(),
                std::mem::size_of::<KeyData>(),
                (&mut out as *mut KeyData).cast(),
                &mut size,
            )
        };
        if kr != 0 { Err(kr) } else { Ok(out) }
    }

    pub fn probe(keys: &[&str]) -> Result<Vec<Probe>, String> {
        let service = unsafe { IOServiceGetMatchingService(0, IOServiceMatching(c"AppleSMC".as_ptr())) };
        if service == 0 {
            return Err("AppleSMC not found".into());
        }
        let mut conn = 0;
        let kr = unsafe { IOServiceOpen(service, mach_task_self_, 0, &mut conn) };
        unsafe { IOObjectRelease(service) };
        if kr != 0 {
            return Err(format!("could not open AppleSMC (0x{kr:x})"));
        }
        let results = keys
            .iter()
            .map(|k| {
                let mut input = KeyData {
                    key: fourcc(k),
                    data8: CMD_READ_KEYINFO,
                    ..Default::default()
                };
                let info = match call(conn, &input) {
                    Err(kr) => return Probe::Error(format!("0x{:x}", kr as u32)),
                    Ok(o) if o.result == 132 => return Probe::Missing, // kSMCKeyNotFound
                    Ok(o) if o.result != 0 => return Probe::Error(format!("SMC result {}", o.result)),
                    Ok(o) => o.info,
                };
                let t = fourcc_str(info.data_type);
                if info.size == 0 {
                    return Probe::Present(t, 0, vec![]);
                }
                input.info = info;
                input.data8 = CMD_READ_BYTES;
                match call(conn, &input) {
                    Ok(o) if o.result == 0 => {
                        let n = (info.size as usize).min(32);
                        Probe::Present(t, info.size, o.bytes[..n].to_vec())
                    }
                    Ok(o) => Probe::Error(format!(
                        "type {t}, {} bytes, read failed (SMC result {})",
                        info.size, o.result
                    )),
                    Err(kr) => Probe::Error(format!("type {t}, {} bytes, read failed (0x{:x})", info.size, kr as u32)),
                }
            })
            .collect();
        unsafe { IOServiceClose(conn) };
        Ok(results)
    }
}

/// Reads every key in [`KEYS`]. Never writes.
pub fn probe_all() -> String {
    #[cfg(target_os = "macos")]
    {
        let names: Vec<&str> = KEYS.iter().map(|(k, _)| *k).collect();
        match mac::probe(&names) {
            Ok(r) => report(&KEYS.iter().zip(r).map(|((k, w), p)| (*k, *w, p)).collect::<Vec<_>>()),
            Err(e) => format!("Couldn't read the SMC: {e}\n"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        "The SMC probe only runs on a Mac.\n".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_says_what_each_key_is() {
        let r = report(&[
            (
                "CHTE",
                "stop charging (newer firmware)",
                Probe::Present("ui32".into(), 0, vec![]),
            ),
            ("CHIE", "x", Probe::Present("hex_".into(), 1, vec![0])),
            ("CH0B", "y", Probe::Missing),
        ]);
        assert!(r.contains("CHTE  present but EMPTY"));
        assert!(r.contains("CHIE  present  type hex_, 1 bytes, value 00"));
        assert!(r.contains("CH0B  not found"));
        assert!(r.contains("nothing was changed"));
    }
}
