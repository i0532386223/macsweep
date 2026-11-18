//! File-system helpers shared by every feature.
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

fn bump() {
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
    Command::new("du")
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
        1..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

pub fn reveal(p: &Path) {
    let _ = Command::new("open").arg("-R").arg(p).spawn();
}

/// Opens macOS Quick Look (the Space-bar preview) for a file.
pub fn quick_look(p: &Path) {
    let _ = Command::new("qlmanage")
        .arg("-p")
        .arg(p)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

pub fn open(target: &str) {
    let _ = Command::new("open").arg(target).spawn();
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

/// Final gate before any permanent delete: the item must sit *inside* one of
/// the JUNK_ROOTS (never the root itself) and must not be protected.
pub fn deletable(p: &Path) -> bool {
    let h = home();
    if h.as_os_str().is_empty() || p.components().any(|c| c == std::path::Component::ParentDir) {
        return false;
    }
    JUNK_ROOTS.iter().any(|r| {
        let root = h.join(r);
        let inside = p.starts_with(&root) && p != root;
        // Things in the Trash were already thrown away by you, so names don't matter there.
        inside && (*r == ".Trash" || !is_protected(p))
    })
}

/// Permanently deletes junk items — refuses anything that isn't `deletable`.
pub fn delete_permanently(items: &[(PathBuf, u64)]) -> (u64, usize) {
    let (mut freed, mut failed) = (0u64, 0usize);
    for (p, size) in items {
        if !deletable(p) {
            failed += 1;
            continue;
        }
        let mut res = remove_any(p);
        if res.is_err() && p.starts_with(trash_dir()) {
            res = unlock_and_remove(p);
        }
        match res {
            Ok(()) => freed += size,
            Err(_) => failed += 1,
        }
    }
    bump();
    (freed, failed)
}

/// Moves items to the Trash.
///
/// Each item goes through Finder first, so "Put Back" works. Items Finder
/// refuses (usually apps installed by a .pkg and owned by the system) are
/// retried once with the standard macOS administrator password prompt.
/// Returns Err with a readable message if anything is still left.
pub fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let tdir = trash_dir();
    let mut refused = Vec::new();
    let (links, items): (Vec<PathBuf>, Vec<PathBuf>) = paths.iter().filter(|p| lexists(p)).cloned().partition(|p| is_symlink(p));
    // A link (e.g. a leftover shortcut in /Applications) is moved itself;
    // Finder would try to follow it to its target.
    for l in links {
        if move_into(&l, &tdir).is_err() && lexists(&l) {
            refused.push(l);
        }
    }
    // Finder plays its sound once per request, so send items in batches.
    for chunk in items.chunks(200) {
        if trash::delete_all(chunk).is_err() {
            // Find which items Finder refused, one at a time.
            for p in chunk.iter().filter(|p| lexists(p)) {
                if trash::delete(p).is_err() && lexists(p) {
                    refused.push(p.clone());
                }
            }
        }
    }
    let mut cancelled = false;
    if !refused.is_empty() {
        cancelled = !trash_with_admin(&refused);
    }
    bump();
    let left: Vec<String> = paths
        .iter()
        .filter(|p| lexists(p))
        .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        .collect();
    if left.is_empty() {
        Ok(())
    } else if cancelled {
        Err(format!("Cancelled. {} was not removed.", left.join(", ")))
    } else {
        Err(format!("Couldn't remove {}. macOS denied permission.", left.join(", ")))
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
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut dest = dir.join(&name);
    let mut i = 2;
    while lexists(&dest) {
        dest = dir.join(format!("{name} {i}"));
        i += 1;
    }
    fs::rename(p, &dest)?;
    Ok(dest)
}

/// Single-quotes a string for /bin/sh.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Builds the shell script that moves items into the user's Trash and gives
/// them back to the user, so the Trash can be emptied without a password.
pub fn admin_trash_script(paths: &[PathBuf], trash_dir: &Path, user: &str, stamp: u64) -> String {
    // Two items with the same name must not land on the same spot: `mv` would
    // put the second one *inside* the first.
    let mut used = std::collections::HashSet::new();
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut dest = trash_dir.join(&name);
            if lexists(&dest) || used.contains(&dest) {
                dest = trash_dir.join(format!("{name} {stamp}-{i}"));
            }
            used.insert(dest.clone());
            format!(
                "/bin/mv {src} {dst} && /usr/sbin/chown -hR {u} {dst}",
                src = sh_quote(&p.to_string_lossy()),
                dst = sh_quote(&dest.to_string_lossy()),
                u = sh_quote(user),
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Runs the move with administrator rights (macOS shows its password prompt).
/// Returns false if the user cancelled the prompt.
fn trash_with_admin(paths: &[PathBuf]) -> bool {
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty() {
        return true;
    }
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let script = admin_trash_script(paths, &trash_dir(), &user, stamp);
    let applescript = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"MacSweep needs your password to move these items to the Trash.\"",
        script.replace('\\', "\\\\").replace('"', "\\\"")
    );
    match Command::new("osascript").args(["-e", &applescript]).output() {
        // -128 is "User canceled" in AppleScript.
        Ok(o) => !String::from_utf8_lossy(&o.stderr).contains("-128"),
        Err(_) => true,
    }
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

fn remove_any(p: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(md) if md.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) => Err(e),
    }
}

/// Files marked "Locked" in Finder can't be deleted until unlocked.
fn unlock_and_remove(p: &Path) -> std::io::Result<()> {
    let _ = Command::new("chflags").arg("-R").arg("nouchg").arg(p).status();
    remove_any(p)
}

/// Permanently deletes everything in every Trash. Returns (freed, failed).
pub fn empty_trash() -> (u64, usize) {
    let (mut freed, mut failed) = (0u64, 0usize);
    for dir in trash_dirs() {
        for p in children(&dir) {
            let size = size_of(&p);
            if remove_any(&p).is_err() && unlock_and_remove(&p).is_err() && lexists(&p) {
                failed += 1;
            } else {
                freed += size;
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
        let script = format!("sleep 1; open -n {:?}", app);
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
            assert!(!deletable(&h.join(p)), "{p} must be protected");
        }
    }

    #[test]
    fn admin_script_quotes_awkward_names() {
        let t = PathBuf::from("/Users/me/.Trash");
        let p = PathBuf::from("/Applications/Bob's \"Tool\" 1.0.app");
        let s = admin_trash_script(&[p], &t, "me", 7);
        assert_eq!(
            s,
            "/bin/mv '/Applications/Bob'\\''s \"Tool\" 1.0.app' '/Users/me/.Trash/Bob'\\''s \"Tool\" 1.0.app' && /usr/sbin/chown -hR 'me' '/Users/me/.Trash/Bob'\\''s \"Tool\" 1.0.app'"
        );
    }

    #[test]
    fn same_named_items_get_different_trash_names() {
        let t = PathBuf::from("/nonexistent-trash");
        let a = PathBuf::from("/Applications/Foo.app");
        let b = PathBuf::from("/Applications/Utilities/Foo.app");
        let s = admin_trash_script(&[a, b], &t, "me", 7);
        assert!(s.contains("'/nonexistent-trash/Foo.app'"));
        assert!(s.contains("'/nonexistent-trash/Foo.app 7-1'"));
    }

    #[test]
    fn broken_links_are_detected_and_moved() {
        let d = std::env::temp_dir().join(format!("macsweep-link-{}", std::process::id()));
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
            assert!(!deletable(&home().join(r)), "{r}");
        }
    }

    #[test]
    fn ordinary_cache_items_are_deletable() {
        let h = home();
        assert!(deletable(&h.join("Library/Caches/com.google.Chrome")));
        assert!(deletable(&h.join("Library/Caches/Homebrew")));
        assert!(deletable(&h.join(".Trash/old.dmg")));
        assert!(deletable(&h.join(".Trash/passwords-backup.txt")));
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
            assert!(!deletable(&h.join(p)), "{p}");
        }
    }
}
