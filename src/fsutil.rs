//! File-system helpers shared by every feature.
use crate::{safe_fs, safety_log};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Bumped every time we delete or trash something, so the UI knows to
/// refresh disk-space and Trash numbers right away.
pub static CHANGES: AtomicU64 = AtomicU64::new(0);

pub fn changes() -> u64 {
    CHANGES.load(Ordering::Relaxed)
}

pub fn bump() {
    CHANGES.fetch_add(1, Ordering::Relaxed);
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// "~/Library/Caches" style display path.
pub fn pretty(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

pub fn children(p: &Path) -> Vec<PathBuf> {
    fs::read_dir(p)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

/// Real on-disk size (allocated blocks), like Finder's "on disk".
/// Files that macOS stores compressed (common inside app bundles) report
/// 0 blocks, so fall back to their logical size instead of showing 0.
pub fn allocated(md: &fs::Metadata) -> u64 {
    let blocks = md.blocks() * 512;
    if blocks == 0 { md.len() } else { blocks }
}

/// Size of an app bundle. Resolves symlinked apps and, if the fast walk
/// still finds nothing, asks `du` as a last resort.
pub fn app_size(p: &Path) -> u64 {
    let real = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = size_of(&real);
    if s > 0 {
        return s;
    }
    Command::new("/usr/bin/du")
        .arg("-sk")
        .arg(&real)
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

/// Size of a file or a whole folder tree.
///
/// The walk is serial on purpose: callers already run many of these in
/// parallel with rayon, and a nested parallel jwalk on a busy pool returns
/// errors instead of entries (which made folders and apps show 0 bytes).
pub fn size_of(p: &Path) -> u64 {
    let Ok(md) = fs::symlink_metadata(p) else { return 0 };
    if md.file_type().is_symlink() {
        return 0;
    }
    if !md.is_dir() {
        return allocated(&md);
    }
    jwalk::WalkDir::new(p)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(jwalk::Parallelism::Serial)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type.is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| allocated(&m))
        .sum()
}

/// Bytes formatted the way macOS does (base 1000).
pub fn fmt_size(b: u64) -> String {
    const U: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} bytes")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn fmt_age(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "—".into() };
    let days = SystemTime::now().duration_since(t).map(|d| d.as_secs() / 86_400).unwrap_or(0);
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

pub fn reveal(p: &Path) {
    let _ = Command::new("/usr/bin/open").arg("-R").arg(p).spawn();
}

/// Opens macOS Quick Look (the Space-bar preview) for a file.
pub fn quick_look(p: &Path) {
    let _ = Command::new("/usr/bin/qlmanage")
        .arg("-p")
        .arg(p)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

pub fn open(target: &str) {
    let _ = Command::new("/usr/bin/open").arg(target).spawn();
}

// ---------- safety ----------

/// The ONLY folders (relative to ~) whose contents junk cleaning may delete.
/// Browser profiles (~/Library/Application Support/...), Mail (~/Library/Mail),
/// app containers, keychains, cookies, documents etc. are never in this list,
/// so they can never be deleted by a clean.
pub const JUNK_ROOTS: &[&str] = &[
    "Library/Caches",
    "Library/Logs",
    "Library/Developer/Xcode/DerivedData",
    "Library/Developer/Xcode/iOS DeviceSupport",
    "Library/Developer/CoreSimulator/Caches",
    ".npm/_cacache",
    ".gradle/caches",
    ".cache/huggingface",
    ".cache/torch",
    "Library/Containers/com.apple.mail/Data/Library/Mail Downloads",
    ".Trash",
];

/// Cache folders that hold sign-in, iCloud or sync state rather than
/// throw-away data. Deleting them can sign you out or break syncing.
const PROTECTED_NAMES: &[&str] = &[
    "cloudkit",
    "com.apple.akd",
    "com.apple.bird",
    "com.apple.clouddocs",
    "com.apple.cloudd",
    "com.apple.icloudhelper",
    "com.apple.homed",
    "com.apple.passd",
    "com.apple.nsurlsessiond",
    "com.apple.containermanagerd",
    "familycircle",
];

/// Anything whose name suggests credentials or account data is skipped.
const PROTECTED_WORDS: &[&str] = &[
    "keychain",
    "password",
    "1password",
    "bitwarden",
    "lastpass",
    "dashlane",
    "credential",
    "token",
    "auth",
    "account",
    "login",
];

/// True if a junk item must never be shown or deleted.
pub fn is_protected(p: &Path) -> bool {
    let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    PROTECTED_NAMES.contains(&n.as_str()) || PROTECTED_WORDS.iter().any(|w| n.contains(w))
}

/// Only immediate children of a configured root can be selected by System Junk.
fn junk_location(home: &Path, path: &Path) -> bool {
    home.is_absolute()
        && home != Path::new("/")
        && JUNK_ROOTS
            .iter()
            .any(|r| path.parent() == Some(home.join(r).as_path()) && (*r == ".Trash" || !is_protected(path)))
}

pub fn junk_children(home: &Path, root: &str) -> Vec<PathBuf> {
    if !home.is_absolute() || !JUNK_ROOTS.contains(&root) {
        return Vec::new();
    }
    let path = home.join(root);
    match safe_fs::open_dir(&path).and_then(|dir| safe_fs::names(&dir)) {
        Ok(names) => names
            .into_iter()
            .map(|n| path.join(n))
            .filter(|p| junk_location(home, p))
            .collect(),
        Err(e) => {
            if e != rustix::io::Errno::NOENT {
                safety_log::event(true, &format!("Skipped a cleanup root during directory inspection ({e})"));
            }
            Vec::new()
        }
    }
}

fn delete_junk(home: &Path, path: &Path) -> rustix::io::Result<()> {
    if !junk_location(home, path) {
        return Err(rustix::io::Errno::PERM);
    }
    safe_fs::remove(path, path.parent() == Some(home.join(".Trash").as_path()))
}

pub fn delete_permanently(items: &[(PathBuf, u64)]) -> (u64, usize) {
    let home = home();
    let (mut freed, mut failed) = (0u64, 0usize);
    safety_log::event(
        false,
        &format!("Checking the locations of {} selected junk items", items.len()),
    );
    for (path, size) in items {
        match delete_junk(&home, path) {
            Ok(()) => freed += size,
            Err(e) => {
                safety_log::event(true, &format!("Skipped a junk item during removal ({e})"));
                failed += 1;
            }
        }
    }
    bump();
    (freed, failed)
}

/// Finder handles ordinary items, including its native permission prompts.
/// A refusal leaves the item in place. No shell or elevated retry is needed.
pub fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    move_to_trash_with(paths, &trash_dir(), |items| trash::delete_all(items).is_ok())
}

fn move_to_trash_with(paths: &[PathBuf], destination: &Path, finder: impl Fn(&[&PathBuf]) -> bool) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    safety_log::event(false, &format!("Moving {} selected items to the Trash", paths.len()));
    let (links, items): (Vec<_>, Vec<_>) = paths.iter().filter(|p| lexists(p)).partition(|p| is_symlink(p));
    for link in links {
        if let Err(e) = move_into(link, destination) {
            safety_log::event(true, &format!("Could not move a link to the Trash ({e})"));
        }
    }
    for chunk in items.chunks(200) {
        if !finder(chunk) {
            for path in chunk.iter().filter(|p| lexists(p)) {
                if !finder(std::slice::from_ref(path)) {
                    safety_log::event(true, "Finder refused a selected item");
                }
            }
        }
    }
    bump();
    let left: Vec<_> = paths
        .iter()
        .filter(|p| !matches!(fs::symlink_metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound))
        .map(|p| pretty(p))
        .collect();
    if left.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Could not confirm removal of {}. Review these items in Finder.",
            left.join(", ")
        ))
    }
}

/// True if something exists at this path, including a link whose target is gone.
pub fn lexists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

pub fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// A link whose target no longer exists.
pub fn is_broken_link(p: &Path) -> bool {
    is_symlink(p) && !p.exists()
}

/// Moves an item into `dir`, adding a number if the name is taken.
pub fn move_into(p: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    use rustix::fs::{RenameFlags, renameat_with};
    let (source_dir, name) = safe_fs::parent(p)?;
    let dest_dir = safe_fs::open_dir(dir)?;
    let mut candidate = name.to_owned();
    for index in 2.. {
        match renameat_with(&source_dir, name, &dest_dir, &candidate, RenameFlags::NOREPLACE) {
            Ok(()) => return Ok(dir.join(candidate)),
            Err(rustix::io::Errno::EXIST) => {
                candidate = name.to_owned();
                candidate.push(format!(" {index}"));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(std::io::Error::other("No available name in the Trash"))
}

/// Single-quotes a string for /bin/sh.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn trash_dir() -> PathBuf {
    home().join(".Trash")
}

/// Every Trash Finder shows as one: yours, plus the one on each external drive.
pub fn trash_dirs() -> Vec<PathBuf> {
    let mut v = vec![trash_dir()];
    let uid = unsafe { libc::getuid() };
    for vol in children(Path::new("/Volumes")) {
        if is_symlink(&vol) {
            continue; // the startup disk appears here as a link to "/"
        }
        let t = vol.join(".Trashes").join(uid.to_string());
        if t.is_dir() {
            v.push(t);
        }
    }
    v
}

/// Changes whenever something is put in or taken out of any Trash
/// (by Finder or by us), so the sidebar can refresh without re-measuring.
pub fn trash_signature() -> u64 {
    trash_dirs()
        .iter()
        .filter_map(|d| fs::metadata(d).ok())
        .map(|m| (m.mtime() as u64).wrapping_mul(1_000_000_007) ^ m.mtime_nsec() as u64 ^ m.nlink())
        .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b))
}

pub fn trash_size() -> u64 {
    trash_dirs().iter().map(|d| size_of(d)).sum()
}

/// Permanently deletes the contents of the user's Trash on each volume.
pub fn empty_trash() -> (u64, usize) {
    let (mut freed, mut failed) = (0u64, 0usize);
    safety_log::event(false, "Opening the Trash directories before removal");
    for path in trash_dirs() {
        let result = safe_fs::open_dir(&path).and_then(|dir| Ok((safe_fs::names(&dir)?, dir)));
        match result {
            Ok((names, dir)) => {
                for name in names {
                    let size = size_of(&path.join(&name));
                    match safe_fs::remove_at(&dir, &name, true) {
                        Ok(()) => freed += size,
                        Err(e) => {
                            safety_log::event(true, &format!("Could not empty a Trash item ({e})"));
                            failed += 1;
                        }
                    }
                }
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(e) => {
                safety_log::event(true, &format!("Could not open a Trash directory ({e})"));
                failed += 1;
            }
        }
    }
    bump();
    (freed, failed)
}

/// (total, available) bytes of the startup volume.
pub fn disk_info() -> Option<(u64, u64)> {
    let path = std::ffi::CString::new("/").ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u64;
    Some((s.f_blocks as u64 * frsize, s.f_bavail as u64 * frsize))
}

/// Free space the way Finder counts it: includes space macOS can clear on its
/// own (purgeable caches, old local snapshots). Asked through Foundation via
/// JavaScript for Automation, so no extra libraries are needed. None if it
/// can't be read; callers then use the plain number from `disk_info`.
pub fn finder_available() -> Option<u64> {
    const JS: &str = "ObjC.import('Foundation');\
var u=$.NSURL.fileURLWithPath('/');var r=Ref();\
u.getResourceValueForKeyError(r,$.NSURLVolumeAvailableCapacityForImportantUsageKey,null);\
String(r[0].longLongValue)";
    let out = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", JS])
        .output()
        .ok()?;
    parse_bytes(&String::from_utf8_lossy(&out.stdout))
}

fn parse_bytes(s: &str) -> Option<u64> {
    s.trim().parse::<u64>().ok().filter(|&v| v > 0)
}

/// The name of the startup disk ("Macintosh HD" unless you renamed it).
/// /Volumes holds a link to "/" with that name.
pub fn startup_disk_name() -> String {
    children(Path::new("/Volumes"))
        .into_iter()
        .find(|v| fs::read_link(v).is_ok_and(|t| t == Path::new("/")))
        .and_then(|v| v.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Macintosh HD".into())
}

/// Full Disk Access check: these locations are only readable with it.
/// Several probes are used because not every Mac has every folder.
pub fn has_full_disk_access() -> bool {
    let h = home();
    let files = [
        h.join("Library/Application Support/com.apple.TCC/TCC.db"),
        PathBuf::from("/Library/Application Support/com.apple.TCC/TCC.db"),
    ];
    let dirs = [
        h.join("Library/Safari"),
        h.join("Library/Mail"),
        h.join("Library/Messages"),
        h.join(".Trash"),
    ];
    files.iter().any(|f| fs::File::open(f).is_ok()) || dirs.iter().any(|d| fs::read_dir(d).is_ok())
}

/// The .app bundle we're running from, if any (None when run from Terminal).
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// Starts a fresh copy of the app and quits this one — macOS only applies a
/// newly granted Full Disk Access to a freshly started app.
pub fn relaunch() {
    if let Some(app) = app_bundle() {
        // Single-quoted for the shell: a folder name with $ or ` can't run anything.
        let script = format!("sleep 1; /usr/bin/open -n {}", sh_quote(&app.to_string_lossy()));
        if Command::new("/bin/sh").arg("-c").arg(script).spawn().is_ok() {
            std::process::exit(0);
        }
    }
}

pub fn open_fda_settings() {
    open("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_profiles_and_mail_are_never_deletable() {
        let h = home();
        for p in [
            "Library/Application Support/Google/Chrome/Default",
            "Library/Application Support/BraveSoftware/Brave-Browser/Default",
            "Library/Application Support/Firefox/Profiles",
            "Library/Mail/V10",
            "Library/Keychains/login.keychain-db",
            "Library/Cookies/Cookies.binarycookies",
            "Library/Containers/com.apple.Safari",
            "Library/Safari/History.db",
            "Documents/thesis.docx",
            "Library/Caches/../Application Support/Google",
        ] {
            assert!(!junk_location(&h, &h.join(p)), "{p} must be protected");
        }
    }

    #[test]
    fn finder_free_space_output_is_parsed_safely() {
        assert_eq!(parse_bytes("123456789\n"), Some(123456789));
        assert_eq!(parse_bytes(""), None);
        assert_eq!(parse_bytes("undefined"), None);
        assert_eq!(parse_bytes("0"), None);
    }

    #[test]
    fn a_refused_trash_request_preserves_the_file_and_its_owner() {
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-denied-{}", std::process::id()));
        fs::create_dir_all(base.join("trash")).unwrap();
        let file = base.join("keep");
        fs::write(&file, b"original").unwrap();
        let uid = fs::metadata(&file).unwrap().uid();
        let error = move_to_trash_with(std::slice::from_ref(&file), &base.join("trash"), |_| false).unwrap_err();
        assert!(error.contains("Finder"));
        assert_eq!(fs::metadata(&file).unwrap().uid(), uid);
        assert_eq!(fs::read(file).unwrap(), b"original");
        assert_eq!(fs::read_dir(base.join("trash")).unwrap().count(), 0);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn moving_a_link_preserves_existing_trash_items() {
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-trash-name-{}", std::process::id()));
        let trash = base.join("trash");
        fs::create_dir_all(&trash).unwrap();
        let link = base.join("shortcut");
        std::os::unix::fs::symlink("missing", &link).unwrap();
        fs::write(trash.join("shortcut"), b"already here").unwrap();
        assert_eq!(move_into(&link, &trash).unwrap(), trash.join("shortcut 2"));
        assert_eq!(fs::read(trash.join("shortcut")).unwrap(), b"already here");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn junk_removal_stays_inside_real_cache_directories() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-junk-safe-{}", std::process::id()));
        let caches = base.join("Library/Caches");
        let retained = base.join("Documents");
        fs::create_dir_all(&retained).unwrap();
        fs::create_dir_all(caches.join("ordinary/subdir")).unwrap();
        fs::write(retained.join("notes"), b"keep").unwrap();
        symlink(&retained, caches.join("ordinary/subdir/link")).unwrap();
        assert!(junk_children(&base, "Library/Caches").contains(&caches.join("ordinary")));
        delete_junk(&base, &caches.join("ordinary")).unwrap();
        fs::remove_dir(&caches).unwrap();
        symlink(&retained, &caches).unwrap();
        assert!(junk_children(&base, "Library/Caches").is_empty());
        assert!(delete_junk(&base, &caches.join("notes")).is_err());
        assert_eq!(fs::read(retained.join("notes")).unwrap(), b"keep");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn broken_links_are_detected_and_moved() {
        let d = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("trash")).unwrap();
        let link = d.join("JDownloader2.app");
        std::os::unix::fs::symlink(d.join("gone/JDownloader2.app"), &link).unwrap();
        assert!(!link.exists(), "following the link finds nothing");
        assert!(lexists(&link) && is_broken_link(&link), "but the link itself is there");
        let dest = move_into(&link, &d.join("trash")).unwrap();
        assert!(!lexists(&link));
        assert!(is_symlink(&dest));
    }

    #[test]
    fn roots_themselves_are_not_deletable() {
        for r in JUNK_ROOTS {
            assert!(!junk_location(&home(), &home().join(r)), "{r}");
        }
    }

    #[test]
    fn ordinary_cache_items_are_deletable() {
        let h = home();
        assert!(junk_location(&h, &h.join("Library/Caches/com.google.Chrome")));
        assert!(junk_location(&h, &h.join("Library/Caches/Homebrew")));
        assert!(junk_location(&h, &h.join(".Trash/old.dmg")));
        assert!(junk_location(&h, &h.join(".Trash/passwords-backup.txt")));
    }

    #[test]
    fn sign_in_caches_are_protected() {
        let h = home();
        for p in [
            "Library/Caches/CloudKit",
            "Library/Caches/com.apple.akd",
            "Library/Caches/com.1password.1password",
            "Library/Caches/com.app.AuthService",
        ] {
            assert!(!junk_location(&h, &h.join(p)), "{p}");
        }
    }
}
