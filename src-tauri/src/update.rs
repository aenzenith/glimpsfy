//! Glimpsfy's own updates, after Pitwall's. A new version is asked for at the latest GitHub
//! release (`latest.json`, `tauri-plugin-updater`) once a day while Settings › General allows
//! it, downloaded in the background and checked against the public key in `tauri.conf.json`.
//! Nothing is installed until the user restarts for it from Settings or the popover: Glimpsfy
//! never restarts by itself, because a restart could end a recording or close a capture.
//!
//! The request carries the plugin's user agent and nothing of the user's.

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Error, UpdaterExt};

use crate::{trace, AppState};

/// To every window: the update's state changed. Payload: [`UpdateView`].
pub const UPDATE_CHANGED: &str = "update://changed";

/// How often a new version is asked for while the app runs.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The first ask waits until the app has settled.
const FIRST: Duration = Duration::from_secs(20);
/// A download's progress reaches the pages at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// What Settings and the popover show.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    /// The installed version.
    pub current: String,
    /// `off` (this build never asks: a development run) | `idle` | `checking` | `downloading` |
    /// `ready` | `available` (a new version this copy can't put in its own place) | `installing`
    /// | `failed`
    pub status: &'static str,
    /// The new version: from `downloading` on, and with `available` and `failed`.
    pub version: Option<String>,
    pub downloaded: u64,
    pub total: Option<u64>,
    /// When the release last answered, in ms since the epoch.
    pub checked_at: Option<u64>,
    /// The last ask got no answer.
    pub offline: bool,
    /// With `failed`: `download` | `signature` | `install`.
    pub error: Option<&'static str>,
    /// With `available`: the page of the latest release, where the user gets the new version
    /// that this copy cannot install by itself.
    pub release_url: Option<String>,
}

pub struct Update {
    view: Mutex<UpdateView>,
    /// The downloaded update, its signature checked, until it is installed.
    ready: Mutex<Option<(tauri_plugin_updater::Update, Vec<u8>)>>,
    /// One ask or download at a time.
    busy: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default()
}

/// The key updates are checked against.
fn public_key(app: &AppHandle) -> String {
    let plugins = &app.config().plugins.0;
    plugins
        .get("updater")
        .and_then(|updater| updater.get("pubkey"))
        .and_then(|key| key.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The page of the latest release: the updater's endpoint without the file it downloads.
fn release_page(app: &AppHandle) -> Option<String> {
    let plugins = &app.config().plugins.0;
    let endpoint = plugins.get("updater")?.get("endpoints")?.get(0)?.as_str()?;
    endpoint.strip_suffix("/download/latest.json").map(str::to_string)
}

/// The `.app` bundle the running executable sits in, when it sits in one.
#[cfg(target_os = "macos")]
fn install_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|extension| extension == "app");
    is_bundle.then(|| bundle.to_path_buf())
}

/// A release build running from an app bundle. A development run never replaces itself, and the
/// login item is written again only from such a build (`lib.rs`).
pub fn is_bundled() -> bool {
    #[cfg(target_os = "macos")]
    return !cfg!(debug_assertions) && install_path().is_some();
    #[cfg(not(target_os = "macos"))]
    crate::desktop::is_bundled()
}

/// Whether this copy can put a downloaded update in its own place. Where it cannot (a disk
/// image, a place the user may not write, a `.deb` or `.rpm` install that belongs to the package
/// manager), nothing is downloaded and the pages hear `available`.
fn can_replace_itself() -> bool {
    #[cfg(target_os = "macos")]
    return install_path().is_some_and(|path| !read_only_place(&path) && writable(&path));
    #[cfg(not(target_os = "macos"))]
    crate::desktop::can_self_update()
}

/// A disk image, or the randomised read-only place macOS runs a quarantined app from: nothing
/// can be replaced there, whatever the permissions say.
#[cfg(target_os = "macos")]
fn read_only_place(path: &Path) -> bool {
    let path = path.to_string_lossy();
    path.starts_with("/Volumes/") || path.contains("/AppTranslocation/")
}

/// The user may replace `path` without being asked for a password.
#[cfg(target_os = "macos")]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    let allowed = |path: &Path| {
        std::ffi::CString::new(path.as_os_str().as_bytes())
            // SAFETY: a NUL-terminated path; access only reads it.
            .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0)
    };
    allowed(path) && path.parent().is_some_and(allowed)
}

/// Which step a failed download failed at.
fn download_error(error: &Error) -> &'static str {
    match error {
        Error::Minisign(_) | Error::SignedVersionMismatch { .. } | Error::MissingSignedVersion => "signature",
        _ => "download",
    }
}

impl Update {
    pub fn new(app: &AppHandle) -> Self {
        let on = is_bundled() && !public_key(app).is_empty();
        Self {
            view: Mutex::new(UpdateView {
                current: app.package_info().version.to_string(),
                status: if on { "idle" } else { "off" },
                version: None,
                downloaded: 0,
                total: None,
                checked_at: None,
                offline: false,
                error: None,
                release_url: None,
            }),
            ready: Mutex::new(None),
            busy: AtomicBool::new(false),
        }
    }

    pub fn view(&self) -> UpdateView {
        lock(&self.view).clone()
    }

    fn set(&self, app: &AppHandle, change: impl FnOnce(&mut UpdateView)) {
        let view = {
            let mut view = lock(&self.view);
            change(&mut view);
            view.clone()
        };
        let _ = app.emit(UPDATE_CHANGED, view);
    }

    /// Asks by itself for as long as the app runs, unless Settings turned that off.
    pub fn start(app: &AppHandle) {
        let app = app.clone();
        thread::spawn(move || {
            thread::sleep(FIRST);
            loop {
                let allowed =
                    app.try_state::<AppState>().is_some_and(|state| state.settings.get().general.check_for_updates);
                if allowed {
                    if let Some(update) = app.try_state::<Update>() {
                        update.check(&app);
                    }
                }
                thread::sleep(EVERY);
            }
        });
    }

    /// Asks for a new version now and downloads one that is there. The answer arrives as
    /// `update://changed` events.
    pub fn check(&self, app: &AppHandle) {
        if lock(&self.view).status == "off" || self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        // An update that waits stays in sight while a newer one is asked for.
        self.set(app, |view| {
            if !matches!(view.status, "ready" | "available") {
                view.status = "checking";
                view.error = None;
            }
        });

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(update) = app.try_state::<Update>() {
                update.ask(&app).await;
                update.busy.store(false, Ordering::SeqCst);
            }
        });
    }

    async fn ask(&self, app: &AppHandle) {
        let found = match app.updater() {
            Ok(updater) => updater.check().await,
            Err(error) => Err(error),
        };
        let waiting = lock(&self.ready).as_ref().map(|(update, _)| update.version.clone());
        // What the status falls back to: an update already downloaded still waits.
        let rest = |view: &mut UpdateView| {
            (view.status, view.version) = match &waiting {
                Some(version) => ("ready", Some(version.clone())),
                None => ("idle", None),
            };
        };

        let update = match found {
            Ok(Some(update)) if waiting.as_deref() != Some(update.version.as_str()) => update,
            // Nothing newer than what runs or waits, or no build of this release for this system.
            Ok(_) | Err(Error::TargetNotFound(_) | Error::TargetsNotFound(_)) => {
                return self.set(app, |view| {
                    rest(view);
                    view.checked_at = Some(now_ms());
                    view.offline = false;
                });
            }
            Err(error) => {
                trace::log(format!("[glimpsfy] update: {error}"));
                // No answer: a version this copy can't put in its place is still out.
                return self.set(app, |view| {
                    if view.status != "available" {
                        rest(view);
                    }
                    view.offline = true;
                });
            }
        };

        let version = update.version.clone();
        if !can_replace_itself() {
            let page = release_page(app);
            return self.set(app, |view| {
                view.status = "available";
                view.release_url = page;
                view.version = Some(version);
                view.checked_at = Some(now_ms());
                view.offline = false;
            });
        }

        self.set(app, |view| {
            view.status = "downloading";
            view.version = Some(version.clone());
            view.downloaded = 0;
            view.total = None;
            view.checked_at = Some(now_ms());
            view.offline = false;
        });

        let mut sent = Instant::now();
        let progress = |chunk: usize, total: Option<u64>| {
            let view = {
                let mut view = lock(&self.view);
                view.downloaded += chunk as u64;
                view.total = total;
                view.clone()
            };
            if sent.elapsed() >= PROGRESS_EVERY {
                sent = Instant::now();
                let _ = app.emit(UPDATE_CHANGED, view);
            }
        };

        match update.download(progress, || {}).await {
            Ok(bytes) => {
                *lock(&self.ready) = Some((update, bytes));
                self.set(app, |view| view.status = "ready");
            }
            Err(error) => {
                trace::log(format!("[glimpsfy] update: {error}"));
                let step = download_error(&error);
                // The one downloaded before still waits; the newer one is asked for again later.
                self.set(app, |view| match &waiting {
                    Some(version) => {
                        view.status = "ready";
                        view.version = Some(version.clone());
                    }
                    None => {
                        view.status = "failed";
                        view.error = Some(step);
                    }
                });
            }
        }
    }

    /// The user restarts for the update: it is installed and the app started again. Returns only
    /// when that failed; the update then still waits.
    pub fn install(&self, app: &AppHandle) -> Result<(), String> {
        let Some((update, bytes)) = lock(&self.ready).take() else {
            return Err("no update is downloaded".into());
        };
        self.set(app, |view| view.status = "installing");
        match update.install(&bytes) {
            Ok(()) => app.restart(),
            Err(error) => {
                trace::log(format!("[glimpsfy] update: {error}"));
                let message = error.to_string();
                *lock(&self.ready) = Some((update, bytes));
                self.set(app, |view| {
                    view.status = "failed";
                    view.error = Some("install");
                });
                Err(message)
            }
        }
    }
}
