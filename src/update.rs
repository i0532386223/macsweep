//! "A new version is available": one question to GitHub at launch.
//!
//! MacSweep asks GitHub for the newest published release and compares it
//! with its own version. Nothing about the Mac or the user is sent; it is
//! the same request a browser makes when you open the Releases page. It
//! uses macOS's own curl, so MacSweep needs no networking library.
//!
//! To turn it off, create this empty file:
//! `~/Library/Application Support/MacSweep/no-update-check`
use crate::fsutil;

const LATEST: &str = "https://api.github.com/repos/i0532386223/macsweep/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// "0.4.0"
    pub version: String,
    /// The release page, with its notes and download.
    pub url: String,
}

/// "v0.3.0" or "0.3.0-beta.1" → (0, 3, 0). Pre-release text is ignored.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (
        it.next()??,
        it.next().flatten().unwrap_or(0),
        it.next().flatten().unwrap_or(0),
    );
    Some(v)
}

/// True if `latest` is a newer version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

/// The release GitHub reports as latest (drafts and pre-releases are never
/// "latest"), if it's a well-formed one on our own repository.
pub fn parse(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v.get("draft")?.as_bool()? || v.get("prerelease")?.as_bool()? {
        return None;
    }
    let tag = v.get("tag_name")?.as_str()?;
    let url = v.get("html_url")?.as_str()?;
    // Only ever open our own release pages.
    if !url.starts_with("https://github.com/i0532386223/macsweep/releases/") {
        return None;
    }
    parse_version(tag)?;
    Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        url: url.to_string(),
    })
}

fn turned_off() -> bool {
    fsutil::home()
        .join("Library/Application Support/MacSweep/no-update-check")
        .exists()
}

/// Asks GitHub once. Returns the release only if it's newer than this
/// copy of MacSweep. Any failure (offline, rate limit) is silent.
pub fn check() -> Option<Release> {
    if turned_off() {
        return None;
    }
    let out = std::process::Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--fail",
            "--location",
            "--max-time",
            "10",
            "--proto",
            "=https",
            "--header",
            "Accept: application/vnd.github+json",
            "--user-agent",
            concat!("MacSweep/", env!("CARGO_PKG_VERSION")),
            LATEST,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let r = parse(&String::from_utf8_lossy(&out.stdout))?;
    is_newer(&r.version, env!("CARGO_PKG_VERSION")).then_some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(is_newer("v0.4.0", "0.3.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("v0.2.0", "0.3.0"));
        assert!(!is_newer("v0.3.0-beta.1", "0.3.0"));
        assert!(!is_newer("garbage", "0.3.0"));
    }

    #[test]
    fn reads_githubs_latest_release() {
        // Shaped like GitHub's real answer for this repository.
        let json = r#"{"tag_name":"v0.4.0","html_url":"https://github.com/i0532386223/macsweep/releases/tag/v0.4.0",
                       "draft":false,"prerelease":false,"assets":[{"name":"MacSweep-0.4.0.zip"}]}"#;
        assert_eq!(
            parse(json),
            Some(Release {
                version: "0.4.0".into(),
                url: "https://github.com/i0532386223/macsweep/releases/tag/v0.4.0".into()
            })
        );
    }

    #[test]
    fn ignores_anything_odd() {
        let other_site = r#"{"tag_name":"v9.0.0","html_url":"https://evil.example/x","draft":false,"prerelease":false}"#;
        assert_eq!(parse(other_site), None);
        let pre = r#"{"tag_name":"v0.4.0","html_url":"https://github.com/i0532386223/macsweep/releases/tag/v0.4.0","draft":false,"prerelease":true}"#;
        assert_eq!(parse(pre), None);
        assert_eq!(parse(r#"{"message":"API rate limit exceeded"}"#), None);
        assert_eq!(parse("not json"), None);
    }
}
