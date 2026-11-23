//! The files a recording writes: free names, a rename that never replaces a file, free disk
//! space, and the hidden working files an export writes before it takes its final name.

use std::io;
use std::path::{Path, PathBuf};

/// Below this much free space a recording does not start, and a running one stops while the
/// file can still be finished.
pub const LOW_DISK_BYTES: u64 = 512 * 1024 * 1024;

/// `dir/stem.ext`, or `dir/stem 2.ext`, `dir/stem 3.ext` … for the first name no file has.
pub fn free_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !exists(&first) {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} {n}.{ext}"))).find(|p| !exists(p)).unwrap_or(first)
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// A hidden working path next to `final_path`, which an export writes first; the dot keeps it
/// out of Finder and the `.part` tells a person what it is if they see it.
pub fn part_path(final_path: &Path, job: &str) -> PathBuf {
    let dir = final_path.parent().unwrap_or(Path::new("."));
    let name = final_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    dir.join(format!(".{name}.{job}.part"))
}

/// Moves `from` to `to` only if no file has the name `to`; refuses with `AlreadyExists`
/// otherwise. The check and the move are one step, so a file created in between is not
/// replaced either.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    if exclusive_rename(from, to)? {
        return Ok(());
    }
    link_then_unlink(from, to)
}

/// The system's own rename that refuses an existing name. `Ok(false)` says the file system does
/// not offer it (network shares, FAT), and the caller falls back to a hard link, which also
/// refuses an existing name.
#[cfg(target_os = "macos")]
fn exclusive_rename(from: &Path, to: &Path) -> io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from_c = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let to_c = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    let rc = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EINVAL)) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// `renameat2(RENAME_NOREPLACE)`, through the system call itself, so it works with a C library
/// older than the call's wrapper (glibc 2.28). File systems and kernels without it (before 3.15,
/// some network and FUSE file systems) answer `EINVAL`, `ENOSYS` or `EOPNOTSUPP`, and the hard
/// link takes over.
#[cfg(target_os = "linux")]
fn exclusive_rename(from: &Path, to: &Path) -> io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from_c = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let to_c = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: both are valid NUL-terminated paths for the duration of the call, and the
    // arguments are the ones `renameat2(2)` takes.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    // `EEXIST` already reads as `ErrorKind::AlreadyExists`.
    let err = io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`: the file system refuses an existing name in
/// the same step as the move. A file system or a volume that cannot move the file there answers
/// with an error that hands over to the hard link.
#[cfg(target_os = "windows")]
fn exclusive_rename(from: &Path, to: &Path) -> io::Result<bool> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FILE_EXISTS, ERROR_INVALID_FUNCTION,
        ERROR_NOT_SAME_DEVICE, ERROR_NOT_SUPPORTED,
    };
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
    let (from_w, to_w) = (wide_path(from), wide_path(to));
    // SAFETY: both are NUL-terminated UTF-16 paths that outlive the call, and no flag lets the
    // move replace a file.
    let Err(error) = (unsafe { MoveFileExW(PCWSTR(from_w.as_ptr()), PCWSTR(to_w.as_ptr()), MOVE_FILE_FLAGS(0)) })
    else {
        return Ok(true);
    };
    let code = win32_code(&error);
    if code == Some(ERROR_ALREADY_EXISTS.0) || code == Some(ERROR_FILE_EXISTS.0) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists already", to.display())));
    }
    let unsupported = [ERROR_NOT_SAME_DEVICE, ERROR_NOT_SUPPORTED, ERROR_INVALID_FUNCTION, ERROR_CALL_NOT_IMPLEMENTED];
    if unsupported.iter().any(|e| code == Some(e.0)) {
        return Ok(false);
    }
    Err(win32_io(&error))
}

/// `path` as a NUL-terminated UTF-16 string.
#[cfg(target_os = "windows")]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// The Win32 error code behind a failed call; the bindings report it as `HRESULT_FROM_WIN32`.
#[cfg(target_os = "windows")]
fn win32_code(error: &windows::core::Error) -> Option<u32> {
    let hresult = error.code().0 as u32;
    (hresult & 0xFFFF_0000 == 0x8007_0000).then_some(hresult & 0xFFFF)
}

#[cfg(target_os = "windows")]
fn win32_io(error: &windows::core::Error) -> io::Error {
    match win32_code(error) {
        Some(code) => io::Error::from_raw_os_error(code as i32),
        None => io::Error::other(error.message()),
    }
}

fn link_then_unlink(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::hard_link(from, to) {
        Ok(()) => std::fs::remove_file(from),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            // No hard links here either (FAT): create the name exclusively, then copy into it.
            let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
            let mut input = std::fs::File::open(from)?;
            if let Err(e) = io::copy(&mut input, &mut out) {
                drop(out);
                let _ = std::fs::remove_file(to);
                return Err(e);
            }
            std::fs::remove_file(from)
        }
    }
}

/// Moves `from` to the first free name based on `wanted`, never replacing a file, and returns
/// the name it took.
pub fn place_without_replacing(from: &Path, wanted: &Path) -> io::Result<PathBuf> {
    let dir = wanted.parent().unwrap_or(Path::new("."));
    let stem = wanted.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Recording".into());
    let ext = wanted.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    for _ in 0..32 {
        let target = free_path(dir, &stem, &ext);
        match rename_no_replace(from, &target) {
            Ok(()) => return Ok(target),
            // Someone took the name between the check and the move; try the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free file name was found"))
}

/// The free bytes on the volume that holds `path` (or its nearest existing parent).
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no existing folder"))?;
    }
    // Linux counts the free blocks in fragments (`f_frsize`), which `statvfs` reports; its
    // `statfs` block size is only the preferred transfer size, which differs on some file systems.
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(probe.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: `stat` is plain old data and is fully written by a successful call.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid path and `stat` a valid out pointer.
        if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(probe.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: `stat` is plain old data and is fully written by a successful call.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid path and `stat` a valid out pointer.
        if unsafe { libc::statfs(c.as_ptr(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(stat.f_bavail as u64 * stat.f_bsize as u64)
    }
    // The bytes this user may still write: quotas count, as they do for the recording.
    #[cfg(target_os = "windows")]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        // The call takes a folder; a file's own folder is on the same volume.
        let folder = if probe.is_dir() { probe } else { probe.parent().unwrap_or(probe) };
        let folder = if folder.as_os_str().is_empty() { Path::new(".") } else { folder };
        let wide = wide_path(folder);
        let mut available = 0u64;
        // SAFETY: the path is NUL-terminated and outlives the call, which writes one u64.
        unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut available as *mut u64), None, None) }
            .map_err(|e| win32_io(&e))?;
        Ok(available)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        Ok(u64::MAX)
    }
}

/// Moves a file to the Trash, where the user can still get it back. It is never unlinked.
pub trait Trash: Send + Sync {
    fn trash(&self, path: &Path) -> io::Result<()>;
}

/// The system's Trash (`NSFileManager.trashItemAtURL` on macOS).
pub struct SystemTrash;

impl Trash for SystemTrash {
    fn trash(&self, path: &Path) -> io::Result<()> {
        crate::platform::trash(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("Clip.mp4");
        std::fs::write(&taken, b"keep me").unwrap();
        let from = dir.path().join(".Clip.mp4.job.part");
        std::fs::write(&from, b"new").unwrap();

        let err = rename_no_replace(&from, &taken).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(link_then_unlink(&from, &taken).is_err());
        assert_eq!(std::fs::read(&taken).unwrap(), b"keep me");

        let placed = place_without_replacing(&from, &taken).unwrap();
        assert_eq!(placed, dir.path().join("Clip 2.mp4"));
        assert_eq!(std::fs::read(&taken).unwrap(), b"keep me");
        assert_eq!(std::fs::read(&placed).unwrap(), b"new");
        assert!(!from.exists());
    }
}
