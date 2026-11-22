//! Shared by MacSweep and the menu-bar helper, so the two can't drift apart:
//! the helper's launchd label, the settings-changed notification, and the
//! settings file format.

/// launchd label of the helper's login item.
pub const LABEL: &str = "com.nihal.macsweep.monitor";
/// Darwin notification MacSweep posts after saving new settings.
pub const RELOAD: &str = "com.nihal.macsweep.monitor.reload";
/// Settings file, relative to the home folder.
pub const CONF: &str = "Library/Application Support/MacSweep/monitor.conf";

/// What the menu bar shows, saved as `key=value` lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub cpu: bool,
    pub ram: bool,
    pub disk: bool,
    /// Seconds between updates (1 = live).
    pub interval: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cpu: true,
            ram: true,
            disk: true,
            interval: 1,
        }
    }
}

impl Config {
    /// Parses `key=value` lines; anything unknown or broken keeps its default.
    pub fn parse(text: &str) -> Self {
        let mut c = Self::default();
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let on = v.trim() == "1";
            match k.trim() {
                "cpu" => c.cpu = on,
                "ram" => c.ram = on,
                "disk" => c.disk = on,
                "interval" => {
                    if let Ok(n) = v.trim().parse::<u32>() {
                        c.interval = n.clamp(1, 60);
                    }
                }
                _ => {}
            }
        }
        c
    }

    pub fn render(&self) -> String {
        let b = |x: bool| u8::from(x);
        format!(
            "cpu={}\nram={}\ndisk={}\ninterval={}\n",
            b(self.cpu),
            b(self.ram),
            b(self.disk),
            self.interval
        )
    }
}

/// Memory sizes exactly as Activity Monitor shows them (1 GB = 1024³ bytes,
/// two decimals).
pub fn fmt_mem(bytes: u64) -> String {
    format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_file_round_trips_and_is_read_safely() {
        let c = Config {
            cpu: false,
            ram: true,
            disk: true,
            interval: 5,
        };
        assert_eq!(Config::parse(&c.render()), c);
        assert_eq!(Config::parse(""), Config::default());
        assert_eq!(Config::parse("interval=0").interval, 1, "never faster than once a second");
        assert_eq!(Config::parse("interval=abc").interval, 1);
        assert_eq!(Config::parse("cpu=0\ngarbage\nram=1").cpu, false);
    }

    #[test]
    fn memory_is_shown_like_activity_monitor() {
        assert_eq!(fmt_mem(8 * 1_073_741_824), "8.00 GB");
        assert_eq!(fmt_mem(6_925_000_000), "6.45 GB");
    }
}
