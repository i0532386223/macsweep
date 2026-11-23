//! The Browser Cleanup screen: one section per browser, and inside it
//! AI models, website data and cache, each line with its size and what
//! removing it means. Lines can be narrowed to one profile, searched and
//! sorted; only lines that are both ticked and shown are ever cleaned.
use crate::browsers::{self, Browser, Group, Item, Outcome, Plan};
use crate::fsutil::{self, fmt_size};
use crate::job::{self, Job};
use crate::junk::{busy_screen, done_screen};
use crate::theme::{self, Palette, icon};
use crate::uninstall;
use eframe::egui::{self, Color32, ColorImage, Layout, TextureHandle, TextureOptions, Ui, vec2};
use std::collections::HashMap;
use std::path::PathBuf;

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

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Size,
    Profile,
    Name,
    Recent,
}

impl Sort {
    const ALL: [Sort; 4] = [Sort::Size, Sort::Profile, Sort::Name, Sort::Recent];
    fn label(self) -> &'static str {
        match self {
            Sort::Size => "Size",
            Sort::Profile => "Profile",
            Sort::Name => "Name",
            Sort::Recent => "Recent",
        }
    }
}

/// Logos, loaded in the background after a scan.
enum Logo {
    Browser(&'static str, ColorImage),
    Site(PathBuf, String, ColorImage),
}

/// What the list shows right now.
struct View {
    /// None = all profiles
    profile: Option<String>,
    query: String,
    sort: Sort,
}

impl View {
    fn shows(&self, it: &Item) -> bool {
        let profile_ok = match (&self.profile, &it.profile) {
            (Some(want), Some(p)) => want == p,
            _ => true, // browser-wide items show for every profile
        };
        let q = self.query.trim().to_lowercase();
        let query_ok = q.is_empty() || it.title.to_lowercase().contains(&q) || it.detail.to_lowercase().contains(&q);
        profile_ok && query_ok
    }
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
    view: View,
    running: Option<Job<Vec<bool>>>,
    running_at: f64,
    logo_job: Option<Job<Vec<Logo>>>,
    browser_logos: HashMap<&'static str, TextureHandle>,
    site_logos: HashMap<(PathBuf, String), TextureHandle>,
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
            view: View {
                profile: None,
                query: String::new(),
                sort: Sort::Size,
            },
            running: None,
            running_at: 0.0,
            logo_job: None,
            browser_logos: HashMap::new(),
            site_logos: HashMap::new(),
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

/// Browser logos from the apps, website logos from each profile's own
/// icon database. Nothing is downloaded.
fn load_logos(browsers: Vec<(&'static str, Option<PathBuf>, Vec<(PathBuf, String)>)>) -> Vec<Logo> {
    let mut out = Vec::new();
    for (name, app, sites) in browsers {
        if let Some(app) = app {
            let file = browsers::app_icon_file(&app);
            if let Some(img) = uninstall::load_icon(&app, file.as_deref()).or_else(|| uninstall::system_icon(&app)) {
                out.push(Logo::Browser(name, img));
            }
        }
        let mut by_profile: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for (dir, host) in sites {
            by_profile.entry(dir).or_default().push(host);
        }
        for (dir, hosts) in by_profile {
            let icons = browsers::favicons(&dir);
            for host in hosts {
                if let Some(img) = browsers::favicon_for(&icons, &host).and_then(|png| uninstall::decode_png_bytes(png)) {
                    out.push(Logo::Site(dir.clone(), host, img));
                }
            }
        }
    }
    out
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
        let view = &self.view;
        let plan = Plan {
            name: b.name,
            bundle_id: b.bundle_id,
            data: b.data.clone(),
            cache: b.cache.clone(),
            items: b
                .items
                .iter()
                .filter(|i| i.selected && view.shows(i))
                .map(|i| (i.group, i.paths.clone(), i.size))
                .collect(),
        };
        self.cleaning = b.name;
        self.clean = Some(Job::spawn(ctx, move |_, _| browsers::clean(vec![plan])));
    }

    fn reset_view(&mut self) {
        self.view.profile = None;
        self.view.query.clear();
        self.all_sites = false;
        self.confirm = false;
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(found) = job::poll(&mut self.scan, ctx) {
            // Keep the same browser in focus after a rescan.
            let was = self.browsers.get(self.focus).map(|b| b.name);
            self.browsers = found;
            self.focus = was.and_then(|n| self.browsers.iter().position(|b| b.name == n)).unwrap_or(0);
            if let Some(p) = &self.view.profile {
                if !self.browsers.get(self.focus).is_some_and(|b| b.profiles.contains(p)) {
                    self.view.profile = None;
                }
            }
            self.stage = Stage::Results;
            let want: Vec<_> = self
                .browsers
                .iter()
                .map(|b| {
                    let sites = b
                        .items
                        .iter()
                        .filter(|i| i.group == Group::Site)
                        .filter_map(|i| Some((i.profile_dir.clone()?, i.title.clone())))
                        .filter(|k| !self.site_logos.contains_key(k))
                        .collect();
                    let app = (!self.browser_logos.contains_key(b.name)).then(|| b.app.clone()).flatten();
                    (b.name, app, sites)
                })
                .collect();
            self.logo_job = Some(Job::spawn(ctx, move |_, _| load_logos(want)));
        }
        if let Some(logos) = job::poll(&mut self.logo_job, ctx) {
            for l in logos {
                match l {
                    Logo::Browser(name, img) => {
                        let tex = ctx.load_texture(format!("browser-{name}"), img, TextureOptions::LINEAR);
                        self.browser_logos.insert(name, tex);
                    }
                    Logo::Site(dir, host, img) => {
                        let id = format!("site-{}-{host}", dir.display());
                        let tex = ctx.load_texture(id, img, TextureOptions::LINEAR);
                        self.site_logos.insert((dir, host), tex);
                    }
                }
            }
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

        let mut switch_to = None;
        egui::Panel::left("browsers_list")
            .resizable(false)
            .exact_size((ui.available_width() * 0.30).clamp(220.0, 290.0))
            .frame(theme::side_frame())
            .show_separator_line(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for (i, b) in self.browsers.iter().enumerate() {
                        let logo = self.browser_logos.get(b.name);
                        let resp = theme::row(ui, 58.0, self.focus == i, |ui| {
                            browser_logo(ui, logo, p, 36.0);
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
                            switch_to = Some(i);
                        }
                        ui.add_space(2.0);
                    }
                });
            });
        if let Some(i) = switch_to {
            self.focus = i;
            self.reset_view();
        }

        let mut changed = false;
        let logo = self.browser_logos.get(self.browsers[self.focus].name).cloned();
        let b = &mut self.browsers[self.focus];
        let view = &mut self.view;
        let all_sites = &mut self.all_sites;
        let site_logos = &self.site_logos;
        theme::card(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                browser_logo(ui, logo.as_ref(), p, 42.0);
                ui.vertical(|ui| {
                    ui.label(theme::bold(b.name, 19.0));
                    ui.label(theme::dim(theme::plural(b.profiles.len(), "profile"), 12.5));
                });
            });
            if b.running {
                ui.label(
                    theme::dim(
                        format!(
                            "{}  {name} is open. Quit it with ⌘Q to clean: a browser can damage files that change \
                             while it runs. If {name} has no windows but is still open, turn off \"Continue running \
                             background apps\" in its settings.",
                            icon::WARNING,
                            name = b.name
                        ),
                        12.5,
                    )
                    .color(theme::WARN),
                );
            }
            changed |= controls(ui, b, view);
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for g in [Group::Model, Group::Site, Group::Junk] {
                    changed |= group_section(ui, b, g, view, all_sites, site_logos);
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
        let view = &self.view;
        let sel = move |g: Group| b.items.iter().filter(move |i| i.selected && i.group == g && view.shows(i));
        let deleted: u64 = sel(Group::Junk).map(|i| i.size).sum();
        let trashed: u64 = sel(Group::Model).chain(sel(Group::Site)).map(|i| i.size).sum();
        let n = b.items.iter().filter(|i| i.selected && view.shows(i)).count();
        let hidden = b.items.iter().filter(|i| i.selected && !view.shows(i)).count();
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
            format!("Quit {name} with ⌘Q to clean it")
        } else {
            let mut v = Vec::new();
            if deleted > 0 {
                v.push(format!("{} deleted", fmt_size(deleted)));
            }
            if trashed > 0 {
                v.push(format!("{} to the Trash", fmt_size(trashed)));
            }
            let mut line = if v.is_empty() {
                format!("Nothing selected in {name}")
            } else {
                format!("{} · {name} only", v.join(" · "))
            };
            // Only what is shown is cleaned, so say when the filter hides ticked lines.
            if hidden > 0 {
                line.push_str(&format!(
                    " · {} hidden by the filter won't be cleaned",
                    theme::plural(hidden, "ticked line")
                ));
            }
            line
        };
        ui.label(theme::dim(line, 12.5));
    }
}

/// The browser's own logo, or a globe until it has loaded.
fn browser_logo(ui: &mut Ui, logo: Option<&TextureHandle>, p: Palette, size: f32) {
    match logo {
        Some(t) => {
            ui.add(egui::Image::new(t).fit_to_exact_size(vec2(size, size)));
        }
        None => {
            theme::icon_tile(ui, icon::GLOBE, p.accent.gamma_multiply(0.8), size * 0.94);
        }
    }
}

fn pill_fill(on: bool) -> Color32 {
    if on {
        Color32::from_white_alpha(70)
    } else {
        Color32::from_white_alpha(18)
    }
}

/// Profile chips, search and sort. Returns true if the view changed.
fn controls(ui: &mut Ui, b: &Browser, view: &mut View) -> bool {
    let mut changed = false;
    if b.profiles.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::dim("Profile", 12.0));
            let chip = |ui: &mut Ui, label: &str, on: bool| theme::pill(ui, label, pill_fill(on)).clicked();
            if chip(ui, "All profiles", view.profile.is_none()) {
                view.profile = None;
                changed = true;
            }
            for name in &b.profiles {
                if chip(ui, name, view.profile.as_ref() == Some(name)) {
                    view.profile = Some(name.clone());
                    changed = true;
                }
            }
        });
    }
    ui.horizontal(|ui| {
        let search = egui::TextEdit::singleline(&mut view.query)
            .hint_text(format!("{}  Search websites", icon::MAGNIFYING_GLASS))
            .desired_width(240.0)
            .margin(vec2(10.0, 6.0));
        if ui.add(search).changed() {
            changed = true;
        }
        if !view.query.is_empty() && theme::pill(ui, "Clear", pill_fill(false)).clicked() {
            view.query.clear();
            changed = true;
        }
        // Same pills as the Uninstaller's sort, right-aligned.
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            for s in Sort::ALL.into_iter().rev() {
                if theme::pill(ui, s.label(), pill_fill(view.sort == s)).clicked() {
                    view.sort = s;
                }
            }
            ui.label(theme::dim("Sort", 12.5));
        });
    });
    changed
}

/// One group's header (with select-all for the lines shown) and its lines.
/// Returns true if the selection changed.
fn group_section(
    ui: &mut Ui,
    b: &mut Browser,
    g: Group,
    view: &View,
    all_sites: &mut bool,
    site_logos: &HashMap<(PathBuf, String), TextureHandle>,
) -> bool {
    let (title, glyph, color) = group_info(g);
    let note = group_note(g, b);
    let is_chrome = b.bundle_id.starts_with("com.google.Chrome");
    let bundle = b.bundle_id;
    let profiles = b.profiles.clone();
    let mut changed = false;
    let all_in_group = b.items.iter().filter(|i| i.group == g).count();
    let mut items: Vec<usize> = (0..b.items.len())
        .filter(|&i| b.items[i].group == g && view.shows(&b.items[i]))
        .collect();
    let profile_rank = |it: &Item| {
        it.profile
            .as_ref()
            .and_then(|p| profiles.iter().position(|x| x == p))
            .unwrap_or(0)
    };
    match view.sort {
        Sort::Size => items.sort_by(|&x, &y| b.items[y].size.cmp(&b.items[x].size)),
        Sort::Profile => items.sort_by(|&x, &y| {
            let (a, c) = (&b.items[x], &b.items[y]);
            profile_rank(a).cmp(&profile_rank(c)).then(c.size.cmp(&a.size))
        }),
        Sort::Name => items.sort_by(|&x, &y| {
            let key = |i: &Item| i.title.trim_start_matches("www.").to_lowercase();
            key(&b.items[x]).cmp(&key(&b.items[y]))
        }),
        Sort::Recent => items.sort_by(|&x, &y| b.items[y].newest.cmp(&b.items[x].newest)),
    }
    let shown_total: u64 = items.iter().map(|&i| b.items[i].size).sum();

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
        if items.len() < all_in_group {
            ui.label(theme::dim(format!("showing {} of {}", items.len(), all_in_group), 12.0));
        }
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(theme::semi(
                if items.is_empty() {
                    "—".into()
                } else {
                    fmt_size(shown_total)
                },
                14.0,
            ));
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
        let empty = if all_in_group > 0 {
            "Nothing matches the filter."
        } else {
            match g {
                Group::Model => "No AI models downloaded.",
                Group::Site => "No website data.",
                Group::Junk => "No cache.",
            }
        };
        ui.label(theme::dim(format!("{}  {empty}", icon::CHECK_CIRCLE), 13.0));
        return changed;
    }

    let many_profiles = b.profiles.len() > 1 && view.profile.is_none();
    let shown = if g == Group::Site && !*all_sites && view.query.trim().is_empty() {
        FIRST_SITES
    } else {
        usize::MAX
    };
    let mut last_profile: Option<String> = None;
    for &i in items.iter().take(shown) {
        let running = b.running;
        let it = &mut b.items[i];
        // "By profile": a small heading where each profile starts.
        if view.sort == Sort::Profile && many_profiles && it.profile.is_some() && it.profile != last_profile {
            last_profile = it.profile.clone();
            ui.add_space(4.0);
            ui.label(theme::semi(it.profile.clone().unwrap_or_default(), 12.5));
        }
        let logo = match (&it.profile_dir, g) {
            (Some(dir), Group::Site) => site_logos.get(&(dir.clone(), it.title.clone())),
            _ => None,
        };
        egui::Frame::new()
            .fill(Color32::from_white_alpha(10))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!running, egui::Checkbox::new(&mut it.selected, "")).changed() {
                        changed = true;
                    }
                    if g == Group::Site {
                        match logo {
                            Some(t) => {
                                ui.add(egui::Image::new(t).fit_to_exact_size(vec2(18.0, 18.0)));
                            }
                            None => {
                                ui.label(theme::ic(icon::GLOBE, 16.0).color(Color32::from_white_alpha(110)));
                            }
                        }
                    }
                    // Size on the right first, so long text wraps before it.
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.allocate_ui_with_layout(vec2(96.0, 34.0), Layout::top_down(egui::Align::Max), |ui| {
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
                                    if many_profiles && view.sort != Sort::Profile {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, profile: Option<&str>, group: Group) -> Item {
        Item {
            group,
            title: title.into(),
            detail: "offline files 3.8 GB".into(),
            profile: profile.map(Into::into),
            profile_dir: None,
            paths: vec![],
            size: 1,
            newest: None,
            selected: true,
        }
    }

    #[test]
    fn filter_by_profile_and_search() {
        let game = item("web.nfsmwo.com", Some("NIHAL (Default)"), Group::Site);
        let fb = item("www.facebook.com", Some("Potato (Profile 4)"), Group::Site);
        let model = item("Gemini Nano", None, Group::Model);
        let mut v = View {
            profile: None,
            query: String::new(),
            sort: Sort::Size,
        };
        assert!(v.shows(&game) && v.shows(&fb) && v.shows(&model));

        v.profile = Some("NIHAL (Default)".into());
        assert!(v.shows(&game) && !v.shows(&fb));
        assert!(v.shows(&model), "browser-wide items show under every profile");

        v.profile = None;
        v.query = "  NFS ".into(); // case and spaces don't matter
        assert!(v.shows(&game) && !v.shows(&fb) && !v.shows(&model));
        v.query = "offline".into(); // the breakdown is searched too
        assert!(v.shows(&game));
    }
}
