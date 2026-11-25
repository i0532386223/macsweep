//! Disk Map and the user-confirmed iCloud Drive download action.
use crate::{
    disk_map, fsutil, icloud,
    job::{self, Job},
    safety_log, theme,
};
use eframe::egui::{self, Align2, Color32, FontId, Sense, Ui, vec2};
use std::path::{Path, PathBuf};

fn cloud_drive() -> PathBuf {
    fsutil::home().join("Library/Mobile Documents/com~apple~CloudDocs")
}

fn startup_data() -> PathBuf {
    let data = Path::new("/System/Volumes/Data");
    if data.is_dir() { data.to_owned() } else { PathBuf::from("/") }
}

#[derive(Default)]
pub struct DiskPane {
    root: PathBuf,
    history: Vec<PathBuf>,
    scan: Option<Job<Result<disk_map::Map, String>>>,
    map: Option<disk_map::Map>,
    selected: Option<usize>,
    message: Option<String>,
    show_other: bool,
    revision: u64,
}

impl DiskPane {
    fn scan(&mut self, ctx: &egui::Context, path: PathBuf) {
        if let Some(job) = self.scan.take() {
            job.stop();
        }
        self.root = path.clone();
        self.map = None;
        self.selected = None;
        self.show_other = false;
        self.revision = fsutil::changes();
        self.message = None;
        self.scan = Some(Job::spawn(ctx, move |progress, cancel| {
            disk_map::scan(&path, &progress, &cancel).map_err(|e| {
                safety_log::event(true, &format!("Disk Map: scan stopped, OS error {:?}", e.raw_os_error()));
                e.to_string()
            })
        }));
    }

    fn source(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.history.clear();
        self.scan(ctx, path);
    }

    fn open_entry(&mut self, ctx: &egui::Context, index: usize) {
        let Some(entry) = self.map.as_ref().and_then(|m| m.entries.get(index)) else {
            return;
        };
        if entry.directory {
            let next = self.root.join(&entry.name);
            self.history.push(self.root.clone());
            self.scan(ctx, next);
        } else {
            self.selected = Some(index);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<PathBuf> {
        let ctx = ui.ctx().clone();
        if !self.root.as_os_str().is_empty() && self.revision != fsutil::changes() {
            self.scan(&ctx, self.root.clone());
        }
        if let Some(result) = job::poll(&mut self.scan, &ctx) {
            match result {
                Ok(map) => self.map = Some(map),
                Err(e) => self.message = Some(e),
            }
        }
        ui.label(theme::bold("Disk Map", 28.0));
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Startup Disk")
                .on_hover_text("Map the startup data volume")
                .clicked()
            {
                self.source(&ctx, startup_data());
            }
            if ui.button("Home").clicked() {
                self.source(&ctx, fsutil::home());
            }
            if ui.button("iCloud Drive").clicked() {
                self.source(&ctx, cloud_drive());
            }
            if ui.button("Choose Folder or Volume…").clicked()
                && let Some(path) = icloud::pick(true, &fsutil::home())
            {
                self.source(&ctx, path);
            }
        });
        ui.add_space(4.);
        if self.root.as_os_str().is_empty() {
            ui.label(theme::dim(
                "Choose a location. Each tile shows how much disk space a file or folder uses.",
                14.,
            ));
            return None;
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!self.history.is_empty(), egui::Button::new("Back")).clicked()
                && let Some(path) = self.history.pop()
            {
                self.scan(&ctx, path);
            }
            if ui.button("Refresh").clicked() {
                self.scan(&ctx, self.root.clone());
            }
            ui.label(theme::semi(fsutil::pretty(&self.root), 14.));
        });
        if let Some(job) = &self.scan {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Scanning · {} items checked", job.progress()));
                if ui.button("Stop").clicked() {
                    job.stop();
                    self.message = Some("Scan stopped.".into());
                }
            });
            if self.message.is_some() {
                self.scan = None;
            }
            return None;
        }
        if let Some(msg) = &self.message {
            ui.label(msg);
            return None;
        }
        let Some(map) = &self.map else { return None };
        ui.label(theme::dim(
            format!(
                "{} mapped   ·   {} items checked   ·   {} unreadable",
                fsutil::fmt_size(map.total),
                map.examined,
                map.skipped
            ),
            13.,
        ));
        if map.entries.is_empty() {
            ui.label("This folder is empty or has no visible entries.");
            return None;
        }
        let mut next = None;
        let mut cloud = None;
        const CAP: usize = 32;
        let positive = map.entries.partition_point(|e| e.bytes > 0);
        let named = if positive > CAP { CAP - 1 } else { positive };
        let mut weights: Vec<_> = map.entries[..named].iter().map(|e| e.bytes).collect();
        if positive > named {
            weights.push(
                map.entries[named..positive]
                    .iter()
                    .fold(0_u64, |n, e| n.saturating_add(e.bytes)),
            );
        }
        if !weights.is_empty() {
            let size = vec2(ui.available_width(), (ui.available_height() * 0.48).clamp(100., 360.));
            let (canvas, _) = ui.allocate_exact_size(size, Sense::hover());
            for (i, tile) in disk_map::tiles(&weights, canvas).into_iter().enumerate() {
                if tile.width() < 2. || tile.height() < 2. {
                    continue;
                }
                let grouped = i == named;
                let name = if grouped {
                    format!("Other ({})", positive - named)
                } else {
                    map.entries[i].name.to_string_lossy().into_owned()
                };
                let amount = fsutil::fmt_size(weights[i]);
                let response = ui
                    .interact(tile, ui.scope_id().with(("storage_tile", i)), Sense::click())
                    .on_hover_text(format!("{name}\n{amount}"));
                let color = [
                    theme::SMART.accent,
                    theme::LARGE.accent,
                    theme::HEALTH.accent,
                    theme::BROWSERS.accent,
                    theme::DUPES.accent,
                    theme::AGENTS.accent,
                ][i % 6];
                let active = response.hovered() || self.selected == Some(i) && !grouped;
                let painter = ui.painter().with_clip_rect(tile.shrink(1.));
                painter.rect_filled(tile.shrink(1.), 3, color.gamma_multiply(if active { 0.84 } else { 0.59 }));
                if tile.width() > 48. && tile.height() > 24. {
                    let pos = tile.min + vec2(7., 5.);
                    painter.text(pos, Align2::LEFT_TOP, &name, theme::semi_font(13.), Color32::WHITE);
                    if tile.height() > 43. {
                        painter.text(
                            pos + vec2(0., 18.),
                            Align2::LEFT_TOP,
                            &amount,
                            FontId::proportional(12.),
                            Color32::WHITE,
                        );
                    }
                }
                if response.clicked() {
                    if grouped {
                        self.show_other = true;
                        self.selected = None;
                    } else {
                        next = Some(i);
                    }
                }
            }
        } else {
            ui.label("These entries have no allocated data blocks.");
        }
        let start = if self.show_other { named } else { 0 };
        ui.horizontal_wrapped(|ui| {
            if self.show_other && ui.small_button("Show all entries").clicked() {
                self.show_other = false;
            }
            if let Some(i) = self.selected {
                let entry = &map.entries[i];
                let path = self.root.join(&entry.name);
                ui.label(theme::semi(entry.name.to_string_lossy(), 13.));
                if ui.button("Show in Finder").clicked() {
                    fsutil::reveal(&path);
                }
                if ui.button("Check iCloud Download…").clicked() {
                    cloud = Some(path);
                }
            } else {
                ui.label(theme::dim(
                    "Open a folder to see its contents. Select a file for actions.",
                    12.,
                ));
            }
        });
        egui::ScrollArea::vertical()
            .id_salt(("disk_rows", &self.root, start))
            .show_rows(ui, 20., map.entries.len() - start, |ui, rows| {
                for i in rows.map(|i| i + start) {
                    let e = &map.entries[i];
                    ui.horizontal(|ui| {
                        ui.label(theme::ic(
                            if e.directory { theme::icon::FOLDER } else { theme::icon::FILE },
                            14.,
                        ));
                        ui.monospace(format!("{:>10}", fsutil::fmt_size(e.bytes)));
                        if ui
                            .selectable_label(self.selected == Some(i), e.name.to_string_lossy())
                            .clicked()
                        {
                            next = Some(i);
                        }
                    });
                }
            });
        if let Some(i) = next {
            self.open_entry(&ctx, i);
        }
        cloud
    }
}

#[derive(Default)]
pub struct CloudPane {
    path: Option<PathBuf>,
    probe: Option<Job<Result<icloud::Download, String>>>,
    request: Option<Job<Result<(), String>>>,
    ready: Option<icloud::Download>,
    confirming: bool,
    message: Option<String>,
}

impl CloudPane {
    pub fn select(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.request.is_some() {
            return;
        }
        self.path = Some(path.clone());
        self.ready = None;
        self.message = None;
        self.confirming = false;
        self.probe = Some(Job::spawn(ctx, move |_, _| icloud::inspect(&path)));
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if let Some(result) = job::poll(&mut self.probe, &ctx) {
            match result {
                Ok(item) => self.ready = Some(item),
                Err(reason) => self.message = Some(reason),
            }
        }
        if let Some(result) = job::poll(&mut self.request, &ctx) {
            self.message = Some(match result {
                Ok(()) => {
                    fsutil::bump();
                    "macOS removed the local download. Open the file in Finder to download it again.".into()
                }
                Err(error) => format!("The download could not be removed. {error}"),
            });
        }
        ui.label(theme::bold("iCloud Downloads", 28.));
        ui.label(theme::dim(
            "Free space on this Mac while keeping the original file in iCloud Drive.",
            14.,
        ));
        ui.add_space(14.);
        if ui
            .add_enabled(
                self.request.is_none() && !self.confirming,
                egui::Button::new("Choose iCloud File…"),
            )
            .clicked()
            && let Some(path) = icloud::pick(false, &cloud_drive())
        {
            self.select(&ctx, path);
        }
        if let Some(path) = &self.path {
            ui.add_space(10.);
            ui.label(theme::semi(fsutil::pretty(path), 15.));
        }
        if self.probe.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Checking iCloud status…");
            });
        }
        if self.request.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Removing this download…");
            });
        }
        if let Some(reason) = &self.message {
            ui.label(reason);
        }
        if let Some(item) = &self.ready {
            ui.label(format!("{} on this Mac · Uploaded to iCloud", fsutil::fmt_size(item.bytes)));
            if self.confirming {
                let result = theme::confirm(
                    ui,
                    "Remove this download?",
                    "The cloud copy stays in iCloud Drive. Opening the file again downloads it to this Mac.",
                    None,
                    "Remove Download",
                );
                match result {
                    Some(true) => {
                        if let Some(item) = self.ready.take() {
                            self.request = Some(Job::spawn(&ctx, move |_, _| icloud::remove_download(item)));
                        }
                        self.confirming = false;
                    }
                    Some(false) => self.confirming = false,
                    None => {}
                }
            } else if ui.button("Remove Download…").clicked() {
                self.confirming = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, FullOutput, Pos2, RawInput, Rect, Shape};
    use std::fs;
    use std::time::Duration;

    #[derive(Default)]
    struct Screen {
        ctx: egui::Context,
        disk: DiskPane,
        cloud: CloudPane,
        in_cloud: bool,
    }

    impl Screen {
        fn frame(&mut self, events: Vec<Event>) -> FullOutput {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000., 720.))),
                events,
                ..Default::default()
            };
            let mut output = self.ctx.run_ui(input, |ui| {
                if self.in_cloud {
                    self.cloud.ui(ui);
                } else if let Some(path) = self.disk.ui(ui) {
                    self.cloud.select(&self.ctx, path);
                    self.in_cloud = true;
                }
            });
            // The headless renderer inspects text shapes without uploading textures.
            output.textures_delta.clear();
            output
        }

        fn locate(&mut self, label: &str) -> Pos2 {
            for _ in 0..120 {
                let frame = self.frame(vec![]);
                for item in frame.shapes {
                    if let Shape::Text(text) = item.shape
                        && text.galley.text() == label
                        && text.visual_bounding_rect().is_positive()
                    {
                        return text.visual_bounding_rect().center();
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("Missing control: {label}");
        }

        fn click(&mut self, label: &str) {
            let pos = self.locate(label);
            let button = |pressed| Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![Event::PointerMoved(pos), button(true)]);
            self.frame(vec![button(false)]);
        }
    }

    #[test]
    fn disk_tiles_navigate_back_and_send_a_selected_file_to_icloud() {
        // Draw the real panels and click their labels. All file operations use the fixture.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-disk-ui-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("Example folder")).unwrap();
        let file = root.join("Example folder/Example file.txt");
        fs::write(&file, b"A local document").unwrap();
        let mut screen = Screen::default();
        theme::setup(&screen.ctx);
        screen.disk.source(&screen.ctx, root.clone());
        screen.click("Example folder");
        screen.locate("Example file.txt");
        screen.click("Back");
        assert_eq!(screen.disk.root, root);
        screen.click("Example folder");
        screen.click("Example file.txt");
        screen.click("Check iCloud Download…");
        assert!(screen.in_cloud);
        #[cfg(target_os = "macos")]
        screen.locate("This file is outside iCloud Drive.");
        assert!(screen.cloud.ready.is_none());
        assert_eq!(screen.cloud.path.as_ref(), Some(&file));
        assert_eq!(fs::read(file).unwrap(), b"A local document");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cancel_keeps_the_download_and_confirm_rechecks_the_selected_file() {
        // Simulate the earlier eligible preview. The real provider rejects this local file at confirmation.
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-icloud-ui-{}.txt", std::process::id()));
        fs::write(&path, b"Content is still here").unwrap();
        let mut item = icloud::sample_download(42);
        item.path = path.clone();
        let mut screen = Screen {
            in_cloud: true,
            ..Screen::default()
        };
        theme::setup(&screen.ctx);
        screen.cloud.path = Some(path.clone());
        screen.cloud.ready = Some(item);
        screen.click("Remove Download…");
        screen.click("Cancel");
        assert!(!screen.cloud.confirming);
        assert!(screen.cloud.ready.is_some() && screen.cloud.request.is_none());
        screen.click("Remove Download…");
        screen.click("Remove Download");
        screen.locate("The download could not be removed. This file is outside iCloud Drive.");
        assert_eq!(fs::read(&path).unwrap(), b"Content is still here");
        fs::remove_file(path).unwrap();
    }
}
