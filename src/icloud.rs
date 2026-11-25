//! Only Foundation can remove an iCloud download. This module never deletes file data.
use crate::safety_log;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Clone, Copy)]
struct Identity {
    device: u64,
    inode: u64,
    size: i64,
    seconds: i64,
    nanos: i64,
}

#[derive(Debug)]
pub struct Download {
    pub path: PathBuf,
    pub bytes: u64,
    identity: Identity,
}

// Fixed-width fields match the Foundation bridge in cloud_file.m.
#[cfg(any(test, target_os = "macos"))]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CloudStatus {
    in_cloud: u8,
    uploaded: u8,
    local_current: u8,
    idle: u8,
    conflict_free: u8,
    purgeable: u8,
    error_free: u8,
}

#[cfg(any(test, target_os = "macos"))]
impl CloudStatus {
    fn check(self) -> Result<(), String> {
        for (confirmed, reason) in [
            (self.in_cloud, "This file is outside iCloud Drive."),
            (self.uploaded, "iCloud has not confirmed a complete upload."),
            (self.local_current, "The current version is not downloaded on this Mac."),
            (
                self.idle,
                "iCloud is transferring the file or its transfer status is unavailable.",
            ),
            (
                self.conflict_free,
                "iCloud has not confirmed that this file is free of conflicts.",
            ),
            (self.error_free, "iCloud reports a sync error for this file."),
            (
                self.purgeable,
                "macOS is keeping this download. Check Keep Downloaded in Finder.",
            ),
        ] {
            if confirmed != 1 {
                safety_log::event(false, reason);
                return Err(reason.into());
            }
        }
        Ok(())
    }
}

fn confirm_with(
    item: &Download,
    probe: impl FnOnce(&Path) -> Result<Download, String>,
    evict: impl FnOnce(&Download) -> Result<(), String>,
) -> Result<(), String> {
    let current = probe(&item.path)?;
    if current.identity != item.identity {
        return Err("The file changed. Select it again.".into());
    }
    evict(&current)
}

pub fn remove_download(item: Download) -> Result<(), String> {
    safety_log::event(false, "iCloud: recheck selection and ask Foundation to remove the download");
    let result = confirm_with(&item, inspect, native::evict);
    safety_log::event(
        result.is_err(),
        if result.is_ok() {
            "iCloud: Foundation accepted the eviction"
        } else {
            "iCloud: check or eviction failed; show the reason"
        },
    );
    result
}

pub fn inspect(path: &Path) -> Result<Download, String> {
    safety_log::event(false, "iCloud: check file identity, sync state and local allocation");
    native::inspect(path)
}

pub use native::pick;

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use crate::safe_fs;
    use rustix::fs::{AtFlags, FileType, statat};
    use std::ffi::{CStr, CString, OsStr, c_char, c_int};
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;
    unsafe extern "C" {
        fn macsweep_pick_path(directory: c_int, start: *const c_char) -> *mut c_char;
        fn macsweep_cloud_state(path: *const c_char, status: *mut CloudStatus, error: *mut *mut c_char) -> c_int;
        fn macsweep_evict(
            path: *const c_char,
            device: u64,
            inode: u64,
            size: i64,
            seconds: i64,
            nanos: i64,
            error: *mut *mut c_char,
        ) -> c_int;
    }

    fn cpath(path: &Path) -> Result<CString, String> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| "Invalid file name.".into())
    }

    fn checked(ok: c_int, error: *mut c_char) -> Result<(), String> {
        if ok != 0 {
            return Ok(());
        }
        if error.is_null() {
            return Err("macOS could not complete the operation.".into());
        }
        // The bridge returns an owned NUL-terminated message on failure.
        let message = unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() };
        unsafe { libc::free(error.cast()) };
        Err(message)
    }

    pub fn pick(directory: bool, start: &Path) -> Option<PathBuf> {
        let start = cpath(start).ok()?;
        // Called on the main UI thread. NSOpenPanel owns its modal run loop.
        let path = unsafe { macsweep_pick_path(directory as c_int, start.as_ptr()) };
        if path.is_null() {
            return None;
        }
        let result = PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(path) }.to_bytes()));
        unsafe { libc::free(path.cast()) };
        Some(result)
    }

    pub fn inspect(path: &Path) -> Result<Download, String> {
        let (parent, name) = safe_fs::parent(path).map_err(|e| e.to_string())?;
        let st = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| e.to_string())?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile || st.st_nlink != 1 {
            return Err("Select a regular file with a single link.".into());
        }
        if st.st_flags & (libc::UF_IMMUTABLE | libc::SF_IMMUTABLE) != 0 {
            return Err("This file is locked.".into());
        }
        let path_c = cpath(path)?;
        let mut status = CloudStatus::default();
        let mut error = ptr::null_mut();
        let ok = unsafe { macsweep_cloud_state(path_c.as_ptr(), &mut status, &mut error) };
        checked(ok, error)?;
        status.check()?;
        let bytes = (st.st_blocks.max(0) as u64).saturating_mul(512);
        if bytes == 0 {
            return Err("This file has no allocated local data.".into());
        }
        Ok(Download {
            path: path.to_owned(),
            bytes,
            identity: Identity {
                device: st.st_dev as u64,
                inode: st.st_ino,
                size: st.st_size,
                seconds: st.st_mtime,
                nanos: st.st_mtime_nsec,
            },
        })
    }

    pub fn evict(item: &Download) -> Result<(), String> {
        let path = cpath(&item.path)?;
        let id = item.identity;
        let mut error = ptr::null_mut();
        let ok = unsafe { macsweep_evict(path.as_ptr(), id.device, id.inode, id.size, id.seconds, id.nanos, &mut error) };
        checked(ok, error)
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::*;
    pub fn pick(_: bool, _: &Path) -> Option<PathBuf> {
        None
    }
    pub fn inspect(_: &Path) -> Result<Download, String> {
        Err("iCloud Drive requires macOS.".into())
    }
    pub fn evict(_: &Download) -> Result<(), String> {
        Err("iCloud Drive requires macOS.".into())
    }
}

#[cfg(test)]
pub(super) fn sample_download(inode: u64) -> Download {
    Download {
        path: PathBuf::from("/fixture/document"),
        bytes: 4096,
        identity: Identity {
            device: 3,
            inode,
            size: 14,
            seconds: 27,
            nanos: 3,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_cloud_checks_must_pass() {
        let ready = CloudStatus {
            in_cloud: 1,
            uploaded: 1,
            local_current: 1,
            idle: 1,
            conflict_free: 1,
            purgeable: 1,
            error_free: 1,
        };
        assert!(ready.check().is_ok());
        for unavailable in [
            CloudStatus { in_cloud: 0, ..ready },
            CloudStatus { uploaded: 0, ..ready },
            CloudStatus {
                local_current: 0,
                ..ready
            },
            CloudStatus { idle: 0, ..ready },
            CloudStatus {
                conflict_free: 0,
                ..ready
            },
            CloudStatus { purgeable: 0, ..ready },
            CloudStatus { error_free: 0, ..ready },
            CloudStatus::default(),
        ] {
            assert!(unavailable.check().is_err());
        }
    }
    #[test]
    fn confirmation_rechecks_status_and_identity_before_the_native_request() {
        let first = sample_download(42);
        assert!(
            confirm_with(
                &first,
                |_| Err("Upload in progress".into()),
                |_| panic!("unexpected eviction")
            )
            .is_err()
        );
        assert!(confirm_with(&first, |_| Ok(sample_download(43)), |_| panic!("unexpected eviction")).is_err());
        assert!(confirm_with(&first, |_| Ok(sample_download(42)), |_| Ok(())).is_ok());
        assert_eq!(
            confirm_with(&first, |_| Ok(sample_download(42)), |_| Err("Provider is offline".into())).unwrap_err(),
            "Provider is offline"
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn foundation_rejects_a_real_non_cloud_file_without_changing_it() {
        use std::fs;
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("macsweep-cloud-{}.txt", std::process::id()));
        fs::write(&path, b"Keep this content").unwrap();
        assert!(inspect(&path).unwrap_err().contains("outside iCloud Drive"));
        assert_eq!(fs::read(&path).unwrap(), b"Keep this content");
        fs::remove_file(path).unwrap();
    }
}
