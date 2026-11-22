//! The Mac Health screen: battery, SSD and system readings, plus the menu bar
//! monitor switch. Readings load in the background when the screen opens.
use crate::health::{self, Health};
use crate::job::{self, Job};
use crate::monitor::Monitor;
use crate::theme::{self, Palette, icon};
use eframe::egui::{self, Color32, Layout, Ui};

#[derive(Default)]
pub struct HealthPane {
    data: Option<Health>,
    load: Option<Job<Health>>,
    /// Memory, pressure, swap and thermal state change within seconds, so
    /// they are re-read every 2 s while this screen is open.
    live: Option<Job<health::System>>,
    live_at: f64,
    pub monitor: Monitor,
}

fn grouped(n: u128) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// One "label · value" line; the hint explains where it comes from.
fn line(ui: &mut Ui, label: &str, value: String, hint: &str) {
    ui.label(theme::dim(label, 12.5)).on_hover_text(hint);
    ui.label(theme::text(value, 13.0)).on_hover_text(hint);
    ui.end_row();
}

use crate::shared::fmt_mem;

fn na() -> String {
    "Not available".into()
}

fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([18.0, 6.0])
        .min_col_width(150.0)
        .show(ui, add);
}

impl HealthPane {
    fn reload(&mut self, ctx: &egui::Context) {
        if self.load.is_none() {
            self.load = Some(Job::spawn(ctx, |_, _| health::collect()));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: Palette) {
        let ctx = ui.ctx().clone();
        if self.data.is_none() {
            self.reload(&ctx);
        }
        if let Some(h) = job::poll(&mut self.load, &ctx) {
            self.data = Some(h);
        }
        let now = ctx.input(|i| i.time);
        if self.data.is_some() && self.load.is_none() && self.live.is_none() && now - self.live_at > 2.0 {
            self.live_at = now;
            self.live = Some(Job::spawn(&ctx, |_, _| health::system_now()));
        }
        if let Some(sys) = job::poll(&mut self.live, &ctx) {
            if let Some(d) = &mut self.data {
                d.system = sys;
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(2));

        ui.horizontal(|ui| {
            theme::icon_tile(ui, icon::HEART, p.accent, 46.0);
            ui.vertical(|ui| {
                ui.label(theme::bold("Mac Health", 28.0));
                ui.label(theme::dim("Battery, storage and system, straight from macOS", 13.5));
            });
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.load.is_some() {
                    "Reading…".to_string()
                } else {
                    format!("{}  Refresh", icon::ARROW_CLOCKWISE)
                };
                if theme::pill(ui, label, Color32::from_white_alpha(35)).clicked() {
                    self.reload(&ctx);
                }
            });
        });
        ui.add_space(10.0);

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            match self.data.clone() {
                None => {
                    theme::card(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(theme::dim("Reading battery, storage and system information…", 13.0));
                        });
                    });
                }
                Some(h) => {
                    self.battery(ui, &h);
                    ui.add_space(10.0);
                    self.disk(ui, &h, p);
                    ui.add_space(10.0);
                    self.system(ui, &h);
                }
            }
            ui.add_space(10.0);
            self.monitor.card(ui, p);
            ui.add_space(10.0);
            ui.label(theme::dim(
                "Every value comes from macOS itself (system_profiler, the I/O Registry and the drive's own NVMe health log). \
                 Nothing is estimated: if macOS doesn't report something, it says Not available. Hover a value to see its source.",
                11.5,
            ));
        });
    }

    fn battery(&self, ui: &mut Ui, h: &Health) {
        theme::card(ui, |ui| {
            ui.set_width(ui.available_width());
            let Some(b) = &h.battery else {
                ui.horizontal(|ui| {
                    theme::icon_tile(ui, icon::BATTERY, Color32::from_white_alpha(90), 34.0);
                    ui.label(theme::semi("Battery", 15.0));
                    ui.label(theme::dim("This Mac has no battery.", 13.0));
                });
                return;
            };
            ui.horizontal(|ui| {
                let glyph = if b.charging { icon::BATTERY_CHARGING } else { icon::BATTERY };
                theme::icon_tile(ui, glyph, theme::GOOD, 34.0);
                ui.vertical(|ui| {
                    ui.label(theme::semi("Battery", 15.0));
                    ui.label(theme::dim("Health as Apple reports it in System Settings › Battery", 12.0));
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(c) = &b.condition {
                        let color = if c == "Normal" { theme::GOOD } else { theme::WARN };
                        theme::badge(ui, c, color);
                        ui.label(theme::dim("Condition", 12.0));
                    }
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(theme::bold(b.max_capacity_pct.map_or("—".into(), |v| format!("{v}%")), 34.0));
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.label(theme::text("Maximum capacity", 13.0));
                    ui.label(theme::dim("compared with when it was new", 11.5));
                });
            });
            if let Some(v) = b.max_capacity_pct {
                theme::usage_bar(ui, v as f32 / 100.0, theme::GOOD, 6.0);
            }
            ui.add_space(8.0);
            grid(ui, "battery_grid", |ui| {
                let cycles = match (b.cycles, b.design_cycles) {
                    (Some(c), Some(d)) => format!("{c} of {} it's designed for", grouped(d.into())),
                    (Some(c), None) => c.to_string(),
                    _ => na(),
                };
                line(
                    ui,
                    "Charge cycles",
                    cycles,
                    "Cycle count from System Information; design cycles from the battery itself.",
                );
                let state = match (b.charge_pct, b.charging, b.plugged_in) {
                    (Some(c), true, _) => format!("{c}% · charging"),
                    (Some(c), false, true) => format!("{c}% · plugged in, not charging"),
                    (Some(c), false, false) => format!("{c}% · on battery"),
                    _ => na(),
                };
                line(ui, "Charge now", state, "From the battery (I/O Registry).");
                let mah = |v: Option<u32>| v.map_or_else(na, |v| format!("{} mAh", grouped(v.into())));
                line(
                    ui,
                    "Design capacity",
                    mah(b.design_mah),
                    "DesignCapacity, as reported by the battery.",
                );
                line(
                    ui,
                    "Full-charge capacity",
                    match (b.nominal_mah, b.raw_max_mah) {
                        (Some(n), Some(r)) => format!("{} mAh (raw reading {} mAh)", grouped(n.into()), grouped(r.into())),
                        (Some(n), None) => format!("{} mAh", grouped(n.into())),
                        _ => na(),
                    },
                    "NominalChargeCapacity and AppleRawMaxCapacity, as reported by the battery.",
                );
            });
            ui.add_space(4.0);
            ui.label(theme::dim(
                "Maximum capacity is Apple's own figure. Apple calculates it its own way, so it won't exactly equal the mAh ratio above.",
                11.5,
            ));
        });
    }

    fn disk(&self, ui: &mut Ui, h: &Health, _p: Palette) {
        let d = &h.disk;
        theme::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::icon_tile(ui, icon::HARD_DRIVE, Color32::from_rgb(0x5a, 0xa8, 0xff), 34.0);
                ui.vertical(|ui| {
                    ui.label(theme::semi("Storage", 15.0));
                    let sub = match (&d.model, d.capacity_bytes) {
                        (Some(m), Some(c)) => format!("{m} · {} GB", c / 1_000_000_000),
                        (Some(m), None) => m.clone(),
                        _ => "Internal drive".into(),
                    };
                    ui.label(theme::dim(sub, 12.0));
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(st) = &d.smart_status {
                        let color = if st == "Verified" { theme::GOOD } else { theme::DANGER };
                        theme::badge(ui, st, color);
                        ui.label(theme::dim("macOS health check", 12.0))
                            .on_hover_text("S.M.A.R.T. status from System Information: Verified or Failing.");
                    }
                });
            });
            ui.add_space(8.0);
            let Some(s) = &d.smart else {
                ui.label(theme::dim(
                    "Detailed readings aren't available for this drive. macOS's own health check is shown above.",
                    12.5,
                ));
                return;
            };
            if s.critical_warning != 0 {
                ui.label(
                    theme::text(
                        format!(
                            "{}  The drive reports a critical warning (code {:#04x}). Back up your data.",
                            icon::WARNING,
                            s.critical_warning
                        ),
                        13.0,
                    )
                    .color(theme::DANGER),
                );
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.label(theme::bold(format!("{}%", s.percentage_used), 34.0));
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.label(theme::text("of its rated lifespan used", 13.0));
                    ui.label(theme::dim(
                        "SSDs wear with writing; 100% is the maker's rating, not a failure point",
                        11.5,
                    ));
                });
            });
            let color = if s.percentage_used >= 90 {
                theme::DANGER
            } else if s.percentage_used >= 70 {
                theme::WARN
            } else {
                theme::GOOD
            };
            theme::usage_bar(ui, (s.percentage_used.min(100)) as f32 / 100.0, color, 6.0);
            ui.add_space(8.0);
            let src = "From the drive's NVMe health log (the same data smartctl shows).";
            grid(ui, "disk_grid", |ui| {
                line(ui, "Spare blocks left", format!("{}%", s.available_spare), src);
                line(ui, "Data written", health::fmt_bytes(s.data_written_bytes), src);
                line(ui, "Data read", health::fmt_bytes(s.data_read_bytes), src);
                line(
                    ui,
                    "Powered on",
                    format!(
                        "{} hours ({} days)",
                        grouped(s.power_on_hours),
                        grouped(s.power_on_hours / 24)
                    ),
                    src,
                );
                line(ui, "Power cycles", grouped(s.power_cycles), src);
                line(ui, "Unexpected power losses", grouped(s.unsafe_shutdowns), src);
                line(ui, "Data integrity errors", grouped(s.media_errors), src);
                line(ui, "Error log entries", grouped(s.error_log_entries), src);
                line(ui, "Temperature", format!("{} °C", s.temperature_c), src);
            });
        });
    }

    fn system(&self, ui: &mut Ui, h: &Health) {
        let s = &h.system;
        theme::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::icon_tile(ui, icon::CPU, Color32::from_rgb(0x96, 0x82, 0xff), 34.0);
                ui.label(theme::semi("System", 15.0));
                ui.label(theme::dim("· memory and temperature state update live", 12.0));
            });
            ui.add_space(8.0);
            grid(ui, "system_grid", |ui| {
                let src = "From macOS (sysctl and Foundation).";
                line(ui, "Model", s.model_id.clone().unwrap_or_else(na), src);
                line(ui, "Chip", s.chip.clone().unwrap_or_else(na), src);
                line(
                    ui,
                    "Memory",
                    s.memory_bytes.map_or_else(na, |m| format!("{} GB", m / 1_073_741_824)),
                    src,
                );
                let os = match (&s.macos, &s.build) {
                    (Some(v), Some(b)) => format!("macOS {v} ({b})"),
                    (Some(v), None) => format!("macOS {v}"),
                    _ => na(),
                };
                line(ui, "macOS", os, src);
                line(ui, "Up since restart", s.uptime_secs.map_or_else(na, health::fmt_uptime), src);
                line(
                    ui,
                    "Temperature state",
                    s.thermal.map_or_else(na, |t| t.label().to_string()),
                    "macOS's thermal state: whether it's slowing the Mac down to cool it.",
                );
                let mem = match (s.ram_used, s.memory_bytes, s.pressure) {
                    (Some(u), Some(t), Some(p)) => {
                        format!("{} of {} used · pressure {p}", fmt_mem(u), fmt_mem(t))
                    }
                    _ => na(),
                };
                line(
                    ui,
                    "Memory in use",
                    mem,
                    "Activity Monitor's Memory Used: all memory minus free memory and cached files macOS can drop instantly.",
                );
                let swap = match (s.swap_used, s.swap_total) {
                    (Some(0), _) => "None".to_string(),
                    (Some(u), Some(t)) => format!("{} of {}", fmt_mem(u), fmt_mem(t)),
                    _ => na(),
                };
                line(ui, "Swap in use", swap, "Memory macOS has moved to disk (vm.swapusage).");
            });
        });
    }
}
