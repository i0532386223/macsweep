//! Mac Health: battery, internal SSD and system readings.
//!
//! Every value comes from Apple's own sources and is never estimated:
//! - battery: `system_profiler SPPowerDataType` (the same "Maximum Capacity"
//!   and "Condition" System Settings shows) plus the battery's raw readings
//!   from the I/O Registry (`ioreg`);
//! - SSD: the drive's NVMe SMART log, read through macOS's NVMe SMART
//!   interface (the one smartctl uses), plus macOS's own SMART verdict;
//! - system: sysctl and Foundation.
//!
//! Anything a source doesn't provide stays `None` and is shown as
//! "Not available".

use std::process::Command;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    /// Apple's "Maximum Capacity", exactly as System Settings shows it.
    pub max_capacity_pct: Option<u32>,
    /// Apple's "Condition" (Normal / Service Recommended …).
    pub condition: Option<String>,
    pub cycles: Option<u32>,
    /// Cycles the battery is designed for (from the battery itself).
    pub design_cycles: Option<u32>,
    /// Current charge, %.
    pub charge_pct: Option<u32>,
    pub charging: bool,
    pub plugged_in: bool,
    /// Raw readings, mAh.
    pub design_mah: Option<u32>,
    pub nominal_mah: Option<u32>,
    pub raw_max_mah: Option<u32>,
}

/// The NVMe "SMART / Health Information" log (NVMe spec, log page 0x02).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NvmeSmart {
    pub critical_warning: u8,
    pub temperature_c: i32,
    pub available_spare: u8,
    pub spare_threshold: u8,
    /// Share of the drive's rated endurance used (can pass 100).
    pub percentage_used: u8,
    pub data_read_bytes: u128,
    pub data_written_bytes: u128,
    pub power_cycles: u128,
    pub power_on_hours: u128,
    pub unsafe_shutdowns: u128,
    pub media_errors: u128,
    pub error_log_entries: u128,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Disk {
    pub model: Option<String>,
    pub capacity_bytes: Option<u64>,
    /// macOS's own verdict: "Verified" or "Failing".
    pub smart_status: Option<String>,
    pub smart: Option<NvmeSmart>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // only macOS reports it
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl Thermal {
    pub fn label(self) -> &'static str {
        match self {
            Thermal::Nominal => "Normal",
            Thermal::Fair => "Warm",
            Thermal::Serious => "Hot: performance reduced",
            Thermal::Critical => "Critical: performance heavily reduced",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct System {
    pub model_id: Option<String>,
    pub chip: Option<String>,
    pub memory_bytes: Option<u64>,
    pub macos: Option<String>,
    pub build: Option<String>,
    pub uptime_secs: Option<u64>,
    pub thermal: Option<Thermal>,
    pub ram_used: Option<u64>,
    pub pressure: Option<&'static str>,
    pub swap_used: Option<u64>,
    pub swap_total: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    /// None on Macs without a battery.
    pub battery: Option<Battery>,
    pub disk: Disk,
    pub system: System,
}

// ---------- parsers (pure, tested against real outputs) ----------

/// "Label: value" from system_profiler's text, first match.
fn field<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix(label)?.strip_prefix(':').map(str::trim))
}

/// Apple's battery summary from `system_profiler SPPowerDataType`.
pub fn parse_power(text: &str) -> (Option<u32>, Option<String>, Option<u32>) {
    let pct = field(text, "Maximum Capacity").and_then(|v| v.trim_end_matches('%').trim().parse().ok());
    let condition = field(text, "Condition").map(str::to_string);
    let cycles = field(text, "Cycle Count").and_then(|v| v.parse().ok());
    (pct, condition, cycles)
}

/// The battery's raw readings from `ioreg -rn AppleSmartBattery -a`.
/// None if the Mac has no battery.
pub fn parse_ioreg_battery(xml: &[u8]) -> Option<Battery> {
    let v = plist::Value::from_reader_xml(xml).ok()?;
    let d = v.as_array()?.first()?.as_dictionary()?;
    if d.get("BatteryInstalled").and_then(|b| b.as_boolean()) == Some(false) {
        return None;
    }
    let int = |dict: &plist::Dictionary, k: &str| dict.get(k).and_then(|v| v.as_signed_integer()).map(|n| n as u32);
    let bd = d.get("BatteryData").and_then(|v| v.as_dictionary());
    let flag = |k: &str| d.get(k).and_then(|v| v.as_boolean()).unwrap_or(false);
    Some(Battery {
        cycles: int(d, "CycleCount"),
        design_cycles: int(d, "DesignCycleCount9C"),
        charge_pct: int(d, "CurrentCapacity"),
        charging: flag("IsCharging"),
        plugged_in: flag("ExternalConnected"),
        design_mah: int(d, "DesignCapacity").or_else(|| bd.and_then(|b| int(b, "DesignCapacity"))),
        nominal_mah: int(d, "NominalChargeCapacity").or_else(|| bd.and_then(|b| int(b, "NominalChargeCapacity"))),
        raw_max_mah: int(d, "AppleRawMaxCapacity").or_else(|| bd.and_then(|b| int(b, "AppleRawMaxCapacity"))),
        ..Default::default()
    })
}

/// Model, capacity and macOS's SMART verdict for the first NVMe drive,
/// from `system_profiler SPNVMeDataType`.
pub fn parse_nvme_profiler(text: &str) -> (Option<String>, Option<u64>, Option<String>) {
    let model = field(text, "Model").map(str::to_string);
    let capacity = field(text, "Capacity").and_then(|v| {
        let inside = v.split_once('(')?.1.split_once(" bytes")?.0;
        inside.replace(',', "").parse().ok()
    });
    let status = field(text, "S.M.A.R.T. status").map(str::to_string);
    (model, capacity, status)
}

fn le(bytes: &[u8]) -> u128 {
    bytes.iter().rev().fold(0u128, |acc, &b| (acc << 8) | u128::from(b))
}

/// Decodes the 512-byte NVMe SMART log (layout from the NVMe specification).
pub fn parse_smart_log(log: &[u8; 512]) -> NvmeSmart {
    // A "data unit" is 1,000 blocks of 512 bytes.
    let units = |o: usize| le(&log[o..o + 16]) * 512_000;
    NvmeSmart {
        critical_warning: log[0],
        temperature_c: i32::from(u16::from_le_bytes([log[1], log[2]])) - 273,
        available_spare: log[3],
        spare_threshold: log[4],
        percentage_used: log[5],
        data_read_bytes: units(32),
        data_written_bytes: units(48),
        power_cycles: le(&log[112..128]),
        power_on_hours: le(&log[128..144]),
        unsafe_shutdowns: le(&log[144..160]),
        media_errors: le(&log[160..176]),
        error_log_entries: le(&log[176..192]),
    }
}

// ---------- formatting shared by the screen and the report ----------

/// Decimal units, rounded down to one decimal like smartctl, so the two
/// can be compared directly.
pub fn fmt_bytes(b: u128) -> String {
    let b = b as f64;
    for (unit, size) in [("TB", 1e12), ("GB", 1e9), ("MB", 1e6)] {
        if b >= size {
            return format!("{:.1} {unit}", (b / size * 10.0).floor() / 10.0);
        }
    }
    format!("{b:.0} bytes")
}

pub fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{m} min"),
        (0, _) => format!("{h} h {m} min"),
        _ => format!("{d} d {h} h"),
    }
}

/// Key–value lines for `MacSweep --health-report`, used by the validation
/// script to compare our values with Apple's tools.
pub fn report(h: &Health) -> String {
    let mut out = Vec::new();
    let mut put = |k: &str, v: Option<String>| out.push(format!("{k}={}", v.unwrap_or_else(|| "n/a".into())));
    match &h.battery {
        Some(b) => {
            put("battery.max_capacity_pct", b.max_capacity_pct.map(|v| v.to_string()));
            put("battery.condition", b.condition.clone());
            put("battery.cycles", b.cycles.map(|v| v.to_string()));
            put("battery.design_cycles", b.design_cycles.map(|v| v.to_string()));
            put("battery.design_mah", b.design_mah.map(|v| v.to_string()));
            put("battery.nominal_mah", b.nominal_mah.map(|v| v.to_string()));
            put("battery.raw_max_mah", b.raw_max_mah.map(|v| v.to_string()));
        }
        None => put("battery", Some("none".into())),
    }
    let d = &h.disk;
    put("disk.model", d.model.clone());
    put("disk.smart_status", d.smart_status.clone());
    if let Some(s) = &d.smart {
        put("disk.percentage_used", Some(s.percentage_used.to_string()));
        put("disk.available_spare", Some(s.available_spare.to_string()));
        put("disk.temperature_c", Some(s.temperature_c.to_string()));
        put("disk.data_written_units", Some((s.data_written_bytes / 512_000).to_string()));
        put("disk.data_read_units", Some((s.data_read_bytes / 512_000).to_string()));
        put("disk.power_cycles", Some(s.power_cycles.to_string()));
        put("disk.power_on_hours", Some(s.power_on_hours.to_string()));
        put("disk.unsafe_shutdowns", Some(s.unsafe_shutdowns.to_string()));
        put("disk.media_errors", Some(s.media_errors.to_string()));
        put("disk.error_log_entries", Some(s.error_log_entries.to_string()));
    } else {
        put("disk.smart_log", None);
    }
    let s = &h.system;
    put("system.model_id", s.model_id.clone());
    put("system.chip", s.chip.clone());
    put("system.memory_bytes", s.memory_bytes.map(|v| v.to_string()));
    put("system.macos", s.macos.clone());
    put("system.thermal", s.thermal.map(|t| t.label().to_string()));
    out.join("\n")
}

// ---------- collecting (macOS) ----------

fn run(cmd: &str, args: &[&str]) -> Option<Vec<u8>> {
    let o = Command::new(cmd).args(args).output().ok()?;
    o.status.success().then_some(o.stdout)
}

/// Reads everything. Takes about a second (system_profiler), so call it off
/// the UI thread.
/// Just the fast-changing system readings (memory, pressure, swap, thermal
/// state): a few sysctl calls, cheap enough to repeat every few seconds.
pub fn system_now() -> System {
    mac::system()
}

pub fn collect() -> Health {
    // One system_profiler run for both reports: it's the slow part (~1 s each).
    let profile = run("/usr/sbin/system_profiler", &["SPPowerDataType", "SPNVMeDataType"])
        .map(|t| String::from_utf8_lossy(&t).into_owned())
        .unwrap_or_default();
    let battery = run("/usr/sbin/ioreg", &["-rn", "AppleSmartBattery", "-a"])
        .and_then(|x| parse_ioreg_battery(&x))
        .map(|mut b| {
            if let Some(power) = section(&profile, "Power:") {
                let (pct, cond, cycles) = parse_power(power);
                b.max_capacity_pct = pct;
                b.condition = cond;
                b.cycles = cycles.or(b.cycles);
            }
            b
        });
    let mut disk = Disk::default();
    if let Some(nvme) = section(&profile, "NVMExpress:") {
        let (model, capacity_bytes, smart_status) = parse_nvme_profiler(nvme);
        disk = Disk {
            model,
            capacity_bytes,
            smart_status,
            smart: None,
        };
    }
    disk.smart = mac::read_smart_log("disk0").map(|log| parse_smart_log(&log));
    Health {
        battery,
        disk,
        system: mac::system(),
    }
}

/// One report ("Power:", "NVMExpress:") out of a combined system_profiler
/// output: from its heading to the next unindented heading.
pub fn section<'a>(text: &'a str, heading: &str) -> Option<&'a str> {
    let start = text.lines().position(|l| l == heading)?;
    let mut offset = 0;
    let mut begin = None;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        if n == start {
            begin = Some(offset);
        } else if begin.is_some() && !line.starts_with(' ') && line.trim_end().ends_with(':') {
            return Some(&text[begin?..offset]);
        }
        offset += line.len();
    }
    begin.map(|b| &text[b..])
}

#[path = "../monitor/src/stats.rs"]
#[allow(dead_code)]
mod stats;

#[cfg(target_os = "macos")]
pub use stats::footprint;

/// One of the current user's processes, found by executable name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Proc {
    pub pid: i32,
    /// When it started, Unix seconds.
    pub started: u64,
    /// CPU time used so far, nanoseconds.
    pub cpu_ns: u64,
}

/// Finds the current user's process running an executable called `name`,
/// straight from the kernel (no `pgrep`/`ps`).
pub fn find_process(name: &str) -> Option<Proc> {
    mac::find_process(name)
}

#[cfg(not(target_os = "macos"))]
mod mac {
    pub fn find_process(_name: &str) -> Option<super::Proc> {
        None
    }
    pub fn read_smart_log(_bsd: &str) -> Option<[u8; 512]> {
        None
    }
    pub fn system() -> super::System {
        super::System::default()
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{System, Thermal, stats};
    use std::ffi::{CString, c_char, c_void};

    fn sysctl_string(name: &str) -> Option<String> {
        let c = CString::new(name).ok()?;
        let mut len = 0usize;
        unsafe { libc::sysctlbyname(c.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) };
        let mut buf = vec![0u8; len];
        let ok = unsafe { libc::sysctlbyname(c.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
        (ok == 0).then(|| String::from_utf8_lossy(&buf[..len]).trim_end_matches('\0').to_string())
    }

    fn sysctl_raw<T: Copy>(name: &str) -> Option<T> {
        let c = CString::new(name).ok()?;
        let mut v = std::mem::MaybeUninit::<T>::zeroed();
        let mut len = std::mem::size_of::<T>();
        let ok = unsafe { libc::sysctlbyname(c.as_ptr(), v.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
        (ok == 0 && len == std::mem::size_of::<T>()).then(|| unsafe { v.assume_init() })
    }

    /// `struct xsw_usage` from <sys/sysctl.h>.
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct SwapUsage {
        total: u64,
        avail: u64,
        used: u64,
        pagesize: u32,
        encrypted: u32,
    }

    pub fn system() -> System {
        let sampler = stats::Sampler::new();
        let boot: Option<libc::timeval> = sysctl_raw("kern.boottime");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .ok();
        let swap: Option<SwapUsage> = sysctl_raw("vm.swapusage");
        let thermal = match objc2_foundation::NSProcessInfo::processInfo().thermalState().0 {
            0 => Some(Thermal::Nominal),
            1 => Some(Thermal::Fair),
            2 => Some(Thermal::Serious),
            3 => Some(Thermal::Critical),
            _ => None,
        };
        System {
            model_id: sysctl_string("hw.model"),
            chip: sysctl_string("machdep.cpu.brand_string"),
            memory_bytes: sysctl_raw::<u64>("hw.memsize"),
            macos: sysctl_string("kern.osproductversion"),
            build: sysctl_string("kern.osversion"),
            uptime_secs: boot.zip(now).map(|(b, n)| n.saturating_sub(b.tv_sec as u64)),
            thermal,
            ram_used: Some(sampler.ram_used()).filter(|&v| v > 0),
            pressure: Some(match stats::pressure() {
                stats::Pressure::Normal => "Normal",
                stats::Pressure::Warning => "High",
                stats::Pressure::Critical => "Critical",
            }),
            swap_used: swap.map(|s| s.used),
            swap_total: swap.map(|s| s.total),
        }
    }

    pub fn find_process(name: &str) -> Option<super::Proc> {
        let uid = unsafe { libc::getuid() };
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return None;
        }
        let mut pids = vec![0i32; n as usize + 64];
        let bytes = (pids.len() * std::mem::size_of::<i32>()) as i32;
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        pids.truncate(n.max(0) as usize);
        let bsd_size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        for pid in pids.into_iter().filter(|&p| p > 0) {
            let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut bsd as *mut libc::proc_bsdinfo).cast(),
                    bsd_size,
                )
            };
            if got != bsd_size || bsd.pbi_uid != uid {
                continue;
            }
            let mut path = vec![0u8; 4096];
            let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
            if len <= 0 {
                continue;
            }
            let exe = std::path::Path::new(std::str::from_utf8(&path[..len as usize]).unwrap_or(""));
            if exe.file_name().is_some_and(|f| f == name) {
                return Some(super::Proc {
                    pid,
                    started: bsd.pbi_start_tvsec,
                    cpu_ns: cpu_ns(pid).unwrap_or(0),
                });
            }
        }
        None
    }

    /// `mach_timebase_info_data_t` from <mach/mach_time.h> (declared here:
    /// the libc crate's copy is deprecated).
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }

    /// Total CPU time of a process in nanoseconds.
    fn cpu_ns(pid: i32) -> Option<u64> {
        let mut t: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut t as *mut libc::proc_taskinfo).cast(),
                size,
            )
        };
        if got != size {
            return None;
        }
        // Reported in Mach time units (not nanoseconds on Apple silicon).
        let mut tb = Timebase { numer: 0, denom: 0 };
        unsafe { mach_timebase_info(&mut tb) };
        let ticks = u128::from(t.pti_total_user + t.pti_total_system);
        (tb.denom != 0).then(|| (ticks * u128::from(tb.numer) / u128::from(tb.denom)) as u64)
    }

    // --- NVMe SMART through IOKit (the interface smartctl uses on macOS) ---

    type CFTypeRef = *const c_void;
    type IoObject = u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CFUUIDBytes([u8; 16]);

    #[link(name = "IOKit", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        fn IOBSDNameMatching(main_port: u32, options: u32, bsd_name: *const c_char) -> CFTypeRef;
        fn IOServiceGetMatchingService(main_port: u32, matching: CFTypeRef) -> IoObject;
        fn IORegistryEntryGetParentEntry(entry: IoObject, plane: *const c_char, parent: *mut IoObject) -> i32;
        fn IORegistryEntryCreateCFProperty(entry: IoObject, key: CFTypeRef, alloc: CFTypeRef, options: u32) -> CFTypeRef;
        fn IOObjectRelease(o: IoObject) -> i32;
        fn IOCreatePlugInInterfaceForService(
            service: IoObject,
            plugin_type: CFTypeRef,
            interface_type: CFTypeRef,
            the_interface: *mut *mut *mut PlugIn,
            score: *mut i32,
        ) -> i32;
        fn IODestroyPlugInInterface(interface: *mut *mut PlugIn) -> i32;
        fn CFUUIDGetConstantUUIDWithBytes(
            alloc: CFTypeRef,
            b0: u8,
            b1: u8,
            b2: u8,
            b3: u8,
            b4: u8,
            b5: u8,
            b6: u8,
            b7: u8,
            b8: u8,
            b9: u8,
            b10: u8,
            b11: u8,
            b12: u8,
            b13: u8,
            b14: u8,
            b15: u8,
        ) -> CFTypeRef;
        fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
        fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
        fn CFRelease(cf: CFTypeRef);
    }

    /// IOCFPlugInInterface (IOKit/IOCFPlugIn.h).
    #[repr(C)]
    struct PlugIn {
        _reserved: *mut c_void,
        query_interface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32,
        _add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
        _release: unsafe extern "C" fn(*mut c_void) -> u32,
    }

    /// IONVMeSMARTInterface (as documented by smartmontools, os_darwin.h).
    #[repr(C)]
    struct SmartInterface {
        _reserved: *mut c_void,
        _query_interface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32,
        _add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
        release: unsafe extern "C" fn(*mut c_void) -> u32,
        _version: u16,
        _revision: u16,
        smart_read_data: unsafe extern "C" fn(*mut c_void, *mut [u8; 512]) -> i32,
    }

    fn uuid(b: [u8; 16]) -> CFTypeRef {
        unsafe {
            CFUUIDGetConstantUUIDWithBytes(
                std::ptr::null(),
                b[0],
                b[1],
                b[2],
                b[3],
                b[4],
                b[5],
                b[6],
                b[7],
                b[8],
                b[9],
                b[10],
                b[11],
                b[12],
                b[13],
                b[14],
                b[15],
            )
        }
    }

    const NVME_SMART_USER_CLIENT: [u8; 16] = [
        0xAA, 0x0F, 0xA6, 0xF9, 0xC2, 0xD6, 0x45, 0x7F, 0xB1, 0x0B, 0x59, 0xA1, 0x32, 0x53, 0x29, 0x2F,
    ];
    const NVME_SMART_INTERFACE: [u8; 16] = [
        0xCC, 0xD1, 0xDB, 0x19, 0xFD, 0x9A, 0x4D, 0xAF, 0xBF, 0x95, 0x12, 0x45, 0x4B, 0x23, 0x0A, 0xB6,
    ];
    const CF_PLUGIN_INTERFACE: [u8; 16] = [
        0xC2, 0x44, 0xE8, 0x58, 0x10, 0x9C, 0x11, 0xD4, 0x91, 0xD4, 0x00, 0x50, 0xE4, 0xC6, 0x42, 0x6F,
    ];

    fn has_true(entry: IoObject, key: &str) -> bool {
        let Ok(ckey) = CString::new(key) else { return false };
        unsafe {
            let k = CFStringCreateWithCString(std::ptr::null(), ckey.as_ptr(), 0x0800_0100);
            let v = IORegistryEntryCreateCFProperty(entry, k, std::ptr::null(), 0);
            CFRelease(k);
            if v.is_null() {
                return false;
            }
            let yes = CFEqual(v, kCFBooleanTrue) != 0;
            CFRelease(v);
            yes
        }
    }

    /// The raw 512-byte SMART log of the NVMe drive behind `bsd` (e.g. "disk0").
    pub fn read_smart_log(bsd: &str) -> Option<[u8; 512]> {
        let name = CString::new(bsd).ok()?;
        let plane = CString::new("IOService").ok()?;
        unsafe {
            let mut entry = IOServiceGetMatchingService(0, IOBSDNameMatching(0, 0, name.as_ptr()));
            // Walk up to the controller that offers NVMe SMART.
            while entry != 0 && !has_true(entry, "NVMe SMART Capable") {
                let mut parent = 0;
                let r = IORegistryEntryGetParentEntry(entry, plane.as_ptr(), &mut parent);
                IOObjectRelease(entry);
                entry = if r == 0 { parent } else { 0 };
            }
            if entry == 0 {
                return None;
            }
            let mut plugin: *mut *mut PlugIn = std::ptr::null_mut();
            let mut score = 0;
            let r = IOCreatePlugInInterfaceForService(
                entry,
                uuid(NVME_SMART_USER_CLIENT),
                uuid(CF_PLUGIN_INTERFACE),
                &mut plugin,
                &mut score,
            );
            IOObjectRelease(entry);
            if r != 0 || plugin.is_null() {
                return None;
            }
            let mut smart: *mut *mut SmartInterface = std::ptr::null_mut();
            let q = ((**plugin).query_interface)(
                plugin.cast(),
                CFUUIDBytes(NVME_SMART_INTERFACE),
                (&mut smart as *mut *mut *mut SmartInterface).cast(),
            );
            let mut log = [0u8; 512];
            let ok = q == 0 && !smart.is_null() && ((**smart).smart_read_data)(smart.cast(), &mut log) == 0;
            if !smart.is_null() {
                ((**smart).release)(smart.cast());
            }
            IODestroyPlugInInterface(plugin);
            ok.then_some(log)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/health/");
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{DIR}{name}")).unwrap()
    }

    #[test]
    fn battery_values_match_apple_on_a_real_macbook_air() {
        let (pct, cond, cycles) = parse_power(&String::from_utf8(fixture("system_profiler-power.txt")).unwrap());
        // What System Settings → Battery Health showed on that Mac.
        assert_eq!(pct, Some(90));
        assert_eq!(cond.as_deref(), Some("Normal"));
        assert_eq!(cycles, Some(290));

        let b = parse_ioreg_battery(&fixture("ioreg-battery.plist")).unwrap();
        assert_eq!(b.cycles, Some(290));
        assert_eq!(b.design_cycles, Some(1000));
        assert_eq!(b.design_mah, Some(4382));
        assert_eq!(b.nominal_mah, Some(3911));
        assert_eq!(b.raw_max_mah, Some(3781));
        assert_eq!(b.charge_pct, Some(90));
        assert!(!b.charging && !b.plugged_in);
    }

    #[test]
    fn maximum_capacity_is_never_computed_from_raw_numbers() {
        // Neither ratio equals Apple's 90%: computing it would mislead.
        let b = parse_ioreg_battery(&fixture("ioreg-battery.plist")).unwrap();
        let nominal = b.nominal_mah.unwrap() * 100 / b.design_mah.unwrap();
        let raw = b.raw_max_mah.unwrap() * 100 / b.design_mah.unwrap();
        assert_eq!((nominal, raw), (89, 86));
        assert_eq!(b.max_capacity_pct, None, "only system_profiler supplies it");
    }

    #[test]
    fn combined_system_profiler_output_is_split_correctly() {
        let power = String::from_utf8(fixture("system_profiler-power.txt")).unwrap();
        let nvme = String::from_utf8(fixture("system_profiler-storage.txt")).unwrap();
        for combined in [format!("{power}\n{nvme}"), format!("{nvme}\n{power}")] {
            let p = section(&combined, "Power:").unwrap();
            let n = section(&combined, "NVMExpress:").unwrap();
            assert_eq!(parse_power(p), (Some(90), Some("Normal".into()), Some(290)));
            let (model, cap, status) = parse_nvme_profiler(n);
            assert_eq!(model.as_deref(), Some("APPLE SSD AP0256Q"));
            assert_eq!(cap, Some(251_000_193_024));
            assert_eq!(status.as_deref(), Some("Verified"));
            assert!(!p.contains("NVMExpress:") && !n.contains("Power:"));
        }
        assert_eq!(section("Other:\n  x: 1\n", "Power:"), None);
    }

    #[test]
    fn a_mac_without_a_battery_has_none() {
        let empty = br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><array/></plist>"#;
        assert_eq!(parse_ioreg_battery(empty), None);
    }

    #[test]
    fn disk_summary_from_system_profiler() {
        let (model, cap, status) = parse_nvme_profiler(&String::from_utf8(fixture("system_profiler-storage.txt")).unwrap());
        assert_eq!(model.as_deref(), Some("APPLE SSD AP0256Q"));
        assert_eq!(cap, Some(251_000_193_024));
        assert_eq!(status.as_deref(), Some("Verified"));
    }

    /// Builds a SMART log with the values smartctl printed on the same Mac.
    fn smartctl_log() -> [u8; 512] {
        let mut l = [0u8; 512];
        let put = |l: &mut [u8; 512], o: usize, v: u128| l[o..o + 16].copy_from_slice(&v.to_le_bytes());
        l[0] = 0;
        l[1..3].copy_from_slice(&(36u16 + 273).to_le_bytes());
        l[3] = 100;
        l[4] = 99;
        l[5] = 2;
        put(&mut l, 32, 153_097_065);
        put(&mut l, 48, 60_501_322);
        put(&mut l, 112, 1_631);
        put(&mut l, 128, 1_272);
        put(&mut l, 144, 5);
        put(&mut l, 160, 0);
        put(&mut l, 176, 0);
        l
    }

    #[test]
    fn smart_log_decodes_like_smartctl() {
        let s = parse_smart_log(&smartctl_log());
        assert_eq!(s.temperature_c, 36);
        assert_eq!((s.available_spare, s.spare_threshold, s.percentage_used), (100, 99, 2));
        assert_eq!((s.power_cycles, s.power_on_hours, s.unsafe_shutdowns), (1_631, 1_272, 5));
        assert_eq!((s.media_errors, s.error_log_entries), (0, 0));
        // smartctl: "Data Units Written: 60,501,322 [30.9 TB]", "Read … [78.3 TB]"
        assert_eq!(fmt_bytes(s.data_written_bytes), "30.9 TB");
        assert_eq!(fmt_bytes(s.data_read_bytes), "78.3 TB");
        let smartctl = std::str::from_utf8(&fixture("smartctl.txt")).unwrap().to_string();
        assert!(smartctl.contains("Data Units Written:                 60,501,322"));
    }

    #[test]
    fn report_lists_every_value_or_says_not_available() {
        let h = Health {
            battery: None,
            ..Default::default()
        };
        let r = report(&h);
        assert!(r.contains("battery=none"));
        assert!(r.contains("disk.smart_log=n/a"));
        assert!(!r.contains("disk.percentage_used"));
    }
}
