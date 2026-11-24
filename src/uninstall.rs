//! App Uninstaller: every app with its real icon, details and leftovers.
use crate::fsutil::{self, fmt_size};
use crate::job::{self, Job};
use crate::junk::{busy_screen, done_screen};
use crate::running_app::quit_bundles;
use crate::theme::{self, Palette, icon};
use eframe::egui::{self, Color32, ColorImage, Layout, TextureHandle, TextureOptions, Ui};
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const ICON_PX: usize = 64;

#[derive(Clone, Copy, PartialEq)]
enum Arch {
    Universal,
    AppleSilicon,
    Intel,
    Unknown,
}

#[derive(Clone)]
struct Leftover {
    path: PathBuf,
    size: u64,
    kind: &'static str,
    /// Profiles, accounts, saved documents, logins… kept unless the user ticks it.
    user_data: bool,
    selected: bool,
}

#[derive(Clone)]
pub struct AppInfo {
    path: PathBuf,
    name: String,
    version: Option<String>,
    bundle_id: Option<String>,
    size: u64,
    arch: Arch,
    app_store: bool,
    icon_file: Option<String>,
    /// The .app bundle the icon and metadata come from (the main app of a package).
    icon_app: PathBuf,
    /// Actual bundles to quit before removal, including the apps inside a package.
    members: Vec<PathBuf>,
    /// Number of apps inside, when this entry is an install folder.
    package_apps: usize,
    /// A shortcut whose app no longer exists.
    broken: bool,
    leftovers: Vec<Leftover>,
    checked: bool,
}

impl AppInfo {
    fn leftovers_selected(&self) -> u64 {
        self.leftovers.iter().filter(|l| l.selected).map(|l| l.size).sum()
    }
    fn total(&self) -> u64 {
        self.size + self.leftovers_selected()
    }
}

#[derive(Default)]
pub struct Uninstaller {
    apps: Vec<AppInfo>,
    loaded: bool,
    query: String,
    sort_size: bool,
    focus: Option<PathBuf>,
    icons: HashMap<PathBuf, TextureHandle>,
    last_opened: HashMap<PathBuf, String>,
    scan: Option<Job<Vec<AppInfo>>>,
    icon_job: Option<Job<Vec<(PathBuf, ColorImage)>>>,
    opened_job: Option<Job<(PathBuf, String)>>,
    remove: Option<Job<(usize, u64, Option<String>)>>,
    done: Option<(usize, u64, Option<String>)>,
    /// The selection the user is confirming; any change cancels it.
    confirm_for: Option<Vec<(PathBuf, usize)>>,
}

// ---------- discovery ----------

fn plist_str(d: &plist::Dictionary, key: &str) -> Option<String> {
    d.get(key).and_then(|v| v.as_string()).map(str::to_owned)
}

/// Reads the Mach-O header of the main executable.
fn detect_arch(app: &Path, exe: Option<&str>) -> Arch {
    let Some(exe) = exe else { return Arch::Unknown };
    let Ok(mut f) = std::fs::File::open(app.join("Contents/MacOS").join(exe)) else {
        return Arch::Unknown;
    };
    let mut buf = [0u8; 4096];
    let n = f.read(&mut buf).unwrap_or(0);
    if n < 8 {
        return Arch::Unknown;
    }
    let be = |o: usize| u32::from_be_bytes(buf[o..o + 4].try_into().unwrap());
    let le = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
    const ARM64: u32 = 0x0100_000C;
    const X86_64: u32 = 0x0100_0007;
    let (mut arm, mut intel) = (false, false);
    match be(0) {
        0xCAFE_BABE | 0xCAFE_BABF => {
            let entry = if be(0) == 0xCAFE_BABE { 20 } else { 32 };
            let count = be(4).min(16) as usize;
            for i in 0..count {
                let o = 8 + i * entry;
                if o + 4 > n {
                    break;
                }
                match be(o) {
                    ARM64 => arm = true,
                    X86_64 => intel = true,
                    _ => {}
                }
            }
        }
        _ if le(0) == 0xFEED_FACF => match le(4) {
            ARM64 => arm = true,
            X86_64 => intel = true,
            _ => {}
        },
        _ => {}
    }
    match (arm, intel) {
        (true, true) => Arch::Universal,
        (true, false) => Arch::AppleSilicon,
        (false, true) => Arch::Intel,
        _ => Arch::Unknown,
    }
}

/// (folder in ~/Library, label, holds user data?)
/// User-data folders (browser profiles, mailboxes, logins, saved content)
/// are never pre-selected: removing them is always the user's explicit choice.
const LIB_DIRS: [(&str, &str, bool); 13] = [
    ("Application Support", "App Support", true),
    ("Caches", "Cache", false),
    ("Preferences", "Preferences", false),
    ("Preferences/ByHost", "Preferences", false),
    ("Containers", "Container", true),
    ("Group Containers", "Group Container", true),
    ("Saved Application State", "Saved State", false),
    ("Logs", "Logs", false),
    ("HTTPStorages", "Web Data", true),
    ("WebKit", "Web Data", true),
    ("Cookies", "Cookies", true),
    ("LaunchAgents", "Launch Agent", false),
    ("Application Scripts", "Scripts", false),
];

/// System-wide folders (in /Library, for all users). Items here are matched
/// by bundle ID only. Finder controls permission to move them.
const SYS_DIRS: [(&str, &str, bool); 7] = [
    ("Application Support", "App Support · all users", true),
    ("Caches", "Cache · all users", false),
    ("Preferences", "Preferences · all users", false),
    ("Logs", "Logs · all users", false),
    ("LaunchAgents", "Launch Agent · all users", false),
    ("LaunchDaemons", "Background service", false),
    ("PrivilegedHelperTools", "Helper tool", false),
];

struct LibItem {
    path: PathBuf,
    name: String,
    kind: &'static str,
    data: bool,
    /// In /Library rather than ~/Library.
    system: bool,
}

/// One listing of ~/Library and /Library, shared by every app's leftover search.
fn library_index() -> Vec<LibItem> {
    let list = |root: PathBuf, dirs: &'static [(&'static str, &'static str, bool)], system: bool| {
        dirs.iter()
            .flat_map(move |(dir, kind, data)| {
                fsutil::children(&root.join(dir)).into_iter().map(move |p| LibItem {
                    name: p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default(),
                    path: p,
                    kind,
                    data: *data,
                    system,
                })
            })
            .collect::<Vec<_>>()
    };
    let mut v = list(fsutil::home().join("Library"), &LIB_DIRS, false);
    v.extend(list(PathBuf::from("/Library"), &SYS_DIRS, true));
    v
}

fn is_app(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "app")
}

/// How a folder inside /Applications should be listed.
#[derive(Debug, PartialEq)]
enum Folder {
    /// An install folder (app + its own libraries, helpers or resources),
    /// e.g. "Cisco Packet Tracer 8.2.2". Listed and removed as one unit.
    Package(Vec<PathBuf>),
    /// A plain folder of separate apps (e.g. one the user made). Listed per app.
    Apps(Vec<PathBuf>),
    None,
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The app named like its folder ("Cisco Packet Tracer 8.2.2" → "Cisco Packet Tracer 8.2.2.app").
fn named_like(folder: &Path, apps: &[PathBuf]) -> Option<PathBuf> {
    let fname = norm(&folder.file_name().unwrap_or_default().to_string_lossy());
    apps.iter()
        .find(|a| {
            let n = norm(&a.file_stem().unwrap_or_default().to_string_lossy());
            !n.is_empty() && !fname.is_empty() && (fname.starts_with(&n) || n.starts_with(&fname))
        })
        .cloned()
}

/// Folders macOS (or the user) uses to group many unrelated apps. They are
/// never removed as one unit.
const SHARED_FOLDERS: &[&str] = &["utilities", "games", "tools", "apps", "applications", "other", "developer"];

fn classify(dir: &Path) -> Folder {
    let entries: Vec<PathBuf> = fsutil::children(dir)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| !n.to_string_lossy().starts_with('.')))
        .filter(|p| p.extension().is_none_or(|e| e != "localized"))
        .collect();
    let apps: Vec<PathBuf> = entries.iter().filter(|p| is_app(p)).cloned().collect();
    let dir_name = dir.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    if apps.is_empty() {
        Folder::None
    } else if entries.len() > apps.len() && !SHARED_FOLDERS.contains(&dir_name.as_str()) && named_like(dir, &apps).is_some() {
        // Only an install folder whose main app carries the folder's name is
        // treated as one unit; anything else is listed app by app, so a
        // folder of yours is never removed as a whole.
        Folder::Package(apps)
    } else {
        Folder::Apps(apps)
    }
}

/// The package's main app: the one named like the folder, else the largest.
fn main_app(folder: &Path, apps: &[PathBuf]) -> PathBuf {
    named_like(folder, apps).unwrap_or_else(|| apps.iter().max_by_key(|a| fsutil::size_of(a)).cloned().unwrap_or_default())
}

struct Bundle {
    id: Option<String>,
    name: Option<String>,
    exe: Option<String>,
    version: Option<String>,
    icon_file: Option<String>,
}

fn read_bundle(app: &Path) -> Bundle {
    let real = std::fs::canonicalize(app).unwrap_or_else(|_| app.to_path_buf());
    let info = plist::Value::from_file(real.join("Contents/Info.plist")).ok();
    let d = info.as_ref().and_then(|v| v.as_dictionary());
    Bundle {
        id: d.and_then(|d| plist_str(d, "CFBundleIdentifier")),
        name: d.and_then(|d| plist_str(d, "CFBundleName")),
        exe: d.and_then(|d| plist_str(d, "CFBundleExecutable")),
        version: d.and_then(|d| plist_str(d, "CFBundleShortVersionString")),
        icon_file: d.and_then(|d| plist_str(d, "CFBundleIconFile")),
    }
}

/// (entry path, apps inside if it is a package)
fn discover() -> Vec<(PathBuf, Option<Vec<PathBuf>>)> {
    let mut out = Vec::new();
    for root in [PathBuf::from("/Applications"), fsutil::home().join("Applications")] {
        discover_in(&root, 0, &mut out);
    }
    out
}

fn discover_in(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, Option<Vec<PathBuf>>)>) {
    for c in fsutil::children(dir) {
        if is_app(&c) {
            out.push((c, None));
        } else if c.is_dir() && !fsutil::is_symlink(&c) {
            match classify(&c) {
                Folder::Package(apps) => out.push((c, Some(apps))),
                Folder::Apps(apps) => {
                    out.extend(apps.into_iter().map(|a| (a, None)));
                    // Shared folders like Utilities can hold install folders too.
                    if depth == 0 {
                        for sub in fsutil::children(&c).into_iter().filter(|p| p.is_dir() && !is_app(p)) {
                            if let Folder::Package(apps) = classify(&sub) {
                                out.push((sub, Some(apps)));
                            }
                        }
                    }
                }
                Folder::None if depth == 0 => discover_in(&c, 1, out),
                Folder::None => {}
            }
        }
    }
}

/// True if `name` contains the bundle ID `id` as whole dot-separated parts:
/// "com.foo.app.plist" and "group.com.foo.app" match "com.foo.app",
/// "com.foo.apple" does not.
fn id_in(name: &str, id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    name.match_indices(id).any(|(i, _)| {
        let before = i == 0 || name.as_bytes()[i - 1] == b'.';
        let end = i + id.len();
        let after = end == name.len() || name.as_bytes()[end] == b'.';
        before && after
    })
}

/// Does this ~/Library item belong to the app with bundle ID `id`?
/// A longer ID of another installed app wins: "com.google.chrome.canary.plist"
/// belongs to Chrome Canary, not to Chrome.
fn belongs_to(name: &str, id: &str, all_ids: &[String]) -> bool {
    id_in(name, id)
        && !all_ids
            .iter()
            .any(|o| o.len() > id.len() && o.starts_with(id) && o.as_bytes()[id.len()] == b'.' && id_in(name, o))
}

fn find_apps() -> Vec<AppInfo> {
    let index = library_index();
    let found = discover();
    let all_ids: Vec<String> = found
        .par_iter()
        .flat_map_iter(|(p, pkg)| match pkg {
            Some(inner) => inner.clone(),
            None => vec![p.clone()],
        })
        .filter_map(|a| read_bundle(&a).id.map(|i| i.to_lowercase()))
        .collect();
    let mut apps: Vec<AppInfo> = found
        .into_par_iter()
        .filter_map(|(path, package)| {
            let main = match &package {
                Some(inner) => main_app(&path, inner),
                None => path.clone(),
            };
            let real = std::fs::canonicalize(&main).unwrap_or_else(|_| main.clone());
            let b = read_bundle(&main);
            if b.id.as_deref().is_some_and(|b| b.starts_with("com.apple.")) {
                return None; // never touch Apple's apps
            }
            // A folder name like "Packet Tracer 8.2.2" has no extension to strip.
            let name = if package.is_some() {
                path.file_name()
            } else {
                path.file_stem()
            }?
            .to_string_lossy()
            .into_owned();
            let main_name = main.file_stem()?.to_string_lossy().into_owned();
            let members = package.clone().unwrap_or_else(|| vec![main.to_path_buf()]);

            // Leftovers: match the main app's bundle ID or exact name (and the folder name).
            let mut names = vec![main_name.to_lowercase(), main_name.to_lowercase().replace(' ', "")];
            if package.is_some() {
                names.push(name.to_lowercase());
            }
            if let Some(bn) = &b.name {
                names.push(bn.to_lowercase());
            }
            let bid = b.id.as_ref().map(|b| b.to_lowercase());
            let leftovers: Vec<Leftover> = index
                .iter()
                .filter(|it| {
                    let n = &it.name;
                    let by_id = bid.as_ref().is_some_and(|b| belongs_to(n, b, &all_ids));
                    if it.system {
                        return by_id; // never guess by name outside your own Library
                    }
                    let stem = Path::new(n)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    by_id || names.iter().any(|x| x == n || *x == stem)
                })
                .map(|it| Leftover {
                    path: it.path.clone(),
                    size: fsutil::size_of(&it.path),
                    kind: it.kind,
                    user_data: it.data,
                    selected: !it.data,
                })
                .collect();

            Some(AppInfo {
                version: b.version,
                icon_file: b.icon_file,
                arch: detect_arch(&real, b.exe.as_deref()),
                app_store: real.join("Contents/_MASReceipt").exists(),
                size: if package.is_some() {
                    fsutil::size_of(&path)
                } else {
                    fsutil::app_size(&path)
                },
                bundle_id: b.id,
                package_apps: package.as_ref().map_or(0, Vec::len),
                broken: fsutil::is_broken_link(&path),
                icon_app: main,
                members,
                name,
                path,
                leftovers,
                checked: false,
            })
        })
        .collect();
    for a in &mut apps {
        a.leftovers.sort_unstable_by(|x, y| y.size.cmp(&x.size));
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

// ---------- icons ----------

/// Decodes the app's .icns into a small 64×64 RGBA image.
pub fn load_icon(app: &Path, icon_file: Option<&str>) -> Option<ColorImage> {
    let real = std::fs::canonicalize(app).ok()?;
    let mut name = icon_file.unwrap_or("AppIcon").to_string();
    if !name.ends_with(".icns") {
        name.push_str(".icns");
    }
    let file = std::fs::File::open(real.join("Contents/Resources").join(name)).ok()?;
    let family = icns::IconFamily::read(std::io::BufReader::new(file)).ok()?;
    let mut types = family.available_icons();
    // Smallest icon that is still >= 64px first, then bigger ones, then smaller.
    types.sort_by_key(|t| {
        let w = t.pixel_width();
        if w >= ICON_PX as u32 { w } else { 100_000 - w }
    });
    for t in types {
        let Ok(img) = family.get_icon_with_type(t) else { continue };
        let img = img.convert_to(icns::PixelFormat::RGBA);
        return Some(downscale(img.data(), img.width() as usize, img.height() as usize));
    }
    None
}

/// Newer apps keep their icon in an asset catalog instead of an .icns file.
/// For those, ask macOS (Quick Look) to render the icon Finder shows.
pub fn system_icon(app: &Path) -> Option<ColorImage> {
    let dir = std::env::temp_dir().join(format!("macsweep-icons-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    Command::new("/usr/bin/qlmanage")
        .args(["-t", "-s", "128", "-o"])
        .arg(&dir)
        .arg(app)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    let file = dir.join(format!("{}.png", app.file_name()?.to_string_lossy()));
    let img = decode_png(&file);
    let _ = std::fs::remove_file(&file);
    img
}

fn decode_png(path: &Path) -> Option<ColorImage> {
    // App icons go up to 1024 px (512 pt @2x).
    decode_png_from(std::io::BufReader::new(std::fs::File::open(path).ok()?), 1024)
}

/// A PNG held in memory (e.g. a website's icon from the browser's database).
/// Website icons are small (MacSweep asks for 16–128 px); anything claiming
/// to be bigger than 256 px is refused before any memory is set aside.
pub fn decode_png_bytes(bytes: &[u8]) -> Option<ColorImage> {
    decode_png_from(std::io::Cursor::new(bytes), 256)
}

/// Decodes a PNG no wider or taller than `max_side` pixels. The size the
/// file claims is checked before decoding, so a file claiming to be huge
/// can't make MacSweep set aside lots of memory.
fn decode_png_from<R: std::io::BufRead + std::io::Seek>(reader: R, max_side: u32) -> Option<ColorImage> {
    let mut dec = png::Decoder::new_with_limits(reader, png::Limits { bytes: 16 * 1024 * 1024 });
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let info = r.info();
    if info.width == 0 || info.height == 0 || info.width > max_side || info.height > max_side {
        return None;
    }
    let mut buf = vec![0u8; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => px.chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    (rgba.len() == w * h * 4).then(|| downscale(&rgba, w, h))
}

/// Box-filter resize to ICON_PX with alpha weighting (keeps edges clean).
fn downscale(src: &[u8], w: usize, h: usize) -> ColorImage {
    let mut out = vec![0u8; ICON_PX * ICON_PX * 4];
    for oy in 0..ICON_PX {
        for ox in 0..ICON_PX {
            let (x0, x1) = (ox * w / ICON_PX, ((ox + 1) * w / ICON_PX).max(ox * w / ICON_PX + 1).min(w));
            let (y0, y1) = (oy * h / ICON_PX, ((oy + 1) * h / ICON_PX).max(oy * h / ICON_PX + 1).min(h));
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    let al = src[i + 3] as u64;
                    r += src[i] as u64 * al;
                    g += src[i + 1] as u64 * al;
                    b += src[i + 2] as u64 * al;
                    a += al;
                    n += 1;
                }
            }
            let o = (oy * ICON_PX + ox) * 4;
            if a > 0 {
                out[o] = (r / a) as u8;
                out[o + 1] = (g / a) as u8;
                out[o + 2] = (b / a) as u8;
                out[o + 3] = (a / n.max(1)) as u8;
            }
        }
    }
    ColorImage::from_rgba_unmultiplied([ICON_PX, ICON_PX], &out)
}

fn last_opened(app: &Path) -> String {
    let out = Command::new("/usr/bin/mdls")
        .args(["-raw", "-name", "kMDItemLastUsedDate"])
        .arg(app)
        .output();
    let s = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if s.is_empty() || s == "(null)" {
        "Never / unknown".into()
    } else {
        s.chars().take(10).collect()
    }
}

/// Quits each app if running, then moves app + selected leftovers to the Trash.
fn uninstall(apps: Vec<AppInfo>) -> (usize, u64, Option<String>) {
    let mut paths = Vec::new();
    let mut refused = Vec::new();
    for a in &apps {
        if let Err(reason) = quit_bundles(&a.members) {
            crate::safety_log::event(true, "Skipped an uninstall after the running application check");
            refused.push(format!("{}: {reason}", a.name));
            continue;
        }
        paths.push(a.path.clone());
        paths.extend(a.leftovers.iter().filter(|l| l.selected).map(|l| l.path.clone()));
        // Stop the app's own background agents first, so they don't keep running.
        let agents = fsutil::home().join("Library/LaunchAgents");
        let uid = unsafe { libc::getuid() };
        for l in a.leftovers.iter().filter(|l| l.selected && l.path.starts_with(&agents)) {
            let _ = Command::new("/bin/launchctl")
                .args(["bootout", &format!("gui/{uid}")])
                .arg(&l.path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    let mut err = fsutil::move_to_trash(&paths).err();
    // Count only what is really gone.
    let removed: Vec<&AppInfo> = apps.iter().filter(|a| !fsutil::lexists(&a.path)).collect();
    let n = removed.len();
    // Support files only count if they really left too.
    let freed: u64 = removed
        .iter()
        .map(|a| {
            a.size
                + a.leftovers
                    .iter()
                    .filter(|l| l.selected && !fsutil::lexists(&l.path))
                    .map(|l| l.size)
                    .sum::<u64>()
        })
        .sum();
    if !refused.is_empty() {
        refused.extend(err);
        err = Some(refused.join("\n"));
    }
    (n, freed, err)
}

// ---------- UI ----------

fn arch_badge(ui: &mut Ui, arch: Arch) {
    match arch {
        Arch::Universal => theme::badge(ui, "Universal", Color32::from_rgb(0x5a, 0xa8, 0xff)),
        Arch::AppleSilicon => theme::badge(ui, "Apple Silicon", Color32::from_rgb(0x34, 0xd9, 0x9a)),
        Arch::Intel => theme::badge(ui, "Intel · runs via Rosetta", theme::WARN),
        Arch::Unknown => {}
    }
}

fn letter_color(name: &str) -> Color32 {
    let h = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let palette = [
        (0x5a, 0x8d, 0xee),
        (0xe0, 0x5c, 0x8a),
        (0x34, 0xb5, 0x8a),
        (0xe0, 0x8a, 0x34),
        (0x8a, 0x5c, 0xe6),
        (0x2b, 0x9c, 0xc4),
    ];
    let (r, g, b) = palette[h as usize % palette.len()];
    Color32::from_rgb(r, g, b)
}

impl Uninstaller {
    fn rescan(&mut self, ctx: &egui::Context) {
        self.scan = Some(Job::spawn(ctx, |_, _| find_apps()));
    }

    fn app_icon(&self, ui: &mut Ui, app: &AppInfo, size: f32) {
        if let Some(tex) = self.icons.get(&app.path) {
            ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(size, size)));
        } else {
            let letter = app.name.chars().next().unwrap_or('?').to_uppercase().to_string();
            theme::icon_tile(ui, &letter, letter_color(&app.name), size);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: Palette) {
        let ctx = ui.ctx().clone();
        if !self.loaded {
            self.loaded = true;
            self.rescan(&ctx);
        }
        if let Some(apps) = job::poll(&mut self.scan, &ctx) {
            // keep the user's checkmarks across rescans
            let checked: Vec<PathBuf> = self.apps.iter().filter(|a| a.checked).map(|a| a.path.clone()).collect();
            self.apps = apps;
            for a in &mut self.apps {
                a.checked = checked.contains(&a.path);
            }
            let need: Vec<(PathBuf, PathBuf, Option<String>)> = self
                .apps
                .iter()
                .filter(|a| !self.icons.contains_key(&a.path))
                .map(|a| (a.path.clone(), a.icon_app.clone(), a.icon_file.clone()))
                .collect();
            if !need.is_empty() {
                self.icon_job = Some(Job::spawn(&ctx, move |_, _| {
                    need.into_par_iter()
                        .filter_map(|(p, app, f)| {
                            load_icon(&app, f.as_deref())
                                .or_else(|| system_icon(&app))
                                .map(|img| (p, img))
                        })
                        .collect()
                }));
            }
            if self.focus.as_ref().is_none_or(|f| !self.apps.iter().any(|a| &a.path == f)) {
                self.focus = None;
            }
        }
        if let Some(icons) = job::poll(&mut self.icon_job, &ctx) {
            for (path, img) in icons {
                let tex = ctx.load_texture(path.to_string_lossy(), img, TextureOptions::LINEAR);
                self.icons.insert(path, tex);
            }
        }
        if let Some((path, s)) = job::poll(&mut self.opened_job, &ctx) {
            self.last_opened.insert(path, s);
        }
        if let Some(res) = job::poll(&mut self.remove, &ctx) {
            self.done = Some(res);
            self.focus = None;
            self.rescan(&ctx);
        }

        if self.remove.is_some() {
            busy_screen(
                ui,
                p,
                icon::PACKAGE,
                "Uninstalling…",
                "Quitting apps and moving them to the Trash.",
            );
            return;
        }
        if let Some((n, freed, err)) = &self.done {
            let sub = err
                .clone()
                .unwrap_or_else(|| "Empty the Trash (bottom-left) to get the space back.".into());
            let title = if *n == 0 {
                "Nothing was removed".to_string()
            } else {
                format!("{n} app{} removed · {}", if *n == 1 { "" } else { "s" }, fmt_size(*freed))
            };
            if done_screen(ui, p, &title, &sub) {
                self.done = None;
            }
            return;
        }
        if self.apps.is_empty() {
            if self.scan.is_some() {
                busy_screen(
                    ui,
                    p,
                    icon::PACKAGE,
                    "Measuring your apps…",
                    "Finding every app and the files it left around.",
                );
            } else {
                theme::centered(ui, |ui| {
                    ui.add_space(120.0);
                    ui.label(theme::dim("No apps found.", 15.0));
                });
            }
            return;
        }

        self.header(ui, &ctx);
        ui.add_space(10.0);
        self.bottom(ui, p, &ctx);
        self.list(ui, &ctx);
        self.detail(ui, &ctx);
    }

    fn header(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let total: u64 = self
            .apps
            .iter()
            .map(|a| a.size + a.leftovers.iter().map(|l| l.size).sum::<u64>())
            .sum();
        ui.horizontal(|ui| {
            theme::icon_tile(ui, icon::PACKAGE, theme::APPS.accent, 46.0);
            ui.vertical(|ui| {
                ui.label(theme::bold(theme::plural(self.apps.len(), "app"), 28.0));
                ui.label(theme::dim(format!("{} including their support files", fmt_size(total)), 13.5));
            });
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.scan.is_some();
                let label = if busy {
                    "Refreshing…".to_string()
                } else {
                    format!("{}  Refresh", icon::ARROW_CLOCKWISE)
                };
                if theme::pill(ui, label, Color32::from_white_alpha(35)).clicked() && !busy {
                    self.rescan(ctx);
                }
                for (label, size) in [("Size", true), ("Name", false)] {
                    let fill = if self.sort_size == size {
                        Color32::from_white_alpha(70)
                    } else {
                        Color32::from_white_alpha(18)
                    };
                    if theme::pill(ui, label, fill).clicked() {
                        self.sort_size = size;
                    }
                }
                ui.label(theme::dim("Sort", 12.5));
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search apps")
                        .desired_width(180.0)
                        .margin(egui::vec2(10.0, 6.0)),
                );
            });
        });
    }

    fn bottom(&mut self, ui: &mut Ui, p: Palette, ctx: &egui::Context) {
        egui::Panel::bottom("apps_action")
            .frame(theme::bottom_frame())
            .show_separator_line(false)
            .show(ui, |ui| {
                theme::centered(ui, |ui| {
                    ui.add_space(4.0);
                    let chosen: Vec<AppInfo> = self.apps.iter().filter(|a| a.checked).cloned().collect();
                    let size: u64 = chosen.iter().map(AppInfo::total).sum();
                    let n = chosen.len();
                    let key: Vec<(PathBuf, usize)> = chosen
                        .iter()
                        .map(|a| (a.path.clone(), a.leftovers.iter().filter(|l| l.selected).count()))
                        .collect();
                    if self.confirm_for.as_ref().is_some_and(|k| *k != key) {
                        self.confirm_for = None;
                    }
                    if self.confirm_for.is_some() {
                        let names: Vec<&str> = chosen.iter().map(|a| a.name.as_str()).collect();
                        let list = match names.len() {
                            0..=3 => names.join(", "),
                            k => format!("{} and {} more", names[..3].join(", "), k - 3),
                        };
                        let with_data: Vec<&str> = chosen
                            .iter()
                            .filter(|a| a.leftovers.iter().any(|l| l.selected && l.user_data))
                            .map(|a| a.name.as_str())
                            .collect();
                        let warn = (!with_data.is_empty()).then(|| {
                            format!(
                                "Your data for {} is ticked too (profiles, accounts, saved content).",
                                with_data.join(", ")
                            )
                        });
                        match theme::confirm(
                            ui,
                            &format!("Uninstall {} ({})?", theme::plural(n, "app"), fmt_size(size)),
                            &format!("{list} will be quit and moved to the Trash with the ticked support files."),
                            warn.as_deref(),
                            &format!("{}  Uninstall", icon::TRASH),
                        ) {
                            Some(true) => {
                                self.confirm_for = None;
                                self.remove = Some(Job::spawn(ctx, move |_, _| uninstall(chosen)));
                            }
                            Some(false) => self.confirm_for = None,
                            None => {}
                        }
                        return;
                    }
                    if theme::round_button(ui, "Uninstall", p, n > 0).clicked() {
                        self.confirm_for = Some(key);
                    }
                    let msg = if n == 0 {
                        "Tick the apps you want to remove".to_string()
                    } else {
                        format!("{} selected · {} · moved to Trash", theme::plural(n, "app"), fmt_size(size))
                    };
                    ui.label(theme::dim(msg, 12.5));
                });
            });
    }

    fn list(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let q = self.query.to_lowercase();
        let mut order: Vec<usize> = (0..self.apps.len())
            .filter(|&i| q.is_empty() || self.apps[i].name.to_lowercase().contains(&q))
            .collect();
        if self.sort_size {
            order.sort_by(|&a, &b| self.apps[b].total().cmp(&self.apps[a].total()));
        }

        egui::Panel::left("apps_list")
            .resizable(false)
            .exact_size((ui.available_width() * 0.45).clamp(256.0, 416.0))
            .frame(theme::side_frame())
            .show_separator_line(false)
            .show(ui, |ui| {
                let row_h = 50.0;
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show_rows(ui, row_h, order.len(), |ui, range| {
                        for &i in &order[range] {
                            let focused = self.focus.as_ref() == Some(&self.apps[i].path);
                            let resp = theme::row(ui, row_h, focused || self.apps[i].checked, |ui| {
                                ui.checkbox(&mut self.apps[i].checked, "");
                                let app = self.apps[i].clone();
                                self.app_icon(ui, &app, 38.0);
                                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(theme::semi(fmt_size(app.total()), 13.5));
                                    if app.arch == Arch::Intel {
                                        ui.label(theme::ic(icon::WARNING, 14.0).color(theme::WARN))
                                            .on_hover_text("Intel-only app (runs through Rosetta)");
                                    }
                                    ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
                                        ui.add_space(8.0);
                                        ui.add(egui::Label::new(theme::semi(&app.name, 14.0)).truncate());
                                        let sub = if app.broken {
                                            "Broken shortcut".to_string()
                                        } else {
                                            app.version.clone().unwrap_or_default()
                                        };
                                        ui.label(theme::dim(sub, 11.5));
                                    });
                                });
                            });
                            if resp.clicked() {
                                let path = self.apps[i].path.clone();
                                if !self.last_opened.contains_key(&path) && self.opened_job.is_none() {
                                    let p2 = path.clone();
                                    self.opened_job = Some(Job::spawn(ctx, move |_, _| (p2.clone(), last_opened(&p2))));
                                }
                                self.focus = Some(path);
                            }
                        }
                    });
            });
    }

    fn detail(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let Some(i) = self.focus.as_ref().and_then(|f| self.apps.iter().position(|a| &a.path == f)) else {
            theme::card(ui, |ui| {
                ui.set_min_size(ui.available_size());
                theme::centered(ui, |ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(theme::ic(icon::PACKAGE, 54.0));
                    ui.label(theme::semi("Select an app", 18.0));
                    ui.label(theme::dim("See its details and every file it keeps on your Mac.", 13.5));
                });
            });
            return;
        };
        // Fetch "last opened" lazily for the focused app.
        let path = self.apps[i].path.clone();
        if !self.last_opened.contains_key(&path) && self.opened_job.is_none() {
            let p2 = path.clone();
            self.opened_job = Some(Job::spawn(ctx, move |_, _| (p2.clone(), last_opened(&p2))));
        }
        let app = self.apps[i].clone();
        let opened = self.last_opened.get(&path).cloned().unwrap_or_else(|| "…".into());

        theme::card(ui, |ui| {
            ui.set_min_size(ui.available_size());
            // One scroll for the whole panel, so nothing spills out on small windows.
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                self.app_icon(ui, &app, 72.0);
                ui.vertical(|ui| {
                    ui.label(theme::bold(&app.name, 24.0));
                    ui.label(theme::dim(
                        format!("Version {}", app.version.as_deref().unwrap_or("unknown")),
                        13.0,
                    ));
                    ui.horizontal(|ui| {
                        if app.app_store {
                            theme::badge(ui, "App Store", Color32::from_rgb(0x5a, 0xa8, 0xff));
                        }
                        if app.broken {
                            theme::badge(ui, "Broken shortcut · the app it opened is gone", theme::WARN);
                        }
                        if app.package_apps > 0 {
                            theme::badge(
                                ui,
                                &format!("Folder · {} apps inside", app.package_apps),
                                Color32::from_rgb(0x8a, 0x5c, 0xe6),
                            );
                        }
                        arch_badge(ui, app.arch);
                    });
                });
            });
            ui.add_space(8.0);
            egui::Grid::new("app_info")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .min_col_width(90.0)
                .show(ui, |ui| {
                    let cell = |ui: &mut Ui, k: &str, v: String| {
                        ui.label(theme::dim(k, 12.5));
                        ui.add(egui::Label::new(theme::text(v, 13.0)).truncate());
                        ui.end_row();
                    };
                    cell(ui, "App", fmt_size(app.size));
                    cell(
                        ui,
                        "Support files",
                        format!(
                            "{} · {}",
                            fmt_size(app.leftovers.iter().map(|l| l.size).sum()),
                            theme::plural(app.leftovers.len(), "item")
                        ),
                    );
                    cell(ui, "Will remove", fmt_size(app.total()));
                    cell(ui, "Last opened", opened);
                    cell(ui, "Bundle ID", app.bundle_id.clone().unwrap_or_else(|| "—".into()));
                    ui.label(theme::dim("Location", 12.5));
                    if ui
                        .add(
                            egui::Label::new(theme::text(fsutil::pretty(&app.path), 13.0).underline())
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text("Reveal in Finder")
                        .clicked()
                    {
                        fsutil::reveal(&app.path);
                    }
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let chk = &mut self.apps[i].checked;
                let label = if *chk {
                    format!("{}  Selected for removal", icon::CHECK_CIRCLE)
                } else {
                    "Select for removal".to_string()
                };
                let fill = if *chk {
                    theme::DANGER.gamma_multiply(0.7)
                } else {
                    Color32::from_white_alpha(35)
                };
                if theme::pill(ui, label, fill).clicked() {
                    *chk = !*chk;
                }
            });
            ui.separator();
            ui.label(theme::semi(format!("Related files ({})", app.leftovers.len()), 14.0));
            if app.leftovers.iter().any(|l| l.user_data) {
                ui.label(
                    theme::dim(
                        format!(
                            "{}  Items marked \"Your data\" (profiles, accounts, logins, saved content) are kept unless you tick them.",
                            icon::LOCK_SIMPLE
                        ),
                        12.0,
                    )
                    .color(Color32::from_rgb(0xc8, 0xe0, 0xff)),
                );
            }
            if app.leftovers.is_empty() {
                ui.label(theme::dim("No support files found.", 12.5));
                return;
            }
            {
                for l in &mut self.apps[i].leftovers {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut l.selected, "");
                        ui.allocate_ui_with_layout(
                            egui::vec2(118.0, 18.0),
                            Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_width(118.0);
                                ui.add(egui::Label::new(theme::dim(l.kind, 11.5)).truncate());
                            },
                        );
                        if l.user_data {
                            theme::badge(ui, "Your data", theme::WARN);
                        }
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(6.0);
                            ui.label(theme::dim(fmt_size(l.size), 12.5));
                            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                                let r = ui.add(
                                    egui::Label::new(theme::text(fsutil::pretty(&l.path), 12.5))
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                );
                                if r.on_hover_text("Reveal in Finder").clicked() {
                                    fsutil::reveal(&l.path);
                                }
                            });
                        });
                    });
                }
            }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PNG header claiming the given size, with no image data.
    fn png_claiming(w: u32, h: u32) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut chunk = b"IHDR".to_vec();
        chunk.extend_from_slice(&ihdr);
        let crc = {
            let mut c = 0xffff_ffffu32;
            for &b in &chunk {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        };
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc.to_be_bytes());
        out
    }

    #[test]
    fn oversized_website_icons_are_refused_before_decoding() {
        // Claims 60000 x 60000 px (about 14 GB of pixels): refused at once.
        assert!(decode_png_bytes(&png_claiming(60_000, 60_000)).is_none());
        assert!(decode_png_bytes(&png_claiming(257, 16)).is_none());
        assert!(decode_png_bytes(b"not a png").is_none());
        // A normal 32 px icon still decodes.
        let mut ok = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut ok, 32, 32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&[200u8; 32 * 32 * 4]).unwrap();
        }
        assert!(decode_png_bytes(&ok).is_some());
    }
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("macsweep-apps-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn app(dir: &Path, name: &str, bytes: usize) {
        let a = dir.join(format!("{name}.app/Contents/MacOS"));
        fs::create_dir_all(&a).unwrap();
        fs::write(a.join("bin"), vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn install_folder_with_helpers_is_one_package() {
        let d = tmp("pkg").join("Cisco Packet Tracer 8.2.2");
        app(&d, "Cisco Packet Tracer 8.2.2", 50_000);
        app(&d, "Linguist", 1_000);
        app(&d, "maintenancetool", 2_000);
        fs::create_dir_all(d.join("lib")).unwrap();
        match classify(&d) {
            Folder::Package(apps) => {
                assert_eq!(apps.len(), 3);
                assert_eq!(main_app(&d, &apps).file_stem().unwrap(), "Cisco Packet Tracer 8.2.2");
            }
            other => panic!("expected a package, got {other:?}"),
        }
    }

    #[test]
    fn folder_of_separate_apps_lists_each_app() {
        let d = tmp("plain").join("Games");
        app(&d, "Chess Pro", 1_000);
        app(&d, "Solitaire", 1_000);
        fs::write(d.join(".DS_Store"), b"x").unwrap();
        assert!(matches!(classify(&d), Folder::Apps(a) if a.len() == 2));
    }

    #[test]
    fn png_icons_decode_to_icon_size() {
        let d = tmp("png");
        let f = d.join("i.png");
        {
            let w = std::io::BufWriter::new(fs::File::create(&f).unwrap());
            let mut e = png::Encoder::new(w, 128, 128);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut wr = e.write_header().unwrap();
            wr.write_image_data(&vec![200u8; 128 * 128 * 4]).unwrap();
        }
        let img = decode_png(&f).unwrap();
        assert_eq!(img.size, [ICON_PX, ICON_PX]);
    }

    #[test]
    fn leftovers_of_a_sibling_app_are_not_taken() {
        let ids = vec!["com.google.chrome".to_string(), "com.google.chrome.canary".to_string()];
        let chrome = "com.google.chrome";
        assert!(belongs_to("com.google.chrome.plist", chrome, &ids));
        assert!(belongs_to("com.google.chrome", chrome, &ids));
        assert!(
            belongs_to("com.google.chrome.helper", chrome, &ids),
            "its own helpers still count"
        );
        assert!(!belongs_to("com.google.chrome.canary.plist", chrome, &ids));
        assert!(!belongs_to("com.google.chrome.canary", chrome, &ids));
        assert!(belongs_to("com.google.chrome.canary", "com.google.chrome.canary", &ids));
        assert!(!belongs_to("com.google.chromecast.plist", chrome, &ids));
        assert!(belongs_to("ubf8t346g9.com.microsoft.teams", "com.microsoft.teams", &[]));
        assert!(!belongs_to("com.microsoft.teams2.plist", "com.microsoft.teams", &[]));
    }

    #[test]
    fn folder_without_a_matching_main_app_is_listed_per_app() {
        let d = tmp("nomatch").join("Vendor Suite");
        app(&d, "Helper", 1_000);
        app(&d, "Editor", 90_000);
        fs::write(d.join("README.txt"), b"x").unwrap();
        assert!(matches!(classify(&d), Folder::Apps(a) if a.len() == 2));
    }

    #[test]
    fn utilities_is_never_one_package() {
        let d = tmp("utils").join("Utilities");
        app(&d, "Utilities Helper", 1_000);
        app(&d, "Adobe Creative Cloud Cleaner Tool", 1_000);
        fs::create_dir_all(d.join("Adobe Creative Cloud")).unwrap();
        assert!(matches!(classify(&d), Folder::Apps(_)));
    }

    #[test]
    fn your_folder_with_an_app_and_notes_is_not_removed_whole() {
        let d = tmp("mine").join("My Stuff");
        app(&d, "Some Game", 1_000);
        fs::write(d.join("notes.txt"), b"x").unwrap();
        assert!(matches!(classify(&d), Folder::Apps(_)));
    }

    #[test]
    fn install_folders_inside_utilities_are_found() {
        let root = tmp("nested");
        let u = root.join("Utilities");
        app(&u, "Disk Tool", 1_000);
        let pkg = u.join("Acme Sync 2");
        app(&pkg, "Acme Sync 2", 1_000);
        fs::create_dir_all(pkg.join("lib")).unwrap();
        let mut out = Vec::new();
        discover_in(&root, 0, &mut out);
        assert!(
            out.iter()
                .any(|(p, pk)| p.ends_with("Utilities/Disk Tool.app") && pk.is_none())
        );
        assert!(out.iter().any(|(p, pk)| p.ends_with("Acme Sync 2") && pk.is_some()));
        assert!(
            !out.iter().any(|(p, _)| p.ends_with("Utilities")),
            "Utilities itself is never an entry"
        );
    }
}
