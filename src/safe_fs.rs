//! File operations relative to open directories. Each lookup refuses links in its parents.
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, open, openat, statat, unlinkat};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub fn open_dir(path: &Path) -> rustix::io::Result<OwnedFd> {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return Err(Errno::INVAL);
    }
    let mut dir = open("/", DIRECTORY, Mode::empty())?;
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(Errno::INVAL);
        };
        dir = openat(&dir, name, DIRECTORY, Mode::empty())?;
    }
    Ok(dir)
}

pub fn parent(path: &Path) -> rustix::io::Result<(OwnedFd, &OsStr)> {
    let name = path.file_name().ok_or(Errno::INVAL)?;
    Ok((open_dir(path.parent().ok_or(Errno::INVAL)?)?, name))
}

pub fn open_regular(path: &Path) -> rustix::io::Result<File> {
    let (dir, name) = parent(path)?;
    let before = statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
        return Err(Errno::INVAL);
    }
    let fd = openat(
        &dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    if !same_object(&before, &fstat(&fd)?) {
        return Err(Errno::STALE);
    }
    Ok(fd.into())
}

pub fn names(dir: &OwnedFd) -> rustix::io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            names.push(OsStr::from_bytes(name).to_owned());
        }
    }
    Ok(names)
}

fn same_object(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

/// Finder's Locked bit can be cleared only on the object already opened in the Trash.
#[cfg(target_os = "macos")]
fn unlock(dir: &OwnedFd, name: &OsStr, entry: &Stat) -> rustix::io::Result<()> {
    use std::os::fd::AsRawFd;
    if entry.st_flags & libc::UF_IMMUTABLE == 0 {
        return Ok(());
    }
    match FileType::from_raw_mode(entry.st_mode) {
        FileType::Directory => {}
        FileType::RegularFile if entry.st_nlink == 1 => {}
        _ => return Err(Errno::PERM),
    }
    let fd = openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let current = fstat(&fd)?;
    if !same_object(&current, entry) {
        return Err(Errno::STALE);
    }
    // The file descriptor pins the object until fchflags returns.
    if unsafe { libc::fchflags(fd.as_raw_fd(), current.st_flags & !libc::UF_IMMUTABLE) } == -1 {
        return Err(Errno::from_io_error(&std::io::Error::last_os_error()).unwrap_or(Errno::IO));
    }
    Ok(())
}

/// Names come from readdir or Path::file_name. Never cross a mount during recursion.
pub fn remove_at(dir: &OwnedFd, name: &OsStr, in_trash: bool) -> rustix::io::Result<()> {
    // openat accepts absolute and multi-component arguments. Require one child.
    if Path::new(name).file_name() != Some(name) {
        return Err(Errno::INVAL);
    }
    let entry = statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if entry.st_dev != fstat(dir)?.st_dev {
        return Err(Errno::XDEV);
    }
    #[cfg(target_os = "macos")]
    if in_trash {
        unlock(dir, name, &entry)?;
    }
    let flags = if FileType::from_raw_mode(entry.st_mode) == FileType::Directory {
        let child = openat(dir, name, DIRECTORY, Mode::empty())?;
        if !same_object(&entry, &fstat(&child)?) {
            return Err(Errno::STALE);
        }
        for child_name in names(&child)? {
            remove_at(&child, &child_name, in_trash)?;
        }
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    if !same_object(&entry, &statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?) {
        return Err(Errno::STALE);
    }
    unlinkat(dir, name, flags)
}

pub fn remove(path: &Path, in_trash: bool) -> rustix::io::Result<()> {
    let (dir, name) = parent(path)?;
    remove_at(&dir, name, in_trash)
}
