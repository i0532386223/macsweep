//! Menu Bar Monitor: turns the tiny helper app on or off and sets what it shows.
//!
//! The helper ("MacSweep Monitor.app", inside MacSweep.app) runs on its own as
//! a login item, so MacSweep itself never has to stay open. It is off until
//! the user turns it on here, and turning it off removes it completely.
use crate::fsutil;
use crate::job::{self, Job};
use crate::theme::{self, Palette, icon};
use eframe::egui::{self, Color32, Ui};
use std::path::PathBuf;
use std::process::Command;

use crate::shared::RELOAD;
pub use crate::shared::{CONF, Config, LABEL};

/// What the menu bar will look like (with example numbers).
trait Preview {
    fn preview(&self) -> String;
}

impl Preview for Config {
    /// What the menu bar will look like (with example numbers).
    fn preview(&self) -> String {
        let mut v = Vec::new();
        if self.cpu {
            v.push("CPU 12%");
        }
        if self.ram {
            v.push("RAM 61%");
        }
        if self.disk {
            v.push("120 GB");
        }
        if v.is_empty() { "MacSweep".into() } else { v.join("  ") }
    }
}

fn conf_path() -> PathBuf {
    fsutil::home().join(CONF)
}

fn plist_path() -> PathBuf {
    fsutil::home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

/// The helper's executable inside this copy of MacSweep.app.
fn helper_exe() -> Option<PathBuf> {
    let contents = std::env::current_exe().ok()?.parent()?.parent()?.to_path_buf();
    let exe = contents.join("Library/LoginItems/MacSweep Monitor.app/Contents/MacOS/macsweep-monitor");
    exe.exists().then_some(exe)
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("launchctl")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn is_on() -> bool {
    plist_path().exists()
}

/// The login item that starts the helper at login (and now).
fn login_item(exe: &std::path::Path) -> plist::Value {
    let mut d = plist::Dictionary::new();
    d.insert("Label".into(), LABEL.into());
    d.insert(
        "ProgramArguments".into(),
        plist::Value::Array(vec![exe.to_string_lossy().into_owned().into()]),
    );
    d.insert("RunAtLoad".into(), true.into());
    // Start again only if it crashed, never after "Turn Off".
    let mut keep = plist::Dictionary::new();
    keep.insert("SuccessfulExit".into(), false.into());
    d.insert("KeepAlive".into(), plist::Value::Dictionary(keep));
    d.insert("LimitLoadToSessionType".into(), "Aqua".into());
    plist::Value::Dictionary(d)
}

/// A login item must point at a copy of MacSweep that will still be there
/// after a restart. Apps opened from Downloads are run by macOS from a hidden
/// temporary folder ("App Translocation"), and disk images get unmounted.
fn install_problem(exe: &std::path::Path) -> Option<&'static str> {
    let p = exe.to_string_lossy();
    if p.contains("/AppTranslocation/") || p.starts_with("/Volumes/") || p.starts_with("/private/var/folders/") {
        Some("Move MacSweep to your Applications folder and open it from there, then turn the monitor on.")
    } else {
        None
    }
}

fn turn_on() -> Result<(), String> {
    let exe = helper_exe().ok_or("The monitor isn't included in this copy of MacSweep. Install the latest release.")?;
    if let Some(problem) = install_problem(&exe) {
        return Err(problem.into());
    }
    let path = plist_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    login_item(&exe)
        .to_file_xml(&path)
        .map_err(|e| format!("Couldn't save the login item: {e}"))?;
    let domain = format!("gui/{}", uid());
    let _ = launchctl(&["bootout", &format!("{domain}/{LABEL}")]);
    if launchctl(&["bootstrap", &domain, &path.to_string_lossy()]) {
        remember_version();
        Ok(())
    } else {
        let _ = std::fs::remove_file(&path);
        Err("macOS didn't start the monitor. Try again, or check System Settings › General › Login Items.".into())
    }
}

/// Records which MacSweep version started the monitor.
fn version_path() -> PathBuf {
    conf_path().with_file_name("monitor.version")
}

fn remember_version() {
    let _ = std::fs::write(version_path(), env!("CARGO_PKG_VERSION"));
}

/// The running monitor is older than the helper inside this MacSweep: the
/// app was updated (new version), or rebuilt/re-copied after it started.
fn running_is_outdated(exe: &std::path::Path) -> bool {
    let recorded = std::fs::read_to_string(version_path()).unwrap_or_default();
    if recorded.trim() != env!("CARGO_PKG_VERSION") {
        return true;
    }
    let changed = std::fs::metadata(exe)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    match (crate::health::find_process("macsweep-monitor"), changed) {
        (Some(p), Some(c)) => c > p.started,
        _ => false,
    }
}

fn turn_off() {
    let _ = launchctl(&["bootout", &format!("gui/{}/{LABEL}", uid())]);
    let _ = std::fs::remove_file(plist_path());
}

/// Tells a running monitor to re-read its settings. Instant, never blocks:
/// a Darwin notification, which the monitor receives at no cost while idle.
fn notify_monitor() {
    let _name = RELOAD;
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn notify_post(name: *const std::ffi::c_char) -> u32;
        }
        let name = std::ffi::CString::new(RELOAD).unwrap();
        unsafe { notify_post(name.as_ptr()) };
    }
}

/// If MacSweep was moved or updated, point the login item at this copy and
/// restart the monitor, so it never keeps running an old version until logout.
pub fn repair_on_launch() {
    if !is_on() {
        return;
    }
    let Some(exe) = helper_exe() else { return };
    if install_problem(&exe).is_some() {
        return; // never point the login item at a temporary copy
    }
    let current = plist::Value::from_file(plist_path()).ok().and_then(|v| {
        v.as_dictionary()?
            .get("ProgramArguments")?
            .as_array()?
            .first()?
            .as_string()
            .map(PathBuf::from)
    });
    if current.as_deref() != Some(exe.as_path()) {
        let _ = turn_on();
    } else if running_is_outdated(&exe) {
        // Already in the background: restarting it here never blocks the UI.
        let _ = launchctl(&["kickstart", "-k", &format!("gui/{}/{LABEL}", uid())]);
        remember_version();
    }
}

/// What a poll of the helper finds, measured from outside without starting
/// any programs.
#[derive(Clone, Copy)]
struct Probe {
    /// Its login item exists (it is switched on).
    on: bool,
    running: Option<crate::health::Proc>,
    /// Memory as Activity Monitor counts it.
    memory: Option<u64>,
    at: std::time::Instant,
}

fn probe() -> Probe {
    let running = crate::health::find_process("macsweep-monitor");
    Probe {
        on: is_on(),
        memory: running.and_then(|p| footprint(p.pid)),
        running,
        at: std::time::Instant::now(),
    }
}

/// Activity Monitor's memory figure for a process (see stats.rs).
#[cfg(target_os = "macos")]
fn footprint(pid: i32) -> Option<u64> {
    crate::health::footprint(pid)
}

#[cfg(not(target_os = "macos"))]
fn footprint(_pid: i32) -> Option<u64> {
    None
}

/// CPU % between two probes of the same process.
fn cpu_between(a: &Probe, b: &Probe) -> Option<f32> {
    let (pa, pb) = (a.running?, b.running?);
    let wall = b.at.duration_since(a.at).as_nanos() as f64;
    (pa.pid == pb.pid && wall > 0.0).then(|| (pb.cpu_ns.saturating_sub(pa.cpu_ns) as f64 / wall * 100.0) as f32)
}

pub struct Monitor {
    config: Config,
    on: bool,
    /// Turning on/off talks to launchd, which can take a moment: off the UI thread.
    switching: Option<Job<Result<bool, String>>>,
    /// The last two polls of the helper (CPU % comes from their difference).
    last: Option<Probe>,
    prev: Option<Probe>,
    probe_job: Option<Job<Probe>>,
    probe_at: f64,
    /// When it was last turned on here (to allow a few seconds to start).
    on_since: f64,
    error: Option<String>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            config: Config::parse(&std::fs::read_to_string(conf_path()).unwrap_or_default()),
            on: is_on(),
            switching: None,
            last: None,
            prev: None,
            probe_job: None,
            probe_at: -100.0,
            on_since: -100.0,
            error: None,
        }
    }
}

impl Monitor {
    fn save(&mut self) {
        let p = conf_path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, self.config.render());
        if self.on {
            notify_monitor();
        }
    }

    /// The Menu Bar Monitor card inside Mac Health.
    pub fn card(&mut self, ui: &mut Ui, p: Palette) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        // Look at the helper every few seconds, only while this screen is open.
        // Reads kernel data directly: no programs are started.
        if self.probe_job.is_none() && now - self.probe_at > 3.0 {
            self.probe_at = now;
            self.probe_job = Some(Job::spawn(&ctx, |_, _| probe()));
        }
        if let Some(pr) = job::poll(&mut self.probe_job, &ctx) {
            // The helper's own menu can turn it off while MacSweep is open.
            if self.switching.is_none() {
                self.on = pr.on;
            }
            self.prev = self.last.replace(pr);
        }
        let running = self.last.is_some_and(|p| p.running.is_some());
        let usage = match (&self.prev, &self.last) {
            (Some(a), Some(b)) => b.memory.zip(cpu_between(a, b)),
            _ => None,
        };
        if let Some(r) = job::poll(&mut self.switching, &ctx) {
            match r {
                Ok(on) => {
                    self.on = on;
                    self.on_since = now;
                    self.last = None;
                    self.prev = None;
                    self.probe_at = now - 2.0; // look soon after it starts
                }
                Err(e) => {
                    self.on = is_on();
                    self.error = Some(e);
                }
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(3));

        theme::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::icon_tile(ui, icon::GAUGE, p.accent, 34.0);
                ui.vertical(|ui| {
                    ui.label(theme::semi("Menu bar monitor", 15.0));
                    let starting = now - self.on_since < 8.0 || self.last.is_none();
                    let (sub, warn) = match (&self.error, self.on, running, usage) {
                        (Some(e), ..) => (e.clone(), true),
                        (None, false, ..) => ("Off · nothing runs in the background".to_string(), false),
                        (None, true, true, Some((mem, cpu))) => (
                            format!(
                                "On · the monitor itself uses {:.1} MB of memory and {cpu:.1}% CPU",
                                mem as f64 / 1e6
                            ),
                            false,
                        ),
                        (None, true, true, None) => ("On · measuring…".to_string(), false),
                        (None, true, false, _) if starting => ("On · starting…".to_string(), false),
                        // Switched on, but not running: usually blocked in Login Items.
                        (None, true, false, _) => (
                            "On, but not running. macOS may be blocking it: in System Settings › General › Login Items, \
                             allow macsweep-monitor in the background, then turn it off and on here."
                                .to_string(),
                            true,
                        ),
                    };
                    if warn {
                        ui.label(theme::text(sub, 12.5).color(theme::WARN));
                    } else {
                        ui.label(theme::dim(sub, 12.5));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let busy = self.switching.is_some();
                    let (label, fill) = match (busy, self.on) {
                        (true, true) => ("Turning off…", Color32::from_white_alpha(35)),
                        (true, false) => ("Turning on…", Color32::from_white_alpha(35)),
                        (false, true) => ("Turn Off", Color32::from_white_alpha(35)),
                        (false, false) => ("Turn On", p.accent.gamma_multiply(0.8)),
                    };
                    if theme::pill(ui, label, fill).clicked() && !busy {
                        self.error = None;
                        let turning_on = !self.on;
                        if turning_on {
                            self.save();
                        }
                        self.switching = Some(Job::spawn(&ctx, move |_, _| {
                            if turning_on {
                                turn_on().map(|_| true)
                            } else {
                                turn_off();
                                Ok(false)
                            }
                        }));
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                // A small mock of the menu bar, so the choice is easy to judge.
                egui::Frame::new()
                    .fill(Color32::from_black_alpha(90))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(12, 5))
                    .show(ui, |ui| {
                        ui.label(theme::text(self.config.preview(), 13.0).monospace());
                    });
                ui.add_space(8.0);
                let mut changed = false;
                changed |= ui.checkbox(&mut self.config.cpu, "CPU").changed();
                changed |= ui.checkbox(&mut self.config.ram, "Memory").changed();
                changed |= ui.checkbox(&mut self.config.disk, "Free disk space").changed();
                if changed {
                    self.save();
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(theme::dim("Update", 12.5));
                for (s, name) in [(1u32, "Live"), (2, "Every 2 s"), (5, "Every 5 s")] {
                    let fill = if self.config.interval == s {
                        Color32::from_white_alpha(70)
                    } else {
                        Color32::from_white_alpha(18)
                    };
                    if theme::pill(ui, name, fill).clicked() && self.config.interval != s {
                        self.config.interval = s;
                        self.save();
                    }
                }
            });
            ui.add_space(6.0);
            ui.label(theme::dim(
                "A separate tiny helper: MacSweep doesn't stay open. Changes apply instantly; turning it off removes it completely.",
                12.0,
            ));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_preview() {
        let c = Config {
            cpu: false,
            ram: true,
            disk: true,
            interval: 10,
        };
        assert_eq!(Config::parse(&c.render()), c);
        assert_eq!(c.preview(), "RAM 61%  120 GB");
        assert_eq!(Config::parse("").preview(), "CPU 12%  RAM 61%  120 GB");
    }

    #[test]
    fn temporary_copies_are_refused() {
        let ok = std::path::Path::new("/Applications/MacSweep.app/Contents/Library/LoginItems/x");
        assert!(install_problem(ok).is_none());
        for bad in [
            "/private/var/folders/ab/T/AppTranslocation/1234/d/MacSweep.app/Contents/x",
            "/Volumes/MacSweep/MacSweep.app/Contents/x",
        ] {
            assert!(install_problem(std::path::Path::new(bad)).is_some(), "{bad}");
        }
    }

    #[test]
    fn login_item_restarts_only_after_a_crash() {
        let v = login_item(std::path::Path::new("/Applications/MacSweep.app/x"));
        let d = v.as_dictionary().unwrap();
        assert_eq!(d.get("Label").and_then(|v| v.as_string()), Some(LABEL));
        assert_eq!(d.get("RunAtLoad").and_then(|v| v.as_boolean()), Some(true));
        let keep = d.get("KeepAlive").and_then(|v| v.as_dictionary()).unwrap();
        assert_eq!(keep.get("SuccessfulExit").and_then(|v| v.as_boolean()), Some(false));
    }
}
