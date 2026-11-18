//! Duplicate finder: exact duplicates (same size + same BLAKE3 hash) in the
//! user's own folders. Always keeps at least one copy; removals go to the Trash.
use crate::fsutil::{self, fmt_age, fmt_size};
use crate::job::{self, Job};
use crate::junk::{busy_screen, done_screen};
use crate::large::Kind;
use crate::theme::{self, Palette, icon};
use eframe::egui::{self, Color32, Layout, Ui};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::SystemTime;

/// Bytes read from each end of a file for the quick first-pass hash.
const EDGE: u64 = 16 * 1024;

/// Folders that are really packages (libraries, bundles, projects). Their
/// internal "duplicates" are intentional; touching them corrupts the package.
const PACKAGE_EXT: &[&str] = &[
    "app",
    "photoslibrary",
    "photolibrary",
    "migratedphotolibrary",
    "aplibrary",
    "musiclibrary",
    "tvlibrary",
    "imovielibrary",
    "fcpbundle",
    "logicx",
    "band",
    "lrdata",
    "xcodeproj",
    "xcworkspace",
    "bundle",
    "framework",
    "plugin",
    "kext",
    "dsym",
    "pkg",
    "mpkg",
];

/// Developer folders where duplicate files are normal and must not be removed.
const DEV_DIRS: &[&str] = &[
    "node_modules",
    "__pycache__",
    "site-packages",
    "venv",
    "Pods",
    "DerivedData",
    "Library",
    "Applications",
];

/// Build and cache folders mark themselves with a standard file: Rust `target`
/// and other tools write CACHEDIR.TAG, Python virtual environments have
/// pyvenv.cfg. Their contents are generated, so duplicates there are expected.
fn is_generated_dir(p: &Path) -> bool {
    p.join("CACHEDIR.TAG").exists() || p.join("pyvenv.cfg").exists()
}

fn skip_dir(name: &str) -> bool {
    if name.starts_with('.') || DEV_DIRS.contains(&name) {
        return true;
    }
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| PACKAGE_EXT.contains(&e.as_str()))
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

pub struct DupFile {
    path: PathBuf,
    modified: Option<SystemTime>,
    remove: bool,
}

pub struct DupGroup {
    size: u64,
    kind: Kind,
    files: Vec<DupFile>,
}

impl DupGroup {
    fn wasted(&self) -> u64 {
        self.size * (self.files.len() as u64).saturating_sub(1)
    }
    fn selected_bytes(&self) -> u64 {
        self.size * self.files.iter().filter(|f| f.remove).count() as u64
    }
    fn name(&self) -> String {
        self.files[0]
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Keep {
    Oldest,
    Newest,
}

/// Stage shown while scanning: 0 = listing files, 1 = comparing contents.
struct ScanState {
    stage: Arc<AtomicUsize>,
}

pub struct Dupes {
    groups: Vec<DupGroup>,
    scanned: bool,
    min_kb: u64,
    keep: Keep,
    focus: usize,
    note: String,
    state: ScanState,
    scan: Option<Job<Vec<DupGroup>>>,
    trash: Option<Job<(usize, u64, Option<String>)>>,
    done: Option<(usize, u64, Option<String>)>,
}

impl Default for Dupes {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            scanned: false,
            min_kb: 1024,
            keep: Keep::Oldest,
            focus: 0,
            note: String::new(),
            state: ScanState {
                stage: Arc::new(AtomicUsize::new(0)),
            },
            scan: None,
            trash: None,
            done: None,
        }
    }
}

// ---------- engine ----------

/// Folders to search: the visible folders in the home folder (Desktop,
/// Documents, Downloads, Pictures, Movies, Music…), never ~/Library.
fn scan_roots() -> Vec<PathBuf> {
    fsutil::children(&fsutil::home())
        .into_iter()
        .filter(|p| p.is_dir() && !p.is_symlink())
        .filter(|p| p.file_name().is_some_and(|n| !skip_dir(&n.to_string_lossy())))
        .collect()
}

fn list_files(roots: &[PathBuf], min_bytes: u64, progress: &AtomicUsize, cancel: &Arc<AtomicBool>) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut seen_inodes = HashSet::new();
    for root in roots {
        let cancel_cb = cancel.clone();
        let walker = jwalk::WalkDir::new(root)
            .skip_hidden(false)
            .follow_links(false)
            .process_read_dir(move |_, _, _, children| {
                if cancel_cb.load(Ordering::Relaxed) {
                    children.clear();
                    return;
                }
                children.retain(|c| {
                    c.as_ref()
                        .map(|e| !e.file_name.to_string_lossy().starts_with('.'))
                        .unwrap_or(false)
                });
                for e in children.iter_mut().flatten() {
                    if e.file_type.is_dir() && (skip_dir(&e.file_name.to_string_lossy()) || is_generated_dir(&e.path())) {
                        e.read_children = None;
                    }
                }
            });
        for entry in walker.into_iter().filter_map(Result::ok) {
            if !entry.file_type.is_file() {
                continue;
            }
            progress.fetch_add(1, Ordering::Relaxed);
            let Ok(md) = entry.metadata() else { continue };
            let size = md.len();
            // Skip small files, and cloud placeholders (no local data): reading
            // those would trigger a download.
            if size < min_bytes || md.blocks() == 0 {
                continue;
            }
            // Hard links are the same file twice, not a duplicate.
            if !seen_inodes.insert((md.dev(), md.ino())) {
                continue;
            }
            out.push(Entry {
                path: entry.path(),
                size,
                modified: md.modified().ok(),
            });
        }
    }
    out
}

fn hash_edges(path: &Path, size: u64) -> Option<blake3::Hash> {
    let mut f = File::open(path).ok()?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; EDGE as usize];
    let n = f.read(&mut buf).ok()?;
    h.update(&buf[..n]);
    if size > 2 * EDGE {
        f.seek(SeekFrom::End(-(EDGE as i64))).ok()?;
        let n = f.read(&mut buf).ok()?;
        h.update(&buf[..n]);
    }
    Some(h.finalize())
}

fn hash_full(path: &Path) -> Option<blake3::Hash> {
    let f = File::open(path).ok()?;
    let mut h = blake3::Hasher::new();
    h.update_reader(f).ok()?;
    Some(h.finalize())
}

/// Splits a group into sub-groups whose members share the same hash.
fn regroup(group: Vec<Entry>, progress: &AtomicUsize, hash: impl Fn(&Entry) -> Option<blake3::Hash> + Sync) -> Vec<Vec<Entry>> {
    let hashed: Vec<(Entry, blake3::Hash)> = group
        .into_par_iter()
        .filter_map(|e| {
            progress.fetch_add(1, Ordering::Relaxed);
            hash(&e).map(|h| (e, h))
        })
        .collect();
    let mut by_hash: HashMap<blake3::Hash, Vec<Entry>> = HashMap::new();
    for (e, h) in hashed {
        by_hash.entry(h).or_default().push(e);
    }
    by_hash.into_values().filter(|v| v.len() >= 2).collect()
}

fn find_duplicates(
    roots: &[PathBuf],
    min_bytes: u64,
    stage: &AtomicUsize,
    progress: &AtomicUsize,
    cancel: &Arc<AtomicBool>,
) -> Vec<DupGroup> {
    stage.store(0, Ordering::Relaxed);
    let files = list_files(roots, min_bytes, progress, cancel);
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }

    // Pass 1: same size.
    let mut by_size: HashMap<u64, Vec<Entry>> = HashMap::new();
    for f in files {
        by_size.entry(f.size).or_default().push(f);
    }
    let candidates: Vec<Vec<Entry>> = by_size.into_values().filter(|v| v.len() >= 2).collect();

    // Pass 2: same first/last 16 KB. Pass 3: same full content.
    stage.store(1, Ordering::Relaxed);
    progress.store(0, Ordering::Relaxed);
    let groups: Vec<Vec<Entry>> = candidates
        .into_par_iter()
        .flat_map(|g| {
            if cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let size = g[0].size;
            regroup(g, progress, |e| hash_edges(&e.path, size))
                .into_iter()
                .flat_map(|g| {
                    if size <= 2 * EDGE {
                        vec![g]
                    } else {
                        regroup(g, progress, |e| hash_full(&e.path))
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let mut out: Vec<DupGroup> = groups
        .into_iter()
        .map(|mut g| {
            g.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.path.cmp(&b.path)));
            DupGroup {
                size: g[0].size,
                kind: Kind::of(&g[0].path),
                files: g
                    .into_iter()
                    .map(|e| DupFile {
                        path: e.path,
                        modified: e.modified,
                        remove: false,
                    })
                    .collect(),
            }
        })
        .collect();
    out.sort_unstable_by(|a, b| b.wasted().cmp(&a.wasted()));
    out
}

/// Moves the selected copies to the Trash after re-checking that each one is
/// unchanged since the scan and that at least one copy in its group remains.
fn remove(plan: Vec<(u64, Vec<(PathBuf, Option<SystemTime>)>, Vec<PathBuf>)>) -> (usize, u64, Option<String>) {
    let mut paths = Vec::new();
    let mut freed = 0;
    let mut skipped = 0;
    for (size, to_remove, kept) in plan {
        let keeper_ok = kept
            .iter()
            .any(|k| std::fs::metadata(k).map(|m| m.len() == size).unwrap_or(false));
        if !keeper_ok {
            skipped += to_remove.len();
            continue;
        }
        for (p, modified) in to_remove {
            match std::fs::metadata(&p) {
                Ok(m) if m.len() == size && m.modified().ok() == modified => {
                    paths.push(p);
                    freed += size;
                }
                _ => skipped += 1,
            }
        }
    }
    let n = paths.len();
    let mut err = fsutil::move_to_trash(&paths).err();
    if skipped > 0 && err.is_none() {
        err = Some(format!("{skipped} file(s) changed since the scan and were left alone."));
    }
    (n, freed, err)
}

// ---------- UI ----------

impl Dupes {
    fn apply_keep(&mut self) {
        for g in &mut self.groups {
            let keep_idx = match self.keep {
                Keep::Oldest => 0,
                Keep::Newest => g.files.len() - 1,
            };
            for (i, f) in g.files.iter_mut().enumerate() {
                f.remove = i != keep_idx;
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.done = None;
        self.note.clear();
        self.focus = 0;
        let min = self.min_kb * 1024;
        let stage = self.state.stage.clone();
        self.scan = Some(Job::spawn(ctx, move |p, c| {
            find_duplicates(&scan_roots(), min, &stage, &p, &c)
        }));
    }

    pub fn ui(&mut self, ui: &mut Ui, p: Palette) {
        let ctx = ui.ctx().clone();
        if let Some(groups) = job::poll(&mut self.scan, &ctx) {
            self.groups = groups;
            self.scanned = true;
            self.apply_keep();
        }
        if let Some(res) = job::poll(&mut self.trash, &ctx) {
            self.done = Some(res);
            for g in &mut self.groups {
                g.files.retain(|f| f.path.exists());
            }
            self.groups.retain(|g| g.files.len() >= 2);
            self.focus = 0;
        }

        if let Some(job) = &self.scan {
            let (title, sub) = if self.state.stage.load(Ordering::Relaxed) == 0 {
                ("Listing your files…", format!("{} files checked", job.progress()))
            } else {
                ("Comparing file contents…", format!("{} candidates hashed", job.progress()))
            };
            busy_screen(ui, p, icon::COPY, title, &sub);
            theme::centered(ui, |ui| {
                ui.add_space(14.0);
                if theme::pill(ui, format!("{}  Stop", icon::STOP), Color32::from_white_alpha(40)).clicked() {
                    job.stop();
                }
            });
            return;
        }
        if self.trash.is_some() {
            busy_screen(
                ui,
                p,
                icon::TRASH,
                "Moving duplicates to the Trash…",
                "Finder may ask for your password.",
            );
            return;
        }
        if let Some((n, freed, err)) = &self.done {
            let sub = err
                .clone()
                .unwrap_or_else(|| "One copy of each file was kept. Empty the Trash to get the space back.".into());
            if done_screen(ui, p, &format!("{n} duplicates · {} moved to Trash", fmt_size(*freed)), &sub) {
                self.done = None;
            }
            return;
        }
        if !self.scanned {
            self.intro(ui, p, &ctx);
            return;
        }
        self.results(ui, p, &ctx);
    }

    fn intro(&mut self, ui: &mut Ui, p: Palette, ctx: &egui::Context) {
        theme::centered(ui, |ui| {
            ui.add_space(ui.available_height() * 0.08);
            theme::hero(ui, icon::COPY, p, 150.0, false, None);
            ui.add_space(10.0);
            ui.label(theme::bold("Duplicates", 30.0));
            ui.label(theme::dim(
                "Find identical copies of files in Desktop, Documents, Downloads,\nPictures, Movies and Music. Files are matched by content, not by name,\nand one copy is always kept.",
                15.0,
            ));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let w = 3.0 * 96.0;
                ui.add_space((ui.available_width() - w) / 2.0);
                for (kb, label) in [(100, "100 KB+"), (1024, "1 MB+"), (10 * 1024, "10 MB+")] {
                    let fill = if self.min_kb == kb {
                        Color32::from_white_alpha(80)
                    } else {
                        Color32::from_white_alpha(22)
                    };
                    if theme::pill(ui, label, fill).clicked() {
                        self.min_kb = kb;
                    }
                }
            });
            ui.add_space(16.0);
            if theme::round_button(ui, "Scan", p, true).clicked() {
                self.start(ctx);
            }
            ui.add_space(8.0);
            ui.label(theme::dim(
                format!(
                    "{}  Photos and Music libraries, app data and iCloud-only files are never touched.",
                    icon::LOCK_SIMPLE
                ),
                12.5,
            ));
        });
    }

    fn results(&mut self, ui: &mut Ui, p: Palette, ctx: &egui::Context) {
        let wasted: u64 = self.groups.iter().map(DupGroup::wasted).sum();
        let copies: usize = self.groups.iter().map(|g| g.files.len() - 1).sum();
        ui.horizontal(|ui| {
            theme::icon_tile(ui, icon::COPY, p.accent, 46.0);
            ui.vertical(|ui| {
                ui.label(theme::bold(fmt_size(wasted), 28.0));
                ui.label(theme::dim(
                    format!("taken up by {copies} extra copies in {} groups", self.groups.len()),
                    13.5,
                ));
            });
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::pill(
                    ui,
                    format!("{}  Scan again", icon::ARROW_CLOCKWISE),
                    Color32::from_white_alpha(35),
                )
                .clicked()
                {
                    self.start(ctx);
                }
                for (label, k) in [("Keep newest", Keep::Newest), ("Keep oldest", Keep::Oldest)] {
                    let fill = if self.keep == k {
                        Color32::from_white_alpha(70)
                    } else {
                        Color32::from_white_alpha(18)
                    };
                    if theme::pill(ui, label, fill).clicked() {
                        self.keep = k;
                        self.apply_keep();
                    }
                }
            });
        });
        ui.add_space(10.0);

        if self.groups.is_empty() {
            theme::centered(ui, |ui| {
                ui.add_space(80.0);
                ui.label(theme::ic(icon::CHECK_CIRCLE, 48.0));
                ui.label(theme::semi("No duplicates found", 18.0));
                ui.label(theme::dim("Your folders don't contain identical copies at this size.", 13.5));
            });
            return;
        }

        egui::Panel::bottom("dupes_action").frame(egui::Frame::new()).show(ui, |ui| {
            theme::centered(ui, |ui| {
                ui.add_space(4.0);
                let n: usize = self.groups.iter().map(|g| g.files.iter().filter(|f| f.remove).count()).sum();
                let bytes: u64 = self.groups.iter().map(DupGroup::selected_bytes).sum();
                if theme::round_button(ui, "Remove", p, n > 0).clicked() {
                    let plan = self
                        .groups
                        .iter()
                        .map(|g| {
                            let rm = g
                                .files
                                .iter()
                                .filter(|f| f.remove)
                                .map(|f| (f.path.clone(), f.modified))
                                .collect();
                            let kept = g.files.iter().filter(|f| !f.remove).map(|f| f.path.clone()).collect();
                            (g.size, rm, kept)
                        })
                        .collect();
                    self.trash = Some(Job::spawn(ctx, move |_, _| remove(plan)));
                }
                let msg = if self.note.is_empty() {
                    format!("{n} duplicates · {} · moved to Trash", fmt_size(bytes))
                } else {
                    self.note.clone()
                };
                ui.label(theme::dim(msg, 12.5));
            });
        });

        // Group list
        egui::Panel::left("dupes_groups")
            .resizable(false)
            .exact_size(360.0)
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let row_h = 52.0;
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show_rows(ui, row_h, self.groups.len(), |ui, range| {
                        for i in range {
                            let g = &self.groups[i];
                            let (name, count, wasted, kind) = (g.name(), g.files.len(), g.wasted(), g.kind);
                            let r = theme::row(ui, row_h - 6.0, self.focus == i, |ui| {
                                theme::icon_tile(ui, kind.glyph(), kind.color(), 30.0);
                                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(theme::semi(fmt_size(wasted), 13.0));
                                    ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
                                        ui.add_space(6.0);
                                        ui.add(egui::Label::new(theme::semi(name, 13.5)).truncate());
                                        ui.label(theme::dim(format!("{count} copies"), 11.5));
                                    });
                                });
                            });
                            if r.clicked() {
                                self.focus = i;
                                self.note.clear();
                            }
                        }
                    });
            });

        // Group detail
        let focus = self.focus.min(self.groups.len() - 1);
        let g = &mut self.groups[focus];
        let mut blocked = false;
        theme::card(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                theme::icon_tile(ui, g.kind.glyph(), g.kind.color(), 42.0);
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(theme::bold(g.name(), 19.0)).truncate());
                    ui.label(theme::dim(
                        format!("{} copies of {} each · identical contents", g.files.len(), fmt_size(g.size)),
                        12.5,
                    ));
                });
            });
            ui.add_space(4.0);
            ui.label(theme::dim(
                "Matched by content, not by name. Preview a copy before removing it. Ticked copies go to the Trash; one is always kept.",
                12.0,
            ));
            ui.separator();
            let all_but_one = g.files.iter().filter(|f| f.remove).count() + 1 >= g.files.len();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for f in &mut g.files {
                    ui.horizontal(|ui| {
                        let was = f.remove;
                        ui.checkbox(&mut f.remove, "");
                        if f.remove && !was && all_but_one {
                            f.remove = false; // would remove every copy
                            blocked = true;
                        }
                        let (tag, color) = if f.remove {
                            ("Remove", theme::DANGER)
                        } else {
                            ("Keep", Color32::from_rgb(0x34, 0xd9, 0x9a))
                        };
                        theme::badge(ui, tag, color);
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(6.0);
                            ui.label(theme::dim(fmt_age(f.modified), 12.5));
                            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(egui::Label::new(theme::text(fsutil::pretty(&f.path), 13.0)).truncate())
                                    .on_hover_text(f.path.display().to_string());
                            });
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(30.0);
                        if theme::mini_button(
                            ui,
                            format!("{}  Preview", icon::EYE),
                            "Quick Look preview, like pressing Space in Finder",
                        )
                        .clicked()
                        {
                            fsutil::quick_look(&f.path);
                        }
                        if theme::mini_button(
                            ui,
                            format!("{}  Show in Finder", icon::FOLDER_OPEN),
                            "Open the folder with this file selected",
                        )
                        .clicked()
                        {
                            fsutil::reveal(&f.path);
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                }
            });
        });
        if blocked {
            self.note = "At least one copy of each file is always kept.".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("macsweep-dupes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, data: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    fn scan(root: &Path, min: u64) -> Vec<DupGroup> {
        let a = AtomicUsize::new(0);
        find_duplicates(
            &[root.to_path_buf()],
            min,
            &a,
            &AtomicUsize::new(0),
            &Arc::new(AtomicBool::new(false)),
        )
    }

    fn noise(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn finds_identical_files_in_different_folders() {
        let d = tmp("ident");
        let data = noise(100_000, 1);
        write(&d.join("Downloads/a.bin"), &data);
        write(&d.join("Documents/copy of a.bin"), &data);
        write(&d.join("Documents/other.bin"), &noise(100_000, 2));
        let g = scan(&d, 1);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].files.len(), 2);
        assert_eq!(g[0].size, 100_000);
    }

    #[test]
    fn same_edges_but_different_middle_is_not_a_duplicate() {
        let d = tmp("middle");
        let a = noise(200_000, 3);
        let mut b = a.clone();
        b[100_000] ^= 0xff; // differs only in the middle, past the 16 KB edges
        write(&d.join("x/a.bin"), &a);
        write(&d.join("y/b.bin"), &b);
        assert!(scan(&d, 1).is_empty());
    }

    #[test]
    fn hard_links_are_not_duplicates() {
        let d = tmp("hardlink");
        write(&d.join("a.bin"), &noise(50_000, 4));
        fs::hard_link(d.join("a.bin"), d.join("b.bin")).unwrap();
        assert!(scan(&d, 1).is_empty());
    }

    #[test]
    fn libraries_dev_folders_and_hidden_files_are_ignored() {
        let d = tmp("ignored");
        let data = noise(60_000, 5);
        write(&d.join("Pictures/Photos Library.photoslibrary/originals/a.jpg"), &data);
        write(&d.join("Pictures/Photos Library.photoslibrary/resources/a.jpg"), &data);
        write(&d.join("code/node_modules/x/lib.js"), &data);
        write(&d.join("code/node_modules/y/lib.js"), &data);
        write(&d.join(".secret/a"), &data);
        write(&d.join("Music/.hidden-copy"), &data);
        write(&d.join("Library/Caches/a"), &data);
        write(&d.join("Library/Caches/b"), &data);
        assert!(
            scan(&d, 1).is_empty(),
            "nothing in packages, dev or hidden folders may be listed"
        );
    }

    #[test]
    fn build_and_virtualenv_folders_are_ignored() {
        let d = tmp("generated");
        let data = noise(40_000, 9);
        write(
            &d.join("proj/target/CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55",
        );
        write(&d.join("proj/target/debug/deps/libfoo-1.rmeta"), &data);
        write(&d.join("proj/target/release/deps/libfoo-2.rmeta"), &data);
        write(&d.join("ml/env/pyvenv.cfg"), b"home = /usr/bin");
        write(&d.join("ml/env/lib/a.so"), &data);
        write(&d.join("ml/env/lib/b.so"), &data);
        assert!(scan(&d, 1).is_empty());
    }

    #[test]
    fn same_name_with_different_content_is_not_grouped() {
        let d = tmp("samename");
        write(&d.join("v1/thesis.pdf"), &noise(30_000, 10));
        write(&d.join("v2/thesis.pdf"), &noise(30_000, 11));
        assert!(scan(&d, 1).is_empty());
    }

    #[test]
    fn files_below_the_minimum_size_are_ignored() {
        let d = tmp("small");
        write(&d.join("a.txt"), &noise(500, 6));
        write(&d.join("b.txt"), &noise(500, 6));
        assert!(scan(&d, 1024).is_empty());
        assert_eq!(scan(&d, 1).len(), 1);
    }

    #[test]
    fn removal_is_refused_when_the_kept_copy_is_gone() {
        let d = tmp("keeper");
        write(&d.join("dup.bin"), &noise(10_000, 7));
        let plan = vec![(
            10_000,
            vec![(d.join("dup.bin"), fs::metadata(d.join("dup.bin")).unwrap().modified().ok())],
            vec![d.join("missing.bin")],
        )];
        let (n, freed, err) = remove(plan);
        assert_eq!((n, freed), (0, 0));
        assert!(err.is_some());
        assert!(d.join("dup.bin").exists(), "the last remaining copy must never be removed");
    }

    #[test]
    fn removal_skips_files_changed_since_the_scan() {
        let d = tmp("changed");
        write(&d.join("keep.bin"), &noise(10_000, 8));
        write(&d.join("dup.bin"), &noise(10_000, 8));
        let old = Some(SystemTime::UNIX_EPOCH); // pretend the scan saw an older version
        let plan = vec![(10_000, vec![(d.join("dup.bin"), old)], vec![d.join("keep.bin")])];
        let (n, _, err) = remove(plan);
        assert_eq!(n, 0);
        assert!(err.is_some());
        assert!(d.join("dup.bin").exists());
    }
}
