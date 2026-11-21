//! MacSweep Monitor: a menu-bar item showing CPU, memory and disk.
//!
//! Built to cost almost nothing while it runs:
//! - native AppKit only (macOS draws the text; no graphics engine),
//! - numbers read straight from the kernel (no subprocesses),
//! - one timer every few seconds, with tolerance so macOS can batch the
//!   wake-up with its own, and no updates while the screen is off,
//! - disk space, which changes slowly, is read once a minute.
//!
//! It is started by MacSweep (as a login item) only if the user turns it on.

// On other systems only the shared logic is built (for tests).
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

mod stats;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("MacSweep Monitor runs on macOS only.");
}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(target_os = "macos")]
mod mac {
    use crate::stats::{self, Config, Sampler};
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSFont, NSFontWeightRegular, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
        NSVariableStatusItemLength, NSWorkspace, NSWorkspaceScreensDidSleepNotification, NSWorkspaceScreensDidWakeNotification,
    };
    use objc2_foundation::{
        NSArray, NSNotification, NSNumber, NSString, NSTimer, NSURL, NSURLVolumeAvailableCapacityForImportantUsageKey, ns_string,
    };
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub const LABEL: &str = "com.nihal.macsweep.monitor";

    /// True while the displays are asleep: nobody can see the menu bar.
    static ASLEEP: AtomicBool = AtomicBool::new(false);

    fn home() -> PathBuf {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
    }

    fn config() -> Config {
        let p = home().join("Library/Application Support/MacSweep/monitor.conf");
        Config::parse(&std::fs::read_to_string(p).unwrap_or_default())
    }

    /// MacSweep.app, found from where this helper lives inside it:
    /// MacSweep.app/Contents/Library/LoginItems/MacSweep Monitor.app/Contents/MacOS/…
    fn main_app() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        exe.ancestors()
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .nth(1)
            .map(PathBuf::from)
    }

    /// Free space the way Finder counts it (includes purgeable space).
    fn disk_free() -> Option<u64> {
        let url = NSURL::fileURLWithPath(ns_string!("/"));
        let key = unsafe { NSURLVolumeAvailableCapacityForImportantUsageKey };
        let keys = NSArray::from_slice(&[key]);
        let found = url
            .resourceValuesForKeys_error(&keys)
            .ok()
            .and_then(|d| d.objectForKey(key))
            .and_then(|v| v.downcast::<NSNumber>().ok())
            .map(|n| n.longLongValue())
            .filter(|&n| n > 0)
            .map(|n| n as u64);
        found.or_else(stats::disk_free_plain)
    }

    define_class!(
        // Receives the menu's clicks.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "MacSweepMonitorActions"]
        struct Actions;

        impl Actions {
            #[unsafe(method(openMacSweep:))]
            fn open_macsweep(&self, _sender: Option<&AnyObject>) {
                if let Some(app) = main_app() {
                    let _ = std::process::Command::new("open").arg(app).spawn();
                }
            }

            /// Removes the login item and quits. MacSweep can turn it back on.
            #[unsafe(method(turnOff:))]
            fn turn_off(&self, _sender: Option<&AnyObject>) {
                let plist = home().join(format!("Library/LaunchAgents/{LABEL}.plist"));
                let _ = std::fs::remove_file(&plist);
                let uid = unsafe { libc::getuid() };
                // Stops this process; exit below is the fallback.
                let _ = std::process::Command::new("launchctl")
                    .args(["bootout", &format!("gui/{uid}/{LABEL}")])
                    .status();
                std::process::exit(0);
            }
        }
    );

    impl Actions {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            unsafe { msg_send![Self::alloc(mtm), init] }
        }
    }

    fn info_item(mtm: MainThreadMarker, menu: &NSMenu) -> Retained<NSMenuItem> {
        let item = NSMenuItem::new(mtm);
        item.setEnabled(false);
        menu.addItem(&item);
        item
    }

    fn action_item(mtm: MainThreadMarker, menu: &NSMenu, title: &str, action: objc2::runtime::Sel, target: &Actions) {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                ns_string!(""),
            )
        };
        unsafe { item.setTarget(Some(target)) };
        menu.addItem(&item);
    }

    struct Ui {
        item: Retained<NSStatusItem>,
        cpu: Retained<NSMenuItem>,
        ram: Retained<NSMenuItem>,
        pressure: Retained<NSMenuItem>,
        disk: Retained<NSMenuItem>,
        own: Retained<NSMenuItem>,
    }

    struct State {
        sampler: Sampler,
        config: Config,
        disk: Option<u64>,
        ticks: u64,
    }

    fn set(item: &NSMenuItem, text: &str) {
        item.setTitle(&NSString::from_str(text));
    }

    fn update(ui: &Ui, s: &mut State, mtm: MainThreadMarker) {
        let cpu = s.sampler.cpu();
        let used = s.sampler.ram_used();
        let total = s.sampler.ram_total;
        let p = stats::pressure();
        // Disk space changes slowly: once a minute is plenty.
        let every = (60.0 / s.config.interval).ceil().max(1.0) as u64;
        if s.ticks % every == 0 {
            s.disk = disk_free();
        }
        s.ticks += 1;

        if let Some(button) = ui.item.button(mtm) {
            button.setTitle(&NSString::from_str(&stats::title(&s.config, cpu, used, total, s.disk, p)));
        }
        set(&ui.cpu, &format!("CPU: {cpu:.0}%"));
        set(
            &ui.ram,
            &format!("Memory: {} of {} used", stats::fmt_bytes(used), stats::fmt_bytes(total)),
        );
        set(&ui.pressure, &format!("Memory pressure: {}", p.label()));
        set(
            &ui.disk,
            &match s.disk {
                Some(f) => format!("Disk: {} available", stats::fmt_bytes(f)),
                None => "Disk: unknown".into(),
            },
        );
        set(
            &ui.own,
            &format!(
                "This monitor: {:.1} MB · updates every {:.0} s",
                stats::own_memory() as f64 / 1e6,
                s.config.interval
            ),
        );
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("must start on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        // No Dock icon, no app menu: just the menu-bar item.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            // Digits of equal width, so the item doesn't wobble as numbers change.
            let font = NSFont::monospacedDigitSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular });
            button.setFont(Some(&font));
            button.setTitle(ns_string!("MacSweep"));
        }

        let actions = Actions::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let ui = Ui {
            cpu: info_item(mtm, &menu),
            ram: info_item(mtm, &menu),
            pressure: info_item(mtm, &menu),
            disk: info_item(mtm, &menu),
            own: {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
                info_item(mtm, &menu)
            },
            item,
        };
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        action_item(mtm, &menu, "Open MacSweep", sel!(openMacSweep:), &actions);
        action_item(mtm, &menu, "Turn Off Menu Bar Monitor", sel!(turnOff:), &actions);
        ui.item.setMenu(Some(&menu));

        // Pause while the displays sleep.
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let sleep = RcBlock::new(|_: NonNull<NSNotification>| ASLEEP.store(true, Ordering::Relaxed));
        let wake = RcBlock::new(|_: NonNull<NSNotification>| ASLEEP.store(false, Ordering::Relaxed));
        let _observers = unsafe {
            (
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceScreensDidSleepNotification),
                    None,
                    None,
                    &sleep,
                ),
                center.addObserverForName_object_queue_usingBlock(Some(NSWorkspaceScreensDidWakeNotification), None, None, &wake),
            )
        };

        let config = config();
        let state = Rc::new(RefCell::new(State {
            sampler: Sampler::new(),
            config,
            disk: None,
            ticks: 0,
        }));
        let ui = Rc::new(ui);
        update(&ui, &mut state.borrow_mut(), mtm);

        let (ui2, state2) = (ui.clone(), state.clone());
        let tick = RcBlock::new(move |_: NonNull<NSTimer>| {
            if !ASLEEP.load(Ordering::Relaxed) {
                update(&ui2, &mut state2.borrow_mut(), mtm);
            }
        });
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(config.interval, true, &tick) };
        // Let macOS shift each wake-up by up to 20% to batch it with others.
        timer.setTolerance(config.interval * 0.2);

        app.run();
    }
}
