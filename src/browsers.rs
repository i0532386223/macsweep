//! Browser Cleanup: finds what each browser keeps on this Mac and sorts it
//! into three groups, so nothing is removed by surprise:
//!
//! - **AI models** the browser downloaded (e.g. Chrome's Gemini Nano),
//! - **website data**: databases, offline files and files that websites
//!   keep. This is where web games keep saves and downloaded game files, so
//!   it is listed per website and never selected for you,
//! - **cache**: page, script and graphics caches the browser rebuilds.
//!
//! Passwords, cookies, history, bookmarks, extensions and Local Storage
//! (where sign-ins live) are never listed and can never be removed: the
//! allowlist in [`allowed`] only accepts the exact folder kinds above.
use crate::fsutil;
use crate::leveldb;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// Browsers built on Chromium, which all lay their files out the same way.
/// (name, bundle id, data folder, cache folder) — folders relative to ~/Library.
const CHROMIUM: &[(&str, &str, &str, &str)] = &[
    (
        "Chrome",
        "com.google.Chrome",
        "Application Support/Google/Chrome",
        "Caches/Google/Chrome",
    ),
    (
        "Chrome Beta",
        "com.google.Chrome.beta",
        "Application Support/Google/Chrome Beta",
        "Caches/Google/Chrome Beta",
    ),
    (
        "Chrome Canary",
        "com.google.Chrome.canary",
        "Application Support/Google/Chrome Canary",
        "Caches/Google/Chrome Canary",
    ),
    (
        "Brave",
        "com.brave.Browser",
        "Application Support/BraveSoftware/Brave-Browser",
        "Caches/BraveSoftware/Brave-Browser",
    ),
    (
        "Edge",
        "com.microsoft.edgemac",
        "Application Support/Microsoft Edge",
        "Caches/Microsoft Edge",
    ),
    (
        "Arc",
        "company.thebrowser.Browser",
        "Application Support/Arc/User Data",
        "Caches/Arc/User Data",
    ),
    (
        "Vivaldi",
        "com.vivaldi.Vivaldi",
        "Application Support/Vivaldi",
        "Caches/Vivaldi",
    ),
    (
        "Opera",
        "com.operasoftware.Opera",
        "Application Support/com.operasoftware.Opera",
        "Caches/com.operasoftware.Opera",
    ),
    (
        "Opera GX",
        "com.operasoftware.OperaGX",
        "Application Support/com.operasoftware.OperaGX",
        "Caches/com.operasoftware.OperaGX",
    ),
    (
        "Chromium",
        "org.chromium.Chromium",
        "Application Support/Chromium",
        "Caches/Chromium",
    ),
];

/// AI model folders browsers keep next to their profiles:
/// (folder, name, what it does).
pub const MODELS: &[(&str, &str, &str)] = &[
    (
        "OptGuideOnDeviceModel",
        "Gemini Nano",
        "Google's on-device language model, used for Help me write, page summaries and tab organising.",
    ),
    (
        "OptGuideOnDeviceClassifierModel",
        "Scam detection model",
        "A small model that checks pages for scams and phishing. Removing it may weaken those warnings.",
    ),
    (
        "optimization_guide_model_store",
        "Small feature models",
        "Small models individual browser features download, such as tab grouping and form filling.",
    ),
    (
        "OnDeviceHeadSuggestModel",
        "Address bar suggestions",
        "Suggests searches as you type in the address bar.",
    ),
    ("screen_ai", "Screen AI", "Reads text in images and PDFs, for accessibility."),
    ("SODA", "Live Caption", "Speech recognition for Live Caption."),
    (
        "SODALanguagePacks",
        "Live Caption languages",
        "Language packs for Live Caption.",
    ),
];

/// Profiles the browser uses internally; never listed or touched.
const HIDDEN_PROFILES: &[&str] = &["System Profile", "Guest Profile"];
/// Graphics caches shared by all profiles.
const SHARED_JUNK: &[&str] = &["GrShaderCache", "ShaderCache", "GraphiteDawnCache"];
/// Caches inside a profile (in the data folder or ~/Library/Caches).
const PROFILE_JUNK: &[&str] = &["Cache", "Code Cache", "GPUCache", "DawnGraphiteCache", "DawnWebGPUCache"];

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Group {
    Model,
    Site,
    Junk,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub group: Group,
    /// "Gemini Nano", "web.nfsmwo.com", "Cache"
    pub title: String,
    /// What it is or what it's made of.
    pub detail: String,
    /// The profile it belongs to (None for browser-wide items).
    pub profile: Option<String>,
    pub paths: Vec<PathBuf>,
    pub size: u64,
    pub newest: Option<SystemTime>,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct Browser {
    pub name: &'static str,
    pub bundle_id: &'static str,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub profiles: Vec<String>,
    pub items: Vec<Item>,
    pub running: bool,
}

impl Browser {
    pub fn total(&self) -> u64 {
        self.items.iter().map(|i| i.size).sum()
    }
    pub fn group_total(&self, g: Group) -> u64 {
        self.items.iter().filter(|i| i.group == g).map(|i| i.size).sum()
    }
}

/// Size on disk and the newest change time of everything under `p`.
fn measure(p: &Path) -> (u64, Option<SystemTime>) {
    let Ok(md) = std::fs::symlink_metadata(p) else {
        return (0, None);
    };
    if md.file_type().is_symlink() {
        return (0, None);
    }
    if !md.is_dir() {
        return (fsutil::allocated(&md), md.modified().ok());
    }
    let mut size = 0;
    let mut newest = md.modified().ok();
    for e in jwalk::WalkDir::new(p)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(jwalk::Parallelism::Serial)
        .into_iter()
        .filter_map(Result::ok)
    {
        if let Ok(m) = e.metadata() {
            if e.file_type.is_file() {
                size += fsutil::allocated(&m);
            }
            newest = newest.max(m.modified().ok());
        }
    }
    (size, newest)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// "https_www.facebook.com_0" → "www.facebook.com"; None for anything that
/// isn't a website (extensions, chrome:// pages, local files).
pub fn origin_host(id: &str) -> Option<String> {
    let (scheme, rest) = id.split_once('_')?;
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let (host, port) = rest.rsplit_once('_')?;
    if host.is_empty() {
        return None;
    }
    Some(if port == "0" {
        host.to_string()
    } else {
        format!("{host}:{port}")
    })
}

/// "https://web.nfsmwo.com/" → "web.nfsmwo.com"
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// The website a Service Worker cache folder belongs to. Chrome names the
/// folder by a hash and writes the site's address into its index.txt, a
/// protocol buffer: the first web address in it is the site that owns it.
pub fn cache_storage_owner(index: &[u8]) -> Option<String> {
    fn walk(buf: &[u8]) -> Option<String> {
        let mut pos = 0;
        while pos < buf.len() {
            let mut key = 0u64;
            let mut shift = 0;
            loop {
                let b = *buf.get(pos)?;
                pos += 1;
                key |= u64::from(b & 0x7f) << shift;
                if b < 0x80 {
                    break;
                }
                shift += 7;
                if shift > 63 {
                    return None;
                }
            }
            match key & 7 {
                0 => {
                    while *buf.get(pos)? >= 0x80 {
                        pos += 1;
                    }
                    pos += 1;
                }
                1 => pos += 8,
                5 => pos += 4,
                2 => {
                    let mut len = 0usize;
                    let mut shift = 0;
                    loop {
                        let b = *buf.get(pos)?;
                        pos += 1;
                        len |= usize::from(b & 0x7f) << shift;
                        if b < 0x80 {
                            break;
                        }
                        shift += 7;
                        if shift > 63 {
                            return None;
                        }
                    }
                    let s = buf.get(pos..pos.checked_add(len)?)?;
                    pos += len;
                    if let Ok(t) = std::str::from_utf8(s) {
                        if let Some(h) = url_host(t.trim()) {
                            return Some(h);
                        }
                    }
                }
                _ => return None,
            }
        }
        None
    }
    walk(index)
}

/// Which numbered "File System" folder belongs to which website, from
/// Chrome's own index (a LevelDB in "File System/Origins").
fn file_system_owners(fs_dir: &Path) -> HashMap<String, String> {
    leveldb::read_all(&fs_dir.join("Origins"))
        .into_iter()
        .filter_map(|(k, v)| {
            let id = String::from_utf8(k).ok()?.strip_prefix("ORIGIN:")?.to_string();
            Some((String::from_utf8(v).ok()?, origin_host(&id)?))
        })
        .collect()
}

/// Profile folder names with the names you gave them in the browser,
/// from its "Local State" file.
fn profile_names(data: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(data.join("Local State")) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    json.pointer("/profile/info_cache")
        .and_then(|c| c.as_object())
        .map(|c| {
            c.iter()
                .filter_map(|(dir, v)| Some((dir.clone(), v.get("name")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Profile folders: those with a Preferences file, except the browser's
/// own hidden ones. Opera keeps its only profile in the data folder itself,
/// which is returned as "".
fn profile_dirs(data: &Path) -> Vec<String> {
    let mut v: Vec<String> = fsutil::children(data)
        .into_iter()
        .filter(|p| p.join("Preferences").is_file())
        .map(|p| name_of(&p))
        .filter(|n| !HIDDEN_PROFILES.contains(&n.as_str()))
        .collect();
    if data.join("Preferences").is_file() {
        v.push(String::new());
    }
    // Default first, then "Profile 2", "Profile 10"… in number order.
    v.sort_by_key(|n| {
        let num = n.strip_prefix("Profile ").and_then(|s| s.parse::<u32>().ok());
        (n != "Default", num.unwrap_or(u32::MAX), n.clone())
    });
    v
}

/// Does this folder hold a model file (not one MacSweep knows by name)?
fn holds_model_file(dir: &Path) -> bool {
    dir.join("weights.bin").is_file() || fsutil::children(dir).iter().any(|c| c.join("weights.bin").is_file())
}

/// "2026.2.12.1554" — the newest version folder inside a model folder.
fn model_version(dir: &Path) -> Option<String> {
    fsutil::children(dir)
        .into_iter()
        .filter(|p| p.is_dir())
        .map(|p| name_of(&p))
        .filter(|n| n.contains('.') && n.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .max_by_key(|n| n.split('.').map(|s| s.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>())
}

fn breakdown(parts: &[(&str, u64)]) -> String {
    let mut by: BTreeMap<&str, u64> = BTreeMap::new();
    for (k, s) in parts {
        *by.entry(k).or_default() += s;
    }
    let mut v: Vec<_> = by.into_iter().filter(|(_, s)| *s > 0).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.iter()
        .map(|(k, s)| format!("{k} {}", fsutil::fmt_size(*s)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Everything one profile holds that MacSweep can list.
fn scan_profile(data: &Path, cache: &Path, dir: &str, label: &str) -> Vec<Item> {
    let p = data.join(dir);
    let mut items = Vec::new();

    // Cache, in the data folder and in ~/Library/Caches.
    let mut cache_parts = Vec::new();
    let mut cache_paths = Vec::new();
    let mut newest = None;
    for root in [&p, &cache.join(dir)] {
        for j in PROFILE_JUNK {
            let path = root.join(j);
            if !path.is_dir() || fsutil::is_symlink(&path) {
                continue;
            }
            let (s, n) = measure(&path);
            let kind = match *j {
                "Cache" => "pages",
                "Code Cache" => "compiled scripts",
                _ => "graphics",
            };
            cache_parts.push((kind, s));
            cache_paths.push(path);
            newest = newest.max(n);
        }
    }
    let size: u64 = cache_parts.iter().map(|(_, s)| s).sum();
    if size > 0 {
        items.push(Item {
            group: Group::Junk,
            title: "Cache".into(),
            detail: breakdown(&cache_parts),
            profile: Some(label.into()),
            paths: cache_paths,
            size,
            newest,
            selected: true,
        });
    }

    // Website data, collected per website.
    let mut parts: Vec<(String, &str, PathBuf)> = Vec::new();
    for e in fsutil::children(&p.join("IndexedDB")) {
        let n = name_of(&e);
        let id = n
            .strip_suffix(".indexeddb.leveldb")
            .or_else(|| n.strip_suffix(".indexeddb.blob"));
        if let Some(host) = id.and_then(origin_host) {
            parts.push((host, "databases", e));
        }
    }
    for e in fsutil::children(&p.join("Service Worker/CacheStorage")) {
        let n = name_of(&e);
        if n.len() != 40 || !n.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let owner = std::fs::read(e.join("index.txt")).ok().and_then(|b| cache_storage_owner(&b));
        parts.push((owner.unwrap_or_default(), "offline files", e));
    }
    let fs_dir = p.join("File System");
    let owners = file_system_owners(&fs_dir);
    for e in fsutil::children(&fs_dir) {
        let n = name_of(&e);
        if n.len() == 3 && n.bytes().all(|b| b.is_ascii_digit()) {
            parts.push((owners.get(&n).cloned().unwrap_or_default(), "files", e));
        }
    }
    let measured: Vec<(String, &str, PathBuf, u64, Option<SystemTime>)> = parts
        .into_par_iter()
        .map(|(h, k, path)| {
            let (s, n) = measure(&path);
            (h, k, path, s, n)
        })
        .collect();
    let mut sites: BTreeMap<String, (Vec<(&str, u64)>, Vec<PathBuf>, Option<SystemTime>)> = BTreeMap::new();
    for (host, kind, path, size, newest) in measured {
        if size == 0 {
            continue;
        }
        let e = sites.entry(host).or_default();
        e.0.push((kind, size));
        e.1.push(path);
        e.2 = e.2.max(newest);
    }
    for (host, (kinds, paths, newest)) in sites {
        let unknown = host.is_empty();
        items.push(Item {
            group: Group::Site,
            title: if unknown {
                "Websites MacSweep couldn't name".into()
            } else {
                host
            },
            detail: if unknown {
                format!("{} · the browser didn't record which site owns these", breakdown(&kinds))
            } else {
                breakdown(&kinds)
            },
            profile: Some(label.into()),
            size: kinds.iter().map(|(_, s)| s).sum(),
            paths,
            newest,
            selected: false,
        });
    }
    items
}

fn scan_browser(name: &'static str, bundle_id: &'static str, data: PathBuf, cache: PathBuf, running: &[String]) -> Browser {
    let mut items = Vec::new();

    // AI models (browser-wide).
    for e in fsutil::children(&data) {
        if !e.is_dir() || fsutil::is_symlink(&e) {
            continue;
        }
        let folder = name_of(&e);
        let known = MODELS.iter().find(|(f, _, _)| *f == folder);
        if known.is_none() && !holds_model_file(&e) {
            continue;
        }
        let (size, newest) = measure(&e);
        if size < 1024 * 1024 {
            continue; // an empty placeholder, like OptGuideOnDeviceModel before a download
        }
        let (title, what) = match known {
            Some((_, t, w)) => (t.to_string(), w.to_string()),
            None => (
                folder.clone(),
                "Holds an AI model file (weights.bin) that MacSweep doesn't know by name.".into(),
            ),
        };
        let version = model_version(&e).map(|v| format!(" Version {v}.")).unwrap_or_default();
        items.push(Item {
            group: Group::Model,
            title,
            detail: format!("{what}{version} Folder: {folder}"),
            profile: None,
            paths: vec![e],
            size,
            newest,
            selected: false,
        });
    }

    // Graphics caches shared by all profiles.
    let mut shared = Vec::new();
    let mut newest = None;
    for j in SHARED_JUNK {
        let path = data.join(j);
        if path.is_dir() && !fsutil::is_symlink(&path) {
            let (s, n) = measure(&path);
            shared.push(("graphics", s, path));
            newest = newest.max(n);
        }
    }
    let size: u64 = shared.iter().map(|(_, s, _)| s).sum();
    if size > 0 {
        items.push(Item {
            group: Group::Junk,
            title: "Graphics cache".into(),
            detail: "Shared by all profiles".into(),
            profile: None,
            paths: shared.into_iter().map(|(_, _, p)| p).collect(),
            size,
            newest,
            selected: true,
        });
    }

    let names = profile_names(&data);
    let dirs = profile_dirs(&data);
    let labels: Vec<String> = dirs
        .iter()
        .map(|d| match names.get(d) {
            Some(n) if !d.is_empty() && n != d => format!("{n} ({d})"),
            Some(n) => n.clone(),
            None if d.is_empty() => name.to_string(),
            None => d.clone(),
        })
        .collect();
    for (d, l) in dirs.iter().zip(&labels) {
        items.extend(scan_profile(&data, &cache, d, l));
    }
    items.sort_by(|a, b| a.group.cmp(&b.group).then(b.size.cmp(&a.size)));

    Browser {
        name,
        bundle_id,
        running: running.contains(&bundle_id.to_lowercase()),
        data,
        cache,
        profiles: labels,
        items,
    }
}

/// Every Chromium browser that has a data folder on this Mac.
pub fn scan() -> Vec<Browser> {
    let lib = fsutil::home().join("Library");
    let running = crate::junk::running_apps();
    CHROMIUM
        .iter()
        .map(|(n, id, d, c)| (n, id, lib.join(d), lib.join(c)))
        .filter(|(_, _, d, _)| d.is_dir())
        .map(|(n, id, d, c)| scan_browser(n, id, d, c, &running))
        .collect()
}

/// Is this browser open right now? Checked again right before cleaning.
pub fn is_running(bundle_id: &str) -> bool {
    crate::junk::running_apps().contains(&bundle_id.to_lowercase())
}

/// The safety net: true only for the exact kinds of folder this feature
/// lists, inside one browser's own folders. Anything else — passwords,
/// cookies, history, Local Storage, extensions, the profile itself — is
/// refused, whatever the scan returned.
pub fn allowed(data: &Path, cache: &Path, p: &Path) -> bool {
    if p.components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::RootDir))
        || fsutil::is_symlink(p)
    {
        return false;
    }
    let check = |root: &Path, in_data: bool| -> bool {
        let Ok(rel) = p.strip_prefix(root) else { return false };
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        // Browser-wide folders.
        if in_data {
            if let [one] = parts.as_slice() {
                let is_profile = data.join(one).join("Preferences").is_file();
                let model = MODELS.iter().any(|(f, _, _)| f == one) || (!is_profile && holds_model_file(p));
                if SHARED_JUNK.contains(one) || model {
                    return true;
                }
            }
        }
        // Inside a profile: "Default/…", or the data folder itself for Opera.
        let rest = match parts.first() {
            Some(first) if HIDDEN_PROFILES.contains(first) => return false,
            Some(first) if data.join(first).join("Preferences").is_file() => &parts[1..],
            _ if data.join("Preferences").is_file() => &parts[..],
            _ => return false,
        };
        match rest {
            [j] => PROFILE_JUNK.contains(j),
            ["IndexedDB", n] if in_data => {
                let id = n
                    .strip_suffix(".indexeddb.leveldb")
                    .or_else(|| n.strip_suffix(".indexeddb.blob"));
                id.and_then(origin_host).is_some()
            }
            ["Service Worker", "CacheStorage", h] if in_data => h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()),
            ["File System", d] if in_data => d.len() == 3 && d.bytes().all(|b| b.is_ascii_digit()),
            _ => false,
        }
    };
    check(data, true) || check(cache, false)
}

/// What one browser's cleaning should remove.
pub struct Plan {
    pub name: &'static str,
    pub bundle_id: &'static str,
    pub data: PathBuf,
    pub cache: PathBuf,
    /// (group, paths, size)
    pub items: Vec<(Group, Vec<PathBuf>, u64)>,
}

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Cache, deleted outright (browsers rebuild it).
    pub deleted: u64,
    /// AI models and website data, moved to the Trash so you can put them back.
    pub trashed: u64,
    pub failed: usize,
    /// Browsers that were open, so nothing of theirs was touched.
    pub skipped: Vec<&'static str>,
}

/// Cleans, browser by browser. A browser that is open is skipped entirely:
/// it could be writing those files right now.
pub fn clean(plans: Vec<Plan>) -> Outcome {
    let mut out = Outcome::default();
    for plan in plans {
        if is_running(plan.bundle_id) {
            out.skipped.push(plan.name);
            continue;
        }
        let mut to_trash: Vec<(Vec<PathBuf>, u64)> = Vec::new();
        for (group, paths, size) in plan.items {
            if !paths.iter().all(|p| allowed(&plan.data, &plan.cache, p)) {
                out.failed += 1;
                continue;
            }
            if group == Group::Junk {
                let ok = paths
                    .iter()
                    .filter(|p| fsutil::lexists(p))
                    .all(|p| std::fs::remove_dir_all(p).is_ok() || !fsutil::lexists(p));
                if ok {
                    out.deleted += size;
                } else {
                    out.failed += 1;
                }
            } else {
                to_trash.push((paths, size));
            }
        }
        let all: Vec<PathBuf> = to_trash
            .iter()
            .flat_map(|(p, _)| p.iter().cloned())
            .filter(|p| fsutil::lexists(p))
            .collect();
        for chunk in all.chunks(200) {
            if trash::delete_all(chunk).is_err() {
                for p in chunk {
                    let _ = trash::delete(p);
                }
            }
        }
        for (paths, size) in to_trash {
            if paths.iter().any(|p| fsutil::lexists(p)) {
                out.failed += 1;
            } else {
                out.trashed += size;
            }
        }
    }
    fsutil::bump();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_read_like_the_browser_shows_them() {
        assert_eq!(origin_host("https_www.facebook.com_0").as_deref(), Some("www.facebook.com"));
        assert_eq!(origin_host("https_192.168.10.55_5555").as_deref(), Some("192.168.10.55:5555"));
        assert_eq!(origin_host("http_localhost_8080").as_deref(), Some("localhost:8080"));
        assert_eq!(origin_host("chrome-extension_gjjabgpgjpampikjhjpfhneeoapjbjaf_0"), None);
        assert_eq!(origin_host("file__0"), None);
    }

    #[test]
    fn cache_storage_owner_comes_from_index() {
        // Shaped like the real index.txt from a Mac (web.nfsmwo.com):
        // repeated cache entries, then the origin and the top-level site.
        let mut b = Vec::new();
        for i in 0..3 {
            let name = format!("pms-files-{i}");
            let uuid = "70b9c7ce-90fa-437f-872e-2e7b800e715e";
            let mut entry = vec![0x0a, name.len() as u8];
            entry.extend_from_slice(name.as_bytes());
            entry.extend_from_slice(&[0x10, 0x96, 0x01]); // a varint field
            entry.extend_from_slice(&[0x22, uuid.len() as u8]);
            entry.extend_from_slice(uuid.as_bytes());
            b.push(0x0a);
            b.push(entry.len() as u8);
            b.extend_from_slice(&entry);
        }
        for f in [0x12u8, 0x2a] {
            let u = b"https://web.nfsmwo.com/";
            b.push(f);
            b.push(u.len() as u8);
            b.extend_from_slice(u);
        }
        assert_eq!(cache_storage_owner(&b).as_deref(), Some("web.nfsmwo.com"));
        assert_eq!(cache_storage_owner(b"\x0a\x03abc"), None);
        assert_eq!(cache_storage_owner(&[0xff; 20]), None);
    }

    /// A fake Chrome folder with one of everything.
    fn fake(root: &Path) -> (PathBuf, PathBuf) {
        let data = root.join("Application Support/Google/Chrome");
        let cache = root.join("Caches/Google/Chrome");
        let p = data.join("Default");
        let w = |path: PathBuf, bytes: usize| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![7u8; bytes]).unwrap();
        };
        w(p.join("Preferences"), 10);
        w(p.join("Login Data"), 10);
        w(p.join("Cookies"), 10);
        w(p.join("Local Storage/leveldb/000003.log"), 10);
        w(p.join("Code Cache/js/a"), 5000);
        w(cache.join("Default/Cache/Cache_Data/f"), 9000);
        w(p.join("IndexedDB/https_poki.com_0.indexeddb.leveldb/000003.log"), 4000);
        w(p.join("IndexedDB/https_poki.com_0.indexeddb.blob/1/00/1"), 4000);
        w(p.join("IndexedDB/chrome-extension_abc_0.indexeddb.leveldb/000003.log"), 4000);
        let hash = "6c531fa06c238ec16b4c66bbf6c28611b8c0713f";
        let mut index = vec![0x12, 23];
        index.extend_from_slice(b"https://web.nfsmwo.com/");
        w(p.join(format!("Service Worker/CacheStorage/{hash}/index.txt")), 0);
        std::fs::write(p.join(format!("Service Worker/CacheStorage/{hash}/index.txt")), index).unwrap();
        w(p.join(format!("Service Worker/CacheStorage/{hash}/uuid/f_0")), 20000);
        std::fs::create_dir_all(p.join("File System/Origins")).unwrap();
        std::fs::write(
            p.join("File System/Origins/000003.log"),
            leveldb::tests::log(&[leveldb::tests::batch(1, &[("ORIGIN:https_web.nfsmwo.com_0", Some("018"))])]),
        )
        .unwrap();
        w(p.join("File System/018/t/00/00000089"), 30000);
        w(
            data.join("OptGuideOnDeviceClassifierModel/2026.2.12.1554/weights.bin"),
            2_000_000,
        );
        w(data.join("OptGuideOnDeviceModel/placeholder"), 10);
        w(data.join("GrShaderCache/data_0"), 3000);
        w(data.join("System Profile/Preferences"), 10);
        std::fs::write(
            data.join("Local State"),
            r#"{"profile":{"info_cache":{"Default":{"name":"Nihal"}}}}"#,
        )
        .unwrap();
        (data, cache)
    }

    #[test]
    fn scan_sorts_everything_into_the_right_group() {
        let root = std::env::temp_dir().join(format!("ms-browsers-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, cache) = fake(&root);
        let b = scan_browser("Chrome", "com.google.Chrome", data, cache, &[]);
        assert_eq!(b.profiles, vec!["Nihal (Default)".to_string()]);
        let titles: Vec<(Group, &str)> = b.items.iter().map(|i| (i.group, i.title.as_str())).collect();
        assert!(titles.contains(&(Group::Model, "Scam detection model")));
        // The empty Gemini Nano placeholder isn't listed.
        assert!(!titles.contains(&(Group::Model, "Gemini Nano")));
        assert!(titles.contains(&(Group::Site, "web.nfsmwo.com")));
        assert!(titles.contains(&(Group::Site, "poki.com")));
        assert!(titles.contains(&(Group::Junk, "Cache")));
        assert!(titles.contains(&(Group::Junk, "Graphics cache")));
        // Extensions' data is never listed.
        assert!(
            !b.items
                .iter()
                .any(|i| i.paths.iter().any(|p| p.to_string_lossy().contains("chrome-extension")))
        );
        // The game's offline files and its file storage are one line.
        let game = b.items.iter().find(|i| i.title == "web.nfsmwo.com").unwrap();
        assert_eq!(game.paths.len(), 2);
        assert!(game.detail.contains("offline files") && game.detail.contains("files"));
        // Only cache starts selected.
        for i in &b.items {
            assert_eq!(i.selected, i.group == Group::Junk, "{}", i.title);
        }
        // Everything the scan lists passes the safety check.
        for i in &b.items {
            for p in &i.paths {
                assert!(allowed(&b.data, &b.cache, p), "{}", p.display());
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn safety_check_refuses_everything_else() {
        let root = std::env::temp_dir().join(format!("ms-browsers-allow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, cache) = fake(&root);
        let p = data.join("Default");
        for bad in [
            data.clone(),
            p.clone(),
            p.join("Login Data"),
            p.join("Cookies"),
            p.join("Preferences"),
            p.join("Local Storage"),
            p.join("Local Storage/leveldb"),
            p.join("IndexedDB"),
            p.join("IndexedDB/chrome-extension_abc_0.indexeddb.leveldb"),
            p.join("Service Worker"),
            p.join("Service Worker/Database"),
            p.join("File System/Origins"),
            p.join("Code Cache/js"),
            data.join("Local State"),
            data.join("Default/../Default/Cache"),
            data.join("System Profile/Cache"),
            cache.clone(),
            cache.join("Default"),
            cache.join("Default/IndexedDB/https_a.com_0.indexeddb.leveldb"),
            fsutil::home().join("Documents"),
            PathBuf::from("/"),
        ] {
            assert!(!allowed(&data, &cache, &bad), "allowed {}", bad.display());
        }
        assert!(allowed(&data, &cache, &p.join("Code Cache")));
        assert!(allowed(&data, &cache, &cache.join("Default/Cache")));
        assert!(allowed(&data, &cache, &data.join("OptGuideOnDeviceModel")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn profiles_are_listed_in_the_browsers_order() {
        let root = std::env::temp_dir().join(format!("ms-browsers-prof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "Profile 10",
            "Default",
            "Profile 4",
            "System Profile",
            "Guest Profile",
            "Crashpad",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            if d != "Crashpad" {
                std::fs::write(root.join(d).join("Preferences"), "{}").unwrap();
            }
        }
        assert_eq!(profile_dirs(&root), vec!["Default", "Profile 4", "Profile 10"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
