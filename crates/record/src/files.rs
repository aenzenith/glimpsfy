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
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from_c = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let to_c = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: both are valid NUL-terminated paths for the duration of the call.
        let rc = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // Some file systems (network shares, FAT) do not support the exclusive rename; fall
        // back to a hard link, which also refuses an existing name.
        if !matches!(err.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EINVAL)) {
            return Err(err);
        }
    }
    link_then_unlink(from, to)
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
    #[cfg(unix)]
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
    #[cfg(not(unix))]
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
