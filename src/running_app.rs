//! Stop only the running instances of selected application bundles.
#[cfg(any(target_os = "macos", test))]
use std::path::Path;
use std::path::PathBuf;

/// Match the selected directory on disk. Display names and Info.plist fields are untrusted.
#[cfg(any(target_os = "macos", test))]
fn bundle_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path)?;
    if !md.is_dir() {
        return Err(std::io::Error::other("Expected an application directory"));
    }
    Ok((md.dev(), md.ino()))
}

#[cfg(target_os = "macos")]
pub fn quit_bundles(paths: &[PathBuf]) -> Result<(), String> {
    use objc2_app_kit::NSWorkspace;
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::time::{Duration, Instant};
    let mut selected = Vec::new();
    for path in paths {
        if !path.is_symlink() {
            selected.push(bundle_identity(path).map_err(|e| format!("Could not check the application directory ({e})."))?);
        }
    }
    if selected.is_empty() {
        return Ok(());
    }
    crate::safety_log::event(false, "Checking running applications against the selected bundles");
    let running = NSWorkspace::sharedWorkspace().runningApplications();
    let mut quitting = Vec::new();
    for app in running.iter() {
        let Some(url) = app.bundleURL() else { continue };
        // Foundation owns these bytes for the lifetime of the URL.
        let bytes = unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) }.to_bytes();
        let identity = bundle_identity(Path::new(OsStr::from_bytes(bytes)));
        if identity.is_ok_and(|id| selected.contains(&id)) && !app.isTerminated() {
            if !app.terminate() && !app.isTerminated() {
                return Err("Quit the application and try again.".into());
            }
            quitting.push(app);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while quitting.iter().any(|app| !app.isTerminated()) {
        if Instant::now() >= deadline {
            return Err("The application is still running. Quit it and try again.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn quit_bundles(_: &[PathBuf]) -> Result<(), String> {
    Err("Application removal requires macOS.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_identity_uses_the_selected_bundle_on_disk() {
        let base = std::env::temp_dir().join(format!("macsweep-bundle-id-{}", std::process::id()));
        let first = base.join("A/Sample.app");
        let second = base.join("B/Sample.app");
        for app in [&first, &second] {
            std::fs::create_dir_all(app.join("Contents")).unwrap();
            std::fs::write(app.join("Contents/Info.plist"), b"identical metadata").unwrap();
        }
        assert_ne!(bundle_identity(&first).unwrap(), bundle_identity(&second).unwrap());
        assert_eq!(
            bundle_identity(&first).unwrap(),
            bundle_identity(&first.canonicalize().unwrap()).unwrap()
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
