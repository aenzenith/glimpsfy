//! The system keyring on Linux, for real. A secret must reach the session's Secret Service and be
//! found, used and removed there, and a session without one must refuse it: no call may succeed
//! by keeping the secret anywhere else.
//!
//! The test never meets the user's keyring. It starts a session bus of its own, which activates
//! no service, and a GNOME Keyring whose home is a temporary folder. Where `dbus-daemon` or
//! `gnome-keyring-daemon` is not installed, the part that needs it is skipped with a note.
#![cfg(target_os = "linux")]

mod support;

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use glimpsfy_upload::engine::Stored;
use glimpsfy_upload::http::HttpClient;
use glimpsfy_upload::secrets::SecretStore;
use glimpsfy_upload::{providers, ProviderId, UploadData, UploadError, Uploader};
use serde_json::json;
use support::{Reply, TestServer};

const SECRET: &str = "imgbb-key-keyring-7c3e91";
/// A service prefix of the test's own, as a dev run has.
const PREFIX: &str = "com.aenzenith.glimpsfy.upload.test";

/// A process the test started, stopped when the test ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts `command`, or says that the program is not installed.
fn start(command: &mut Command) -> Option<Running> {
    match command.spawn() {
        Ok(child) => Some(Running(child)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("skipped: {:?} is not installed", command.get_program());
            None
        }
        Err(error) => panic!("{:?} did not start: {error}", command.get_program()),
    }
}

/// A session bus on `socket` that knows no service files, so nothing is started on demand.
fn session_bus(dir: &Path, socket: &Path) -> Option<Running> {
    let config = dir.join("bus.conf");
    fs::write(
        &config,
        format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>"#,
            socket.display()
        ),
    )
    .unwrap();
    let bus = start(Command::new("dbus-daemon").arg(format!("--config-file={}", config.display())).arg("--nofork"))?;
    let started = Instant::now();
    while !socket.exists() {
        assert!(started.elapsed() < Duration::from_secs(10), "the session bus did not open its socket");
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(bus)
}

/// A GNOME Keyring on the test's bus, whose login keyring is made and unlocked by the password
/// on its standard input.
fn keyring() -> Option<Running> {
    let mut daemon = start(
        Command::new("gnome-keyring-daemon")
            .args(["--foreground", "--unlock", "--components=secrets"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null()),
    )?;
    let mut stdin = daemon.0.stdin.take().expect("the daemon's standard input");
    stdin.write_all(b"a password of the test").unwrap();
    Some(daemon)
}

/// Every file below `dir` that holds `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for item in fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = item.path();
            match item.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(path),
                Ok(kind) if kind.is_file() => {
                    let bytes = fs::read(&path).unwrap_or_default();
                    if bytes.windows(needle.len()).any(|window| window == needle.as_bytes()) {
                        found.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    found
}

fn refused_for_lack_of_a_keyring(result: Result<impl std::fmt::Debug, UploadError>, what: &str) {
    match result {
        Err(UploadError::Keychain(reason)) => {
            assert!(reason.contains("no system keyring is available"), "{what}: {reason}");
            assert!(!reason.contains(SECRET), "{what}: the error carries the secret");
        }
        other => panic!("{what} did not fail for lack of a keyring: {other:?}"),
    }
}

#[test]
fn a_secret_lives_in_the_secret_service_and_nowhere_when_there_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let runtime = dir.path().join("run");
    let socket = dir.path().join("bus");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    // This is the only test of the binary, so no other thread reads the environment meanwhile.
    // Everything a keyring or a fallback could write to now lies below `dir`.
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    std::env::set_var("XDG_CACHE_HOME", home.join(".cache"));
    std::env::set_var("XDG_STATE_HOME", home.join(".local/state"));
    std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", socket.display()));

    let imgbb = ProviderId::new(providers::IMGBB);
    let server = TestServer::start(|request| {
        let origin = request.header("host").unwrap_or_default().to_string();
        Reply::json(200, json!({ "success": true, "status": 200, "data": { "url": format!("http://{origin}/a.png") } }))
    });
    let secrets = SecretStore::system().expect("the store is made without a keyring").with_service_prefix(PREFIX);
    let uploader = Uploader::new(glimpsfy_upload::builtin_providers_at(&server.url), secrets, HttpClient::for_tests());

    // A session without a bus (a headless login): every call fails, and nothing kept the secret.
    refused_for_lack_of_a_keyring(uploader.set_secret(&imgbb, "apiKey", SECRET), "set without a bus");
    refused_for_lack_of_a_keyring(uploader.secret_status(&imgbb, "apiKey"), "status without a bus");
    refused_for_lack_of_a_keyring(uploader.remove_secret(&imgbb, "apiKey"), "remove without a bus");

    // A session with a bus and no Secret Service (a bare window manager): the same.
    let Some(_bus) = session_bus(dir.path(), &socket) else { return };
    refused_for_lack_of_a_keyring(uploader.set_secret(&imgbb, "apiKey", SECRET), "set without a keyring");
    refused_for_lack_of_a_keyring(uploader.secret_status(&imgbb, "apiKey"), "status without a keyring");
    assert_eq!(files_holding(dir.path(), SECRET), Vec::<std::path::PathBuf>::new(), "a refused secret was written");

    // The keyring starts, and the store that failed before works without being made again.
    let Some(_keyring) = keyring() else { return };
    let started = Instant::now();
    while let Err(error) = uploader.secret_status(&imgbb, "apiKey") {
        assert!(started.elapsed() < Duration::from_secs(20), "the keyring did not answer: {error}");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!uploader.secret_status(&imgbb, "apiKey").expect("a status"));

    uploader.set_secret(&imgbb, "apiKey", SECRET).expect("the key is stored");
    assert!(uploader.secret_status(&imgbb, "apiKey").expect("a status"));
    // Another provider's key of the same name is another item.
    assert!(!uploader.secret_status(&ProviderId::new(providers::S3), "secretAccessKey").expect("a status"));

    // The engine reads the key back for a request: the host receives it.
    let values = json!({ "expiration": "0" });
    let confirmed = [uploader.consent_key(&imgbb, Some(&values)).expect("a consent key")];
    let stored = Stored { values: Some(&values), confirmed: &confirmed };
    let pending = uploader
        .begin(&imgbb, stored, UploadData::Bytes(b"png bytes".to_vec().into()), "shot.png", "image/png")
        .expect("an admitted upload");
    uploader.run(pending, &|_| {}).expect("the upload ran");
    let sent = server.requests();
    assert!(
        sent.iter().any(|request| String::from_utf8_lossy(&request.body).contains(SECRET)),
        "the host did not receive the stored key"
    );

    // Removing the key removes it from the keyring; removing it again is not an error.
    uploader.remove_secret(&imgbb, "apiKey").expect("the key is removed");
    assert!(!uploader.secret_status(&imgbb, "apiKey").expect("a status"));
    uploader.remove_secret(&imgbb, "apiKey").expect("a removed key is removed again");
}
