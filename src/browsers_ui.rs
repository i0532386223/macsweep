//! The Browser Cleanup screen: one section per browser, and inside it
//! AI models, website data and cache, each line with its size and what
//! removing it means.
use crate::browsers::{self, Browser, Group, Outcome, Plan};
use crate::fsutil::{self, fmt_size};
use crate::job::{self, Job};
use crate::junk::{busy_screen, done_screen};
use crate::theme::{self, Palette, icon};
use eframe::egui::{self, Color32, Layout, Ui};

const MODEL_COLOR: Color32 = Color32::from_rgb(0x9a, 0x6b, 0xf0);
const SITE_COLOR: Color32 = Color32::from_rgb(0x3a, 0x8d, 0xde);
const CACHE_COLOR: Color32 = Color32::from_rgb(0x2f, 0xb5, 0x7c);
/// Long website lists show this many lines until "Show all" is clicked.
const FIRST_SITES: usize = 25;

#[derive(PartialEq)]
enum Stage {
    Intro,
    Results,
    Done(&'static str, Outcome),
}

pub struct BrowsersPane {
    stage: Stage,
    browsers: Vec<Browser>,
    focus: usize,
    scan: Option<Job<Vec<Browser>>>,
    clean: Option<Job<Outcome>>,
    cleaning: &'static str,
    confirm: bool,
    all_sites: bool,
    running: Option<Job<Vec<bool>>>,
    running_at: f64,
}

impl Default for BrowsersPane {
    fn default() -> Self {
        Self {
            stage: Stage::Intro,
            browsers: Vec::new(),
            focus: 0,
            scan: None,
            clean: None,
            cleaning: "",
            confirm: false,
            all_sites: false,
            running: None,
            running_at: 0.0,
        }
    }
}

fn group_info(g: Group) -> (&'static str, &'static str, Color32) {
    match g {
        Group::Model => ("AI models", icon::BRAIN, MODEL_COLOR),
        Group::Site => ("Website data", icon::GAME_CONTROLLER, SITE_COLOR),
        Group::Junk => ("Cache", icon::BROOM, CACHE_COLOR),
    }
}

fn group_note(g: Group, b: &Browser) -> String {
    match g {
        Group::Model => format!(
            "Downloaded by {} for its AI features. Removed items go to the Trash. {} can download them again: \
             to stop that for Gemini Nano, turn off On-device AI in its Settings › AI innovations first.",
            b.name, b.name
        ),
        Group::Site => "What websites keep on this Mac: web game saves and game files, offline files and databases. \
            Removing a site's data deletes its saves and can sign you out of it. Removed items go to the Trash. \
            Nothing here is selected for you."
            .into(),
        Group::Junk => "Rebuilt automatically. Pages and web games load a little slower the first time. \
            Deleted, not moved to the Trash."
            .into(),
    }
}

impl BrowsersPane {
    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scan.is_some() || self.clean.is_some() {
            return;
        }
        self.confirm = false;
        self.scan = Some(Job::spawn(ctx, |_, _| browsers::scan()));
    }

    fn start_clean(&mut self, ctx: &egui::Context) {
        let Some(b) = self.browsers.get(self.focus) else { return };
        let plan = Plan {
            name: b.name,
            bundle_id: b.bundle_id,
            data: b.data.clone(),
            cache: b.cache.clone(),
            items: b
                .items
                .iter()
                .filter(|i| i.selected)
                .map(|i| (i.group, i.paths.clone(), i.size))
                .collect(),
        };
        self.cleaning = b.name;
        self.clean = Some(Job::spawn(ctx, move |_, _| browsers::clean(vec![plan])));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(found) = job::poll(&mut self.scan, ctx) {
            // Keep the same browser in focus after a rescan.
            let was = self.browsers.get(self.focus).map(|b| b.name);
            self.browsers = found;
            self.focus = was.and_then(|n| self.browsers.iter().position(|b| b.name == n)).unwrap_or(0);
            self.stage = Stage::Results;
        }
        if let Some(out) = job::poll(&mut self.clean, ctx) {
            self.stage = Stage::Done(self.cleaning, out);
        }
        // Whether each browser is open, every few seconds while the list shows.
        let now = ctx.input(|i| i.time);
        if self.stage == Stage::Results && self.running.is_none() && now - self.running_at > 3.0 {
            self.running_at = now;
            let ids: Vec<&'static str> = self.browsers.iter().map(|b| b.bundle_id).collect();
            self.running = Some(Job::spawn(ctx, move |_, _| {
                let r = crate::junk::running_apps();
                ids.iter().map(|id| r.contains(&id.to_lowercase())).collect()
            }));
        }
        if let Some(r) = job::poll(&mut self.running, ctx) {
            for (b, on) in self.browsers.iter_mut().zip(r) {
                b.running = on;
            }
        }
        if self.stage == Stage::Results {
            ctx.request_repaint_after(std::time::Duration::from_secs(3));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: Palette) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);

        if self.scan.is_some() {
            busy_screen(
                ui,
                p,
                icon::GLOBE,
                "Looking through your browsers…",
                "Nothing is changed while scanning.",
            );
            return;
        }
        if self.clean.is_some() {
            busy_screen(
                ui,
                p,
                icon::GLOBE,
                &format!("Cleaning {}…", self.cleaning),
                "This only takes a moment.",
            );
            return;
        }
        match &self.stage {
            Stage::Intro => {
                theme::centered(ui, |ui| {
                    ui.add_space(ui.available_height() * 0.10);
                    theme::hero(ui, icon::GLOBE, p, 150.0, false, None);
                    ui.add_space(10.0);
                    ui.label(theme::bold("Browser Cleanup", 30.0));
                    ui.label(theme::dim(
                        "See what each browser keeps on your Mac: AI models it downloaded,\n\
                         website data such as web game saves, and cache. You choose what goes.",
                        15.0,
                    ));
                    ui.add_space(22.0);
                    if theme::round_button(ui, "Scan", p, true).clicked() {
                        self.start_scan(&ctx);
                    }
                });
            }
            Stage::Done(name, out) => {
                let mut parts = Vec::new();
                if out.deleted > 0 {
                    parts.push(format!("{} of cache deleted", fmt_size(out.deleted)));
                }
                if out.trashed > 0 {
                    parts.push(format!(
                        "{} moved to the Trash (empty it to free the space)",
                        fmt_size(out.trashed)
                    ));
                }
                let (title, sub) = if !out.skipped.is_empty() {
                    (
                        format!("{name} is open"),
                        format!("Nothing was changed. Quit {name} and try again."),
                    )
                } else {
                    let mut sub = parts.join(" · ");
                    if out.failed > 0 {
                        sub.push_str(&format!(
                            "{}{} couldn't be removed.",
                            if sub.is_empty() { "" } else { ". " },
                            theme::plural(out.failed, "item")
                        ));
                    }
                    (format!("{name} cleaned"), sub)
                };
                // Back to a fresh scan, so the list shows what's really left.
                if done_screen(ui, p, &title, &sub) {
                    self.stage = Stage::Results;
                    self.start_scan(&ctx);
                }
            }
            Stage::Results => self.results(ui, p, &ctx),
        }
    }

    fn results(&mut self, ui: &mut Ui, p: Palette, ctx: &egui::Context) {
        let total: u64 = self.browsers.iter().map(Browser::total).sum();
        ui.horizontal(|ui| {
            theme::icon_tile(ui, icon::GLOBE, p.accent, 46.0);
            ui.vertical(|ui| {
                ui.label(theme::bold(fmt_size(total), 28.0));
                ui.label(theme::dim(
                    format!("kept by {} on your Mac", theme::plural(self.browsers.len(), "browser")),
                    13.5,
                ));
                ui.label(
                    theme::dim(
                        format!(
                            "{}  Passwords, cookies, history, bookmarks, extensions and sign-ins are never touched",
                            icon::LOCK_SIMPLE
                        ),
                        12.0,
                    )
                    .color(Color32::from_rgb(0xb8, 0xf5, 0xd8)),
                );
            });
            ui.with_layout(Layout::right_to_left(egui::Align::Min), |ui| {
                ui.with_layout(Layout::top_down(egui::Align::Max), |ui| {
                    ui.add_space(11.0);
                    if theme::pill(
                        ui,
                        format!("{}  Scan again", icon::ARROW_CLOCKWISE),
                        Color32::from_white_alpha(35),
                    )
                    .clicked()
                    {
                        self.start_scan(ctx);
                    }
                });
            });
        });
        ui.add_space(10.0);

        if self.browsers.is_empty() {
            theme::centered(ui, |ui| {
                ui.add_space(80.0);
                ui.label(theme::ic(icon::CHECK_CIRCLE, 44.0));
                ui.label(theme::dim(
                    "No supported browser found. MacSweep supports Chrome, Brave, Edge, Arc, Vivaldi, Opera and Chromium.",
                    14.0,
                ));
            });
            return;
        }

        egui::Panel::bottom("browsers_action")
            .frame(theme::bottom_frame())
            .show_separator_line(false)
            .show(ui, |ui| {
                theme::centered(ui, |ui| {
                    ui.add_space(4.0);
                    self.clean_bar(ui, p, ctx);
                });
            });

        egui::Panel::left("browsers_list")
            .resizable(false)
            .exact_size((ui.available_width() * 0.30).clamp(220.0, 290.0))
            .frame(theme::side_frame())
            .show_separator_line(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for (i, b) in self.browsers.iter().enumerate() {
                        let resp = theme::row(ui, 58.0, self.focus == i, |ui| {
                            theme::icon_tile(ui, icon::GLOBE, p.accent.gamma_multiply(0.8), 34.0);
                            ui.vertical(|ui| {
                                ui.add_space(9.0);
                                ui.label(theme::semi(b.name, 14.0));
                                let sub = if b.running {
                                    "Open now".to_string()
                                } else {
                                    theme::plural(b.profiles.len(), "profile")
                                };
                                ui.label(theme::dim(sub, 11.5).color(if b.running {
                                    theme::WARN
                                } else {
                                    Color32::from_white_alpha(160)
                                }));
                            });
                            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(theme::semi(fmt_size(b.total()), 13.5));
                            });
                        });
                        if resp.clicked() && self.focus != i {
                            self.focus = i;
                            self.confirm = false;
                            self.all_sites = false;
                        }
                        ui.add_space(2.0);
                    }
                });
            });

        let many_profiles = self.browsers[self.focus].profiles.len() > 1;
        let mut changed = false;
        let all_sites = &mut self.all_sites;
        let b = &mut self.browsers[self.focus];
        theme::card(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                theme::icon_tile(ui, icon::GLOBE, p.accent, 42.0);
                ui.vertical(|ui| {
                    ui.label(theme::bold(b.name, 19.0));
                    ui.label(theme::dim(format!("Profiles: {}", b.profiles.join(", ")), 12.5));
                });
            });
            if b.running {
                ui.label(
                    theme::dim(
                        format!(
                            "{}  {} is open. Quit it to clean: a browser can damage files that change while it runs.",
                            icon::WARNING,
                            b.name
                        ),
                        12.5,
                    )
                    .color(theme::WARN),
                );
            }
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for g in [Group::Model, Group::Site, Group::Junk] {
                    changed |= group_section(ui, b, g, many_profiles, all_sites);
                    ui.add_space(14.0);
                }
            });
        });
        if changed {
            self.confirm = false;
        }
    }

    fn clean_bar(&mut self, ui: &mut Ui, p: Palette, ctx: &egui::Context) {
        let Some(b) = self.browsers.get(self.focus) else { return };
        let sel = move |g: Group| b.items.iter().filter(move |i| i.selected && i.group == g);
        let deleted: u64 = sel(Group::Junk).map(|i| i.size).sum();
        let trashed: u64 = sel(Group::Model).chain(sel(Group::Site)).map(|i| i.size).sum();
        let n = b.items.iter().filter(|i| i.selected).count();
        if n == 0 || b.running {
            self.confirm = false;
        }
        if self.confirm {
            let mut sub = Vec::new();
            if deleted > 0 {
                sub.push(format!("Cache ({}) is deleted.", fmt_size(deleted)));
            }
            if trashed > 0 {
                let what = match (sel(Group::Model).count() > 0, sel(Group::Site).count() > 0) {
                    (true, true) => "AI models and website data",
                    (true, false) => "AI models",
                    _ => "Website data",
                };
                sub.push(format!(
                    "{what} ({}) go to the Trash; empty the Trash to free that space.",
                    fmt_size(trashed)
                ));
            }
            let sites: Vec<&str> = sel(Group::Site).map(|i| i.title.as_str()).collect();
            let warn = (!sites.is_empty()).then(|| {
                let names = if sites.len() > 3 {
                    format!("{} and {} more", sites[..3].join(", "), sites.len() - 3)
                } else {
                    sites.join(", ")
                };
                format!("Website data for {names}: saves and offline files there will be gone, and you may be signed out.")
            });
            let name = b.name;
            match theme::confirm(
                ui,
                &format!("Clean {} from {name}?", theme::plural(n, "item")),
                &sub.join(" "),
                warn.as_deref(),
                &format!("{}  Clean {}", icon::BROOM, fmt_size(deleted + trashed)),
            ) {
                Some(true) => {
                    self.confirm = false;
                    self.start_clean(ctx);
                }
                Some(false) => self.confirm = false,
                None => {}
            }
            return;
        }
        let name = b.name;
        let running = b.running;
        if theme::round_button(ui, "Clean", p, n > 0 && !running).clicked() {
            self.confirm = true;
        }
        let line = if running {
            format!("Quit {name} to clean it")
        } else {
            let mut v = Vec::new();
            if deleted > 0 {
                v.push(format!("{} deleted", fmt_size(deleted)));
            }
            if trashed > 0 {
                v.push(format!("{} to the Trash", fmt_size(trashed)));
            }
            if v.is_empty() {
                format!("Nothing selected in {name}")
            } else {
                format!("{} · {name} only", v.join(" · "))
            }
        };
        ui.label(theme::dim(line, 12.5));
    }
}

/// One group's header (with select-all) and its lines. Returns true if the
/// selection changed.
fn group_section(ui: &mut Ui, b: &mut Browser, g: Group, many_profiles: bool, all_sites: &mut bool) -> bool {
    let (title, glyph, color) = group_info(g);
    let note = group_note(g, b);
    let is_chrome = b.bundle_id.starts_with("com.google.Chrome");
    let bundle = b.bundle_id;
    let total = b.group_total(g);
    let mut changed = false;
    let items: Vec<usize> = (0..b.items.len()).filter(|&i| b.items[i].group == g).collect();

    ui.horizontal(|ui| {
        let n_sel = items.iter().filter(|&&i| b.items[i].selected).count();
        let mut on = n_sel > 0;
        let cb = egui::Checkbox::new(&mut on, "").indeterminate(n_sel > 0 && n_sel < items.len());
        if ui.add_enabled(!items.is_empty() && !b.running, cb).changed() {
            for &i in &items {
                b.items[i].selected = on;
            }
            changed = true;
        }
        theme::icon_tile(ui, glyph, color, 32.0);
        ui.label(theme::bold(title, 16.0));
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(theme::semi(if items.is_empty() { "—".into() } else { fmt_size(total) }, 14.0));
        });
    });
    ui.label(theme::dim(note, 12.0));
    if g == Group::Model && is_chrome {
        if theme::mini_button(ui, "Open Chrome's AI settings", "Opens Chrome at Settings › AI innovations").clicked() {
            let _ = std::process::Command::new("/usr/bin/open")
                .args(["-b", bundle, "chrome://settings/ai"])
                .spawn();
        }
    }
    ui.add_space(4.0);
    if items.is_empty() {
        let empty = match g {
            Group::Model => "No AI models downloaded.",
            Group::Site => "No website data.",
            Group::Junk => "No cache.",
        };
        ui.label(theme::dim(format!("{}  {empty}", icon::CHECK_CIRCLE), 13.0));
        return changed;
    }

    let shown = if g == Group::Site && !*all_sites {
        FIRST_SITES
    } else {
        usize::MAX
    };
    for &i in items.iter().take(shown) {
        let running = b.running;
        let it = &mut b.items[i];
        egui::Frame::new()
            .fill(Color32::from_white_alpha(10))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!running, egui::Checkbox::new(&mut it.selected, "")).changed() {
                        changed = true;
                    }
                    // Size on the right first, so long text wraps before it.
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.allocate_ui_with_layout(egui::vec2(96.0, 34.0), Layout::top_down(egui::Align::Max), |ui| {
                            ui.label(theme::semi(fmt_size(it.size), 13.0));
                            if g == Group::Site {
                                ui.label(theme::dim(format!("used {}", fsutil::fmt_age(it.newest)), 11.0));
                            }
                        });
                        ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    let resp = ui.add(
                                        egui::Label::new(theme::semi(&it.title, 13.5))
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                    );
                                    if resp.on_hover_text("Click to reveal in Finder").clicked() {
                                        if let Some(p) = it.paths.first() {
                                            fsutil::reveal(p);
                                        }
                                    }
                                    if many_profiles {
                                        if let Some(pr) = &it.profile {
                                            theme::badge(ui, pr, Color32::from_white_alpha(40));
                                        }
                                    }
                                });
                                ui.add(egui::Label::new(theme::dim(&it.detail, 11.5)).wrap());
                            });
                        });
                    });
                });
            });
        ui.add_space(3.0);
    }
    if items.len() > shown {
        let rest = items.len() - shown;
        if theme::mini_button(ui, format!("Show {} more", theme::plural(rest, "website")), "").clicked() {
            *all_sites = true;
        }
    }
    changed
}
