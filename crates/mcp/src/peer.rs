//! The socket's place and its peers: a Unix socket in the user's own temporary folder, readable
//! by the user alone, that takes a connection only from a process of the same user running
//! Glimpsfy's own binary (the `glimpsfy mcp` bridge).
//!
//! There is no token: any process of the same user could read one from the keychain or the
//! socket's folder as easily as it could start the bridge, so a token would protect nothing that
//! the uid check does not. What keeps an agent from approving its own request is the gate's input
//! check, not the socket.

use std::ffi::{CStr, OsString};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// The user's own temporary folder (`/var/folders/…/T/`), which macOS makes 0700 for the user.
fn user_temp_dir() -> io::Result<PathBuf> {
    // SAFETY: confstr writes at most `len` bytes, NUL included, and returns the size it needs.
    let needed = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u8; needed];
    // SAFETY: `buffer` holds `needed` bytes.
    let written = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buffer.as_mut_ptr().cast(), needed) };
    if written == 0 || written > needed {
        return Err(io::Error::other("the user's temporary folder has no name"));
    }
    buffer.truncate(written - 1);
    Ok(PathBuf::from(OsString::from_vec(buffer)))
}

/// The socket's file name; a dev build listens on its own, so it never answers the installed app's
/// agents.
pub fn socket_name(dev: bool) -> &'static str {
    if dev {
        "glimpsfy-mcp-dev.sock"
    } else {
        "glimpsfy-mcp.sock"
    }
}

/// Where the app listens and the bridge connects.
pub fn socket_path(dev: bool) -> io::Result<PathBuf> {
    Ok(user_temp_dir()?.join(socket_name(dev)))
}

/// Refuses a folder another user owns or may enter, where another user could reach the socket.
pub fn check_folder(folder: &Path) -> io::Result<()> {
    let meta = std::fs::metadata(folder)?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a folder only this user may enter", folder.display()),
        ));
    }
    Ok(())
}

/// Listens at `path`: the folder must be the user's alone, a socket left by a run that crashed is
/// replaced, one another copy of Glimpsfy listens on is not, and the socket is made 0600.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    let folder = path.parent().ok_or_else(|| io::Error::other("the socket has no folder"))?;
    check_folder(folder)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is not a socket", path.display()),
                ));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "another Glimpsfy listens on the socket"));
            }
            std::fs::remove_file(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    // The folder keeps everyone else out until this narrows the socket itself.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// A connected peer that passed the checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: i32,
    pub exe: PathBuf,
}

/// Why a peer was turned away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    OtherUser { uid: u32 },
    OtherBinary { exe: PathBuf },
    Unknown(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::OtherUser { uid } => write!(f, "a process of user {uid} connected"),
            Refusal::OtherBinary { exe } => write!(f, "{} is not Glimpsfy's bridge", exe.display()),
            Refusal::Unknown(why) => write!(f, "the peer could not be checked: {why}"),
        }
    }
}

/// The rule itself: the same user, and the same binary as the app's own.
pub fn check(peer_uid: u32, own_uid: u32, peer_exe: &Path, own_exe: &Path) -> Result<(), Refusal> {
    if peer_uid != own_uid {
        return Err(Refusal::OtherUser { uid: peer_uid });
    }
    let same = match (peer_exe.canonicalize(), own_exe.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same {
        return Err(Refusal::OtherBinary { exe: peer_exe.to_path_buf() });
    }
    Ok(())
}

/// The path of the binary process `pid` runs.
pub fn exe_of(pid: i32) -> Option<PathBuf> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer holds PROC_PIDPATHINFO_MAXSIZE bytes, as proc_pidpath asks.
    let n = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if n <= 0 {
        return None;
    }
    buffer.truncate(n as usize);
    Some(PathBuf::from(OsString::from_vec(buffer)))
}

/// Checks the process at the other end of `stream`.
pub fn verify(stream: &UnixStream, own_exe: &Path) -> Result<Peer, Refusal> {
    let fd = stream.as_raw_fd();
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes the two ids of a connected Unix socket.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(Refusal::Unknown(io::Error::last_os_error().to_string()));
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: LOCAL_PEERPID writes the peer's pid into a pid_t of `len` bytes.
    let got = unsafe {
        libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&mut pid as *mut libc::pid_t).cast(), &mut len)
    };
    if got != 0 || pid <= 0 {
        return Err(Refusal::Unknown(io::Error::last_os_error().to_string()));
    }
    let exe = exe_of(pid).ok_or_else(|| Refusal::Unknown(format!("process {pid} has no binary")))?;
    // SAFETY: getuid cannot fail.
    check(uid, unsafe { libc::getuid() }, &exe, own_exe)?;
    Ok(Peer { pid, exe })
}

fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: PROC_PIDTBSDINFO fills a proc_bsdinfo of `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size)
    };
    (n == size).then_some(info)
}

fn cwd_of(pid: i32) -> Option<PathBuf> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    // SAFETY: PROC_PIDVNODEPATHINFO fills a proc_vnodepathinfo of `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    // SAFETY: the path is a NUL-terminated C string inside its fixed buffer.
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    let path = PathBuf::from(OsString::from_vec(path.to_bytes().to_vec()));
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The first `.app` bundle a binary's path goes through, by its name: "Terminal" for
/// `/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal`, "Cursor" for a helper
/// inside `Cursor.app`.
fn bundle_name(exe: &Path) -> Option<String> {
    exe.components().find_map(|c| {
        let name = c.as_os_str().to_str()?;
        name.strip_suffix(".app").filter(|n| !n.is_empty()).map(str::to_string)
    })
}

/// The line the card's tip and Settings show for a bridge, read from the system rather than from
/// what the agent says: the app it runs in and the agent's working folder, such as
/// "Terminal · ~/projects/glimpsfy".
pub fn describe(bridge: i32) -> Option<String> {
    let agent = bsd_info(bridge)?.pbi_ppid as i32;
    let mut app = None;
    let mut pid = agent;
    for _ in 0..16 {
        if pid <= 1 {
            break;
        }
        if let Some(name) = exe_of(pid).as_deref().and_then(bundle_name) {
            app = Some(name);
            break;
        }
        pid = bsd_info(pid)?.pbi_ppid as i32;
    }
    let app = app.or_else(|| {
        let info = bsd_info(agent)?;
        // SAFETY: pbi_name is a NUL-terminated C string inside its fixed buffer.
        let name = unsafe { CStr::from_ptr(info.pbi_name.as_ptr()) }.to_string_lossy().into_owned();
        (!name.is_empty()).then_some(name)
    })?;
    let Some(cwd) = cwd_of(agent) else {
        return Some(app);
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let folder = match home.as_deref().and_then(|h| cwd.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => cwd.display().to_string(),
    };
    Some(format!("{app} · {folder}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[test]
    fn the_socket_is_the_users_alone() {
        let dir = private_folder();
        let path = dir.path().join(socket_name(true));
        let _listener = bind(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_folder_others_may_enter_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().join(socket_name(true));
        assert_eq!(bind(&path).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(!path.exists());
    }

    #[test]
    fn a_socket_another_copy_listens_on_is_kept() {
        let dir = private_folder();
        let path = dir.path().join(socket_name(true));
        let _first = bind(&path).unwrap();
        assert_eq!(bind(&path).unwrap_err().kind(), io::ErrorKind::AddrInUse);
    }

    #[test]
    fn another_user_or_another_binary_is_refused() {
        let own = std::env::current_exe().unwrap();
        assert!(check(501, 501, &own, &own).is_ok());
        assert_eq!(check(502, 501, &own, &own), Err(Refusal::OtherUser { uid: 502 }));
        let other = Path::new("/usr/bin/true");
        assert_eq!(check(501, 501, other, &own), Err(Refusal::OtherBinary { exe: other.to_path_buf() }));
    }

    #[test]
    fn a_peer_is_checked_by_its_real_binary() {
        let dir = private_folder();
        let path = dir.path().join(socket_name(true));
        let listener = bind(&path).unwrap();
        let _client = UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        // The test binary connected to itself: its own binary passes, another one does not.
        let own = std::env::current_exe().unwrap();
        assert_eq!(verify(&server, &own).unwrap().pid, std::process::id() as i32);
        assert!(matches!(verify(&server, Path::new("/usr/bin/true")), Err(Refusal::OtherBinary { .. })));
    }
}
