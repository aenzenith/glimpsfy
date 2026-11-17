//! Upload commands: the providers and their forms, the first-upload confirmation, uploads with
//! progress and cancellation, deletion from the host, connection tests, secrets and `.sxcu`
//! import. Owner: sharing (milestone 4). The engine is `crates/upload` (`Uploader`).
//!
//! Nothing leaves the machine on its own. An upload, a test or a deletion starts only from one of
//! these commands, which a user action calls, and the engine refuses each one until the user has
//! confirmed the provider's destination once (`upload_confirm_provider`, called by
//! `src/upload/FirstUploadDialog.vue`). No provider is the default until the user picks one.
//!
//! An upload runs on a thread of its own. The starting command returns its id at once
//! ([`UploadStarted`]); `upload://progress` follows, then exactly one of `upload://done` and
//! `upload://failed`. After a success the link goes to the clipboard as Settings › Upload says
//! (the bare link or a Markdown image) and opens in the browser when that is on.
//!
//! A capture that leaves the overlay as an upload is recorded in the history first, with its
//! image, so the link lands on an entry that still shows what was uploaded, and a failure can be
//! tried again from that entry's file (`upload://failed` names the file and the entry).
//!
//! Secrets go to the keychain through `upload_set_secret` and never come back: the status command
//! says only whether one is stored, and no event, log line or error carries one.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use glimpsfy_capture::{Frame, ImageEncoding, PngEffort};
use glimpsfy_history::{EntryId, EntryKind};
use glimpsfy_upload::consent::ConsentInfo;
use glimpsfy_upload::engine::{PendingUpload, Stored, SxcuImported};
use glimpsfy_upload::http::HttpClient;
use glimpsfy_upload::secrets::SecretStore;
use glimpsfy_upload::{
    DeleteOutcome, DeletionHandle, Issue, ProviderDescription, ProviderId, ProviderTestResult, UploadData, UploadError,
    UploadId, UploadProgress, Uploader,
};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::capture::{CloseReason, Selection};
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::output::{file_stem_titled, Naming, Origin};
use crate::settings::{CopyAfterUpload, RetinaScale, Settings, SettingsStore};
use crate::{session, trace, AppState};

use self::events::{UploadDone, UploadFailed};

impl From<UploadError> for CommandError {
    fn from(error: UploadError) -> Self {
        let code = match &error {
            UploadError::NotImplemented(_) => ErrorCode::NotImplemented,
            UploadError::UnknownProvider(_) => ErrorCode::NotFound,
            UploadError::NoProviderChosen | UploadError::InvalidConfig { .. } | UploadError::MissingSecret { .. } => {
                ErrorCode::InvalidArgument
            }
            // The user has not allowed this destination yet: the page shows the confirmation.
            UploadError::NotConfirmed { .. } => ErrorCode::PermissionDenied,
            UploadError::Provider { .. }
            | UploadError::Network(_)
            | UploadError::FileTooLarge { .. }
            | UploadError::HostNotAllowed { .. } => ErrorCode::Provider,
            UploadError::Cancelled => ErrorCode::Cancelled,
            UploadError::Keychain(_) => ErrorCode::Io,
            UploadError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            UploadError::Io(_) => ErrorCode::Io,
        };
        CommandError::new(code, error.to_string())
    }
}

/// Event names of the upload area and their payloads.
pub mod events {
    use glimpsfy_history::EntryId;
    use glimpsfy_upload::{ProviderId, UploadId, UploadResult};
    use serde::Serialize;

    use crate::commands::error::CommandError;
    use crate::settings::CopyAfterUpload;

    /// Bytes sent so far, at most ten times a second. The first event, with zero bytes, comes
    /// before the connection opens. Payload: `glimpsfy_upload::UploadProgress`.
    pub const PROGRESS: &str = "upload://progress";
    /// The host returned the link. Payload: [`UploadDone`].
    pub const DONE: &str = "upload://done";
    /// The upload failed or was cancelled; no link exists. Payload: [`UploadFailed`].
    pub const FAILED: &str = "upload://failed";

    /// What the history records and the pages show after a successful upload.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct UploadDone {
        /// The link, the deletion handle (`deletion`, for `upload_delete`), when it was uploaded
        /// and when the host deletes it on its own (`expiresAtMs`, `null` when it does not say).
        pub result: UploadResult,
        /// The history entry that now records the hosted copy: the one `upload_file` was given,
        /// or the one an upload from the overlay recorded first. The history's own listener
        /// records the link on it.
        pub entry_id: Option<EntryId>,
        /// The file name the host received.
        pub file_name: String,
        /// What went to the clipboard: `link` or `markdown`, or `null` when nothing did (the
        /// settings say `nothing`, or the clipboard refused).
        pub copied: Option<CopyAfterUpload>,
        /// Whether the link opened in the browser.
        pub opened: bool,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct UploadFailed {
        pub upload_id: UploadId,
        pub provider: ProviderId,
        /// `cancelled` when the user cancelled and `provider` when the host refused; the message
        /// then carries the host's own reason (English, with every secret cut out).
        pub error: CommandError,
        /// The failure in one word, for the page's sentence: `cancelled`, `provider`, `network`,
        /// `fileTooLarge`, `hostNotAllowed`, `notConfirmed`, `missingSecret`, `invalidConfig`,
        /// `keychain`, `io` or `other`.
        pub kind: &'static str,
        /// The HTTP status of the host's refusal, when it answered.
        pub status: Option<u16>,
        /// The file to send again for Retry (`upload_file`): the uploaded file, or for a capture
        /// sent from memory its history entry's image or a staged copy. `None` when nothing is
        /// left to retry from.
        pub path: Option<String>,
        /// The history entry the upload belongs to, which Retry passes on so the link still lands
        /// there.
        pub entry_id: Option<EntryId>,
    }
}

// ------------------------------------------------------------------------------- the engine

static UPLOADER: OnceLock<Result<Uploader, String>> = OnceLock::new();
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Keeps the app handle for `upload_delete`, which the history calls without one.
pub fn setup(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

/// The one engine, with the system keychain and the shared HTTP client. It is made on first use,
/// so the app starts without touching the keychain.
fn uploader() -> CommandResult<&'static Uploader> {
    UPLOADER
        .get_or_init(|| {
            let mut secrets = SecretStore::system().map_err(|error| error.to_string())?;
            if let Some(prefix) = crate::paths::keychain_prefix() {
                secrets = secrets.with_service_prefix(&prefix);
            }
            let client = HttpClient::new(env!("CARGO_PKG_VERSION"));
            Ok(Uploader::new(glimpsfy_upload::builtin_providers(), secrets, client))
        })
        .as_ref()
        .map_err(|reason| CommandError::new(ErrorCode::NotImplemented, reason.clone()))
}

/// The settings an engine call reads for `provider`.
fn stored<'a>(settings: &'a Settings, provider: &ProviderId) -> Stored<'a> {
    Stored { values: settings.upload.providers.get(provider.as_str()), confirmed: &settings.upload.confirmed_providers }
}

/// The provider a call names, or the default the user chose.
fn chosen(provider: Option<ProviderId>, settings: &Settings) -> Result<ProviderId, UploadError> {
    provider
        .or_else(|| settings.upload.default_provider.clone().map(ProviderId::new))
        .ok_or(UploadError::NoProviderChosen)
}

async fn off_main<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| CommandError::internal(e.to_string()))?
}

// ------------------------------------------------------------------------- forms and consent

/// Every provider the app offers, with its settings form (`crates/upload/src/schema.rs`).
#[tauri::command]
pub async fn upload_providers() -> CommandResult<Vec<ProviderDescription>> {
    Ok(glimpsfy_upload::builtin_providers().iter().map(|provider| provider.describe()).collect())
}

/// The problems of a provider's form: `values` as the form holds them now, or the saved ones
/// when `null`. Secrets are read from the keychain, so a required one that is missing shows.
#[tauri::command]
pub async fn upload_validate(
    app: AppHandle,
    provider_id: ProviderId,
    values: Option<Value>,
) -> CommandResult<Vec<Issue>> {
    off_main(move || {
        let settings = app.state::<AppState>().settings.get();
        let values = values.or_else(|| settings.upload.providers.get(provider_id.as_str()).cloned());
        Ok(uploader()?.validate(&provider_id, values.as_ref())?)
    })
    .await
}

/// What a page checks before it starts an upload or a test.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadPreflight {
    pub provider: ProviderId,
    /// What the first-upload dialog shows, and whether the user confirmed it already.
    pub consent: ConsentInfo,
    /// The form's problems; an `error` among them stops the upload (a missing key, say).
    pub issues: Vec<Issue>,
}

/// The provider an upload would use now (the named one, or the default), what its first-upload
/// confirmation says, and what stops it. Fails with `invalid_argument` when no provider is named
/// and none is the default yet.
#[tauri::command]
pub async fn upload_preflight(app: AppHandle, provider_id: Option<ProviderId>) -> CommandResult<UploadPreflight> {
    off_main(move || {
        let settings = app.state::<AppState>().settings.get();
        let provider = chosen(provider_id, &settings)?;
        let uploader = uploader()?;
        let consent = uploader.consent_info(&provider, stored(&settings, &provider))?;
        let issues = uploader.validate(&provider, settings.upload.providers.get(provider.as_str()))?;
        Ok(UploadPreflight { provider, consent, issues })
    })
    .await
}

/// Records that the user confirmed uploads to the provider's current destination, after they
/// pressed Upload in the first-upload dialog. `consentKey` is the one the dialog showed: when the
/// settings point somewhere else by now, nothing is recorded and the call fails, so the dialog
/// asks again with the new address.
#[tauri::command]
pub async fn upload_confirm_provider(
    app: AppHandle,
    provider_id: ProviderId,
    consent_key: String,
) -> CommandResult<ConsentInfo> {
    off_main(move || {
        let state = app.state::<AppState>();
        let settings = confirm(uploader()?, &state.settings, &provider_id, &consent_key)?;
        let _ = app.emit(crate::events::SETTINGS_CHANGED, settings.clone());
        Ok(uploader()?.consent_info(&provider_id, stored(&settings, &provider_id))?)
    })
    .await
}

/// Adds the consent key to the settings when it is still the provider's current one. The key is
/// checked against the settings under the store's lock (`SettingsStore::update`), so a page that
/// saves another address at the same moment can neither be undone by this nor slip an
/// unconfirmed address past it.
fn confirm(
    uploader: &Uploader,
    store: &SettingsStore,
    provider: &ProviderId,
    consent_key: &str,
) -> CommandResult<Settings> {
    let current = store.get();
    let known = uploader.consent_key(provider, current.upload.providers.get(provider.as_str()))?;
    if known == consent_key && current.upload.confirmed_providers.iter().any(|k| k == consent_key) {
        return Ok(current);
    }
    let mut outcome: CommandResult<()> = Ok(());
    let (saved, _) = store.update(|settings| {
        match uploader.consent_key(provider, settings.upload.providers.get(provider.as_str())) {
            Ok(key) if key == consent_key => {
                if !settings.upload.confirmed_providers.contains(&key) {
                    settings.upload.confirmed_providers.push(key);
                }
            }
            Ok(_) => {
                outcome = Err(CommandError::invalid(
                    "the provider's address changed after the confirmation opened; review the new address and confirm again",
                ))
            }
            Err(error) => outcome = Err(error.into()),
        }
    })?;
    outcome.map(|()| saved)
}

// ---------------------------------------------------------------------------------- uploads

/// The answer of a starting command: the upload runs, and its events carry this id.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadStarted {
    pub upload_id: UploadId,
    pub provider: ProviderId,
    pub file_name: String,
    pub total_bytes: u64,
}

/// Uploads the selection of a capture as a PNG to the named provider, or to the default one.
/// Every check runs before the capture closes, so a refusal (`permission_denied` when the
/// destination is not confirmed yet, `invalid_argument` for a missing key or no provider) leaves
/// the overlay open. Once admitted, the capture closes as `uploaded` and the history records it
/// with its image before the upload starts, so the link lands on that entry. Returns the upload's
/// id; the outcome arrives as events.
#[tauri::command]
pub async fn upload_capture(
    app: AppHandle,
    selection: Selection,
    provider_id: Option<ProviderId>,
) -> CommandResult<UploadStarted> {
    off_main(move || upload_selection(&app, selection, provider_id)).await
}

fn upload_selection(
    app: &AppHandle,
    selection: Selection,
    provider_id: Option<ProviderId>,
) -> CommandResult<UploadStarted> {
    let settings = app.state::<AppState>().settings.get();
    let provider = chosen(provider_id, &settings)?;
    // The same pixels Copy takes: the picked window's own image, or the crop of the frame.
    let (frame, origin) = crate::output::take(app, &selection)?;
    let frame = match settings.output.retina {
        RetinaScale::ScaleTo1x => frame.at_1x().unwrap_or(frame),
        RetinaScale::Keep2x => frame,
    };
    let file_name = capture_file_name(&settings, &origin, &frame);
    let png: Arc<[u8]> = frame.encode(ImageEncoding::Png(PngEffort::Balanced))?.into();
    let pending = uploader()?.begin(
        &provider,
        stored(&settings, &provider),
        UploadData::Bytes(png.clone()),
        &file_name,
        "image/png",
    )?;
    session::close(app, selection.capture_id, CloseReason::Uploaded);
    let kept = Kept { width: frame.size.width, height: frame.size.height, scale: frame.scale.0 };
    let source = keep_capture(app, &origin, kept, &png, &file_name, None);
    start(app, &settings, pending, source)
}

/// The name Settings › Output gives a capture, as the host receives it.
fn capture_file_name(settings: &Settings, origin: &Origin, frame: &Frame) -> String {
    let (date, time) = Naming::clock(chrono::DateTime::<chrono::Local>::from(origin.taken_at));
    let naming =
        Naming { date, time, app: origin.app_name.as_deref(), width: frame.size.width, height: frame.size.height };
    let stem = file_stem_titled(&settings.output.file_name_template, &naming, origin.window_title.as_deref(), 1);
    format!("{stem}.png")
}

/// Uploads the staged PNG of a capture with drawings (`upload_capture_rendered`): the checks run
/// first and leave the capture open on a refusal, as `upload_capture`'s do; once admitted, the
/// capture closes as `uploaded` and the history records the image before the upload starts.
pub(crate) fn upload_rendered_capture(
    app: &AppHandle,
    path: PathBuf,
    provider_id: Option<ProviderId>,
    selection: Selection,
) -> CommandResult<UploadStarted> {
    let settings = app.state::<AppState>().settings.get();
    let provider = chosen(provider_id, &settings)?;
    let (file_name, content_type) = file_type(&path)?;
    let pending = uploader()?.begin(
        &provider,
        stored(&settings, &provider),
        UploadData::File(path.clone()),
        &file_name,
        content_type,
    )?;
    let origin = Origin::of_selection(app, &selection);
    session::close(app, selection.capture_id, CloseReason::Uploaded);
    let source = match std::fs::read(&path) {
        Ok(png) => {
            let (width, height) = png_size(&png).unwrap_or((0, 0));
            let scale = match settings.output.retina {
                RetinaScale::ScaleTo1x => 1.0,
                RetinaScale::Keep2x => origin.display.as_ref().map_or(1.0, |d| d.scale.0),
            };
            keep_capture(app, &origin, Kept { width, height, scale }, &png, &file_name, Some(path))
        }
        // The upload reads the same file and will report the failure itself.
        Err(_) => Source { path: Some(path), ..Source::default() },
    };
    start(app, &settings, pending, source)
}

/// The width and height of a PNG, from its `IHDR` chunk.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let ihdr = png.get(12..24).filter(|h| &h[..4] == b"IHDR")?;
    Some((u32::from_be_bytes(ihdr[4..8].try_into().ok()?), u32::from_be_bytes(ihdr[8..12].try_into().ok()?)))
}

/// The size and scale of an image the history keeps.
#[derive(Clone, Copy, Debug)]
struct Kept {
    width: u32,
    height: u32,
    scale: f64,
}

/// Records a capture that leaves as an upload in the history, with its image, and says what a
/// failed upload can be tried again from: the entry's own file, else `staged` (a file that
/// already exists), else a copy staged in the cache, because the overlay is gone and the pixels
/// would be lost with a failure.
fn keep_capture(
    app: &AppHandle,
    origin: &Origin,
    kept: Kept,
    png: &[u8],
    file_name: &str,
    staged: Option<PathBuf>,
) -> Source {
    let handoff = crate::commands::history::CaptureHandoff {
        kind: Some(EntryKind::Screenshot),
        width: (kept.width > 0).then_some(kept.width),
        height: (kept.height > 0).then_some(kept.height),
        scale: Some(kept.scale),
        bytes: Some(png.len() as u64),
        app_name: origin.app_name.clone(),
        bundle_id: origin.bundle_id.clone(),
        window_title: origin.window_title.clone(),
        display_id: origin.display.as_ref().map(|d| d.id.0),
        display_name: origin.display.as_ref().map(|d| d.name.clone()),
        created_at_ms: origin.taken_at.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64),
        png: Some(png.to_vec()),
        ..Default::default()
    };
    let entry = match crate::commands::history::record_capture(app, handoff) {
        Ok(entry) => entry,
        Err(error) => {
            trace::log(format!("[glimpsfy] upload: the capture was not recorded in the history: {}", error.message));
            None
        }
    };
    let entry_id = entry.as_ref().map(|e| e.id.clone());
    if let Some(path) = entry.as_ref().and_then(|e| e.media_path()).map(Path::to_path_buf) {
        return Source { entry_id, path: Some(path), staged: false };
    }
    if let Some(path) = staged {
        return Source { entry_id, path: Some(path), staged: false };
    }
    let path = stage_for_retry(app, png, file_name);
    Source { entry_id, staged: path.is_some(), path }
}

/// The cache folder for copies of uploads that exist nowhere else (the history is off).
const RETRY_FOLDER: &str = "uploads";
/// A staged copy older than this is deleted by the next one; the upload is long over.
const RETRY_KEEP: Duration = Duration::from_secs(24 * 60 * 60);

/// Writes `png` under its own name into a fresh folder in the cache, so Retry sends it under the
/// same name. Folders older than a day are removed first.
fn stage_for_retry(app: &AppHandle, png: &[u8], file_name: &str) -> Option<PathBuf> {
    let root = crate::paths::cache_dir(app).ok()?.join(RETRY_FOLDER);
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age > RETRY_KEEP);
            if old {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let folder = root.join(format!("{nanos:x}"));
    std::fs::create_dir_all(&folder).ok()?;
    let path = folder.join(file_name);
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// The files `upload_file` sends: images and recordings, so a page cannot upload any file of the
/// disk by its path.
const FILE_TYPES: [(&str, &str); 9] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
];

/// Uploads a file from disk: a history entry's image, a recording or an exported GIF. With
/// `entryId`, the history records the link on that entry when the upload is done. Returns the
/// upload's id; the outcome arrives as events.
#[tauri::command]
pub async fn upload_file(
    app: AppHandle,
    path: PathBuf,
    provider_id: Option<ProviderId>,
    entry_id: Option<EntryId>,
) -> CommandResult<UploadStarted> {
    off_main(move || {
        let settings = app.state::<AppState>().settings.get();
        let provider = chosen(provider_id, &settings)?;
        let (file_name, content_type) = file_type(&path)?;
        let path_for_retry = path.clone();
        let pending = uploader()?.begin(
            &provider,
            stored(&settings, &provider),
            UploadData::File(path),
            &file_name,
            content_type,
        )?;
        start(&app, &settings, pending, Source { entry_id, path: Some(path_for_retry), staged: false })
    })
    .await
}

fn file_type(path: &Path) -> CommandResult<(String, &'static str)> {
    if !path.is_file() {
        return Err(CommandError::not_found(format!("{} is not a file", path.display())));
    }
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let content_type = FILE_TYPES.iter().find(|(e, _)| *e == extension).map(|(_, t)| *t).ok_or_else(|| {
        CommandError::invalid("only images and recordings upload (png, jpg, gif, webp, mp4, mov and webm files)")
    })?;
    let name =
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| format!("file.{extension}"));
    Ok((name, content_type))
}

/// What happens after a successful upload, from the settings at its start.
#[derive(Clone, Copy, Debug)]
pub(crate) struct After {
    pub copy: CopyAfterUpload,
    pub open: bool,
}

/// Where an upload's bytes live besides the request: the history entry that gets the link, and
/// the file a failed upload can be sent again from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Source {
    pub entry_id: Option<EntryId>,
    pub path: Option<PathBuf>,
    /// `path` is a copy staged only for Retry, deleted once the upload succeeded or was cancelled.
    pub staged: bool,
}

/// Runs an admitted upload on its own thread and returns its id.
fn start(app: &AppHandle, settings: &Settings, pending: PendingUpload, source: Source) -> CommandResult<UploadStarted> {
    let uploader = uploader()?;
    let started = UploadStarted {
        upload_id: pending.id.clone(),
        provider: pending.provider.clone(),
        file_name: pending.file_name.clone(),
        total_bytes: pending.total_bytes,
    };
    let after = After { copy: settings.upload.copy_after_upload, open: settings.upload.open_in_browser };
    let sink = AppSink(app.clone());
    std::thread::Builder::new()
        .name("glimpsfy-upload".into())
        .spawn(move || run(uploader, pending, &sink, after, source))
        .map_err(|error| {
            uploader.abandon(&started.upload_id);
            CommandError::internal(format!("the upload could not start: {error}"))
        })?;
    Ok(started)
}

/// Where an upload's outcome goes: the pages, the clipboard, the browser and the log. The app
/// sends it through Tauri; the tests record it.
pub(crate) trait Sink {
    fn progress(&self, progress: &UploadProgress);
    fn done(&self, done: &UploadDone);
    fn failed(&self, failed: &UploadFailed);
    fn copy_text(&self, text: &str) -> Result<(), String>;
    fn open_url(&self, url: &str) -> Result<(), String>;
    fn log(&self, line: &str);
}

struct AppSink(AppHandle);

impl Sink for AppSink {
    fn progress(&self, progress: &UploadProgress) {
        let _ = self.0.emit(events::PROGRESS, progress);
    }

    fn done(&self, done: &UploadDone) {
        let _ = self.0.emit(events::DONE, done);
    }

    fn failed(&self, failed: &UploadFailed) {
        let _ = self.0.emit(events::FAILED, failed);
    }

    fn copy_text(&self, text: &str) -> Result<(), String> {
        crate::commands::windows::app_copy_text(self.0.clone(), text.to_string()).map_err(|e| e.message)
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        crate::commands::windows::app_open_url(self.0.clone(), url.to_string()).map_err(|e| e.message)
    }

    fn log(&self, line: &str) {
        trace::log(line);
    }
}

/// How often progress reaches the pages.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Sends an admitted upload and reports it: progress, then either the link (copied and opened as
/// `after` says) or the failure. A failed or cancelled upload reports no link at all.
pub(crate) fn run(uploader: &Uploader, pending: PendingUpload, sink: &dyn Sink, after: After, source: Source) {
    let Source { entry_id, path, staged } = source;
    // The file, then its own folder only when that is empty, so nothing else can go with it.
    let drop_staged = || {
        if let (true, Some(path)) = (staged, path.as_deref()) {
            let _ = std::fs::remove_file(path);
            if let Some(folder) = path.parent() {
                let _ = std::fs::remove_dir(folder);
            }
        }
    };
    let upload_id = pending.id.clone();
    let provider = pending.provider.clone();
    let file_name = pending.file_name.clone();
    sink.progress(&UploadProgress {
        upload_id: upload_id.clone(),
        provider: provider.clone(),
        sent_bytes: 0,
        total_bytes: pending.total_bytes,
    });
    let last = Cell::new(Instant::now());
    let report = |progress: UploadProgress| {
        if progress.sent_bytes >= progress.total_bytes || last.get().elapsed() >= PROGRESS_EVERY {
            last.set(Instant::now());
            sink.progress(&progress);
        }
    };
    match uploader.run(pending, &report) {
        Ok(result) => {
            let copied = match after.copy {
                CopyAfterUpload::Nothing => None,
                kind => {
                    let text = match kind {
                        CopyAfterUpload::Markdown => markdown_image(&file_name, &result.url),
                        _ => result.url.clone(),
                    };
                    match sink.copy_text(&text) {
                        Ok(()) => Some(kind),
                        Err(reason) => {
                            sink.log(&format!("[glimpsfy] upload {upload_id}: the link was not copied: {reason}"));
                            None
                        }
                    }
                }
            };
            let opened = after.open
                && match sink.open_url(&result.url) {
                    Ok(()) => true,
                    Err(reason) => {
                        sink.log(&format!("[glimpsfy] upload {upload_id}: the link did not open: {reason}"));
                        false
                    }
                };
            sink.log(&format!("[glimpsfy] upload {upload_id} to {provider} is done"));
            drop_staged();
            sink.done(&UploadDone { result, entry_id, file_name, copied, opened });
        }
        Err(error) => {
            sink.log(&format!("[glimpsfy] upload {upload_id} to {provider} failed: {error}"));
            let kind = failure_kind(&error);
            let (status, reason) = match &error {
                UploadError::Provider { status, reason, .. } => (*status, Some(reason.clone())),
                _ => (None, None),
            };
            // Nobody retries a cancelled upload, so its staged copy goes now.
            if kind == "cancelled" {
                drop_staged();
            }
            let path = path.map(|p| p.to_string_lossy().into_owned());
            let mut error = CommandError::from(error);
            // The page's sentence already names the host and the status; the line under it is
            // the host's own words alone, not the log line around them.
            if let Some(reason) = reason.filter(|r| !r.trim().is_empty()) {
                error.message = reason;
            }
            sink.failed(&UploadFailed { upload_id, provider, error, kind, status, path, entry_id });
        }
    }
}

fn failure_kind(error: &UploadError) -> &'static str {
    match error {
        UploadError::Cancelled => "cancelled",
        UploadError::Provider { .. } => "provider",
        UploadError::Network(_) => "network",
        UploadError::FileTooLarge { .. } => "fileTooLarge",
        UploadError::HostNotAllowed { .. } => "hostNotAllowed",
        UploadError::NotConfirmed { .. } => "notConfirmed",
        UploadError::MissingSecret { .. } => "missingSecret",
        UploadError::InvalidConfig { .. } => "invalidConfig",
        UploadError::Keychain(_) => "keychain",
        UploadError::Io(_) => "io",
        _ => "other",
    }
}

/// `![name](link)`: the file name without its extension as the alternative text.
fn markdown_image(file_name: &str, url: &str) -> String {
    let stem = file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(file_name);
    let alt: String =
        stem.chars().flat_map(|c| if matches!(c, '[' | ']' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
    if url.contains(['(', ')', ' ', '<', '>']) {
        format!("![{alt}](<{url}>)")
    } else {
        format!("![{alt}]({url})")
    }
}

/// Cancels a running upload; `upload://failed` with `cancelled` follows. Fails with `not_found`
/// when no upload with this id is running (it finished a moment ago).
#[tauri::command]
pub async fn upload_cancel(upload_id: UploadId) -> CommandResult<()> {
    if uploader()?.cancel(&upload_id) {
        Ok(())
    } else {
        Err(CommandError::not_found(format!("no upload {upload_id} is running")))
    }
}

// ---------------------------------------------------------------------------- delete and test

/// Deletes the hosted copy of a history entry. A host that deletes with a request (0x0.st, an S3
/// bucket) deletes it here. A host that deletes on its own page (ImgBB, a custom uploader's
/// deletion link) gets that page opened in the browser, and the call fails with `provider`,
/// because the copy is gone only once the user confirms there; the entry keeps its link.
#[tauri::command]
pub async fn upload_delete(entry_id: EntryId) -> CommandResult<()> {
    let app = APP.get().cloned().ok_or_else(|| CommandError::internal("the upload area is not set up yet"))?;
    off_main(move || {
        let entry = crate::commands::history::store(&app)?.get(&entry_id)?;
        let hosted = entry.hosted.ok_or_else(|| CommandError::not_found("the entry has no hosted copy"))?;
        let deletion = hosted
            .deletion
            .ok_or_else(|| CommandError::invalid(format!("{} gives no way to delete an upload", hosted.provider)))?;
        match delete_hosted(&app, &hosted.provider, &deletion)? {
            DeleteOutcome::Deleted => Ok(()),
            DeleteOutcome::OpenPage { url } => {
                crate::commands::windows::app_open_url(app.clone(), url)?;
                Err(CommandError::new(
                    ErrorCode::Provider,
                    format!(
                        "{} deletes an upload on its own page, which is open in the browser now; the entry keeps its link until it is deleted there",
                        hosted.provider
                    ),
                ))
            }
        }
    })
    .await
}

/// Deletes a hosted copy from its host, for the history: `Deleted` once the host confirmed, or
/// the page to open when the host deletes on its own page. Call it from a blocking thread.
pub fn delete_hosted(
    app: &AppHandle,
    provider: &ProviderId,
    deletion: &DeletionHandle,
) -> CommandResult<DeleteOutcome> {
    let settings = app.state::<AppState>().settings.get();
    Ok(uploader()?.delete(provider, stored(&settings, provider), deletion)?)
}

/// Checks a provider's settings and secrets against its host ("Test the connection"). It sends a
/// request, so it needs the confirmation like an upload (`permission_denied` until then). ImgBB
/// and a custom uploader receive a 1 × 1 test image; an S3 bucket receives a small text file,
/// which is deleted again.
#[tauri::command]
pub async fn upload_test(app: AppHandle, provider_id: ProviderId) -> CommandResult<ProviderTestResult> {
    off_main(move || {
        let settings = app.state::<AppState>().settings.get();
        Ok(uploader()?.test(&provider_id, stored(&settings, &provider_id))?)
    })
    .await
}

// ---------------------------------------------------------------------------------- secrets

/// Stores a provider's secret in the keychain: a `secret` row's key (`apiKey`), or one value of
/// a secret `pairs` row (`headers:Authorization`). The value is never written anywhere else.
#[tauri::command]
pub async fn upload_set_secret(provider_id: ProviderId, key: String, value: String) -> CommandResult<()> {
    off_main(move || Ok(uploader()?.set_secret(&provider_id, &key, &value)?)).await
}

/// Removes a provider's secret from the keychain. Removing one that is not there succeeds.
#[tauri::command]
pub async fn upload_remove_secret(provider_id: ProviderId, key: String) -> CommandResult<()> {
    off_main(move || Ok(uploader()?.remove_secret(&provider_id, &key)?)).await
}

/// Whether the keychain holds the secret, without reading it out (the form shows "Stored in the
/// keychain" and an empty field).
#[tauri::command]
pub async fn upload_secret_status(provider_id: ProviderId, key: String) -> CommandResult<bool> {
    off_main(move || Ok(uploader()?.secret_status(&provider_id, &key)?)).await
}

// ------------------------------------------------------------------------------------ .sxcu

/// The outcome of an import: what the file gave, or why it was refused.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SxcuOutcome {
    /// The custom provider's new settings, saved already, and the keychain keys written.
    pub imported: Option<SxcuImported>,
    /// Why the file was refused: the row it concerns and a key of `upload.json`. Nothing was
    /// saved then.
    pub refusal: Option<Issue>,
}

/// Imports a ShareX `.sxcu` file (its text) into the custom provider: the request, the response
/// syntax and the names of its parameters, headers and arguments go to the settings, and their
/// values go to the keychain. A file that could reach beyond one `http`/`https` request is
/// refused, and the outcome names the row and the reason.
#[tauri::command]
pub async fn upload_import_sxcu(app: AppHandle, text: String) -> CommandResult<SxcuOutcome> {
    off_main(move || {
        let state = app.state::<AppState>();
        let outcome = import_sxcu(uploader()?, &state.settings, &text)?;
        if outcome.imported.is_some() {
            let _ = app.emit(crate::events::SETTINGS_CHANGED, state.settings.get());
        }
        Ok(outcome)
    })
    .await
}

/// The import writes the keychain first and then only the custom provider's entry of the
/// settings, under the store's lock (`SettingsStore::update`), so a page that saved another
/// section in the meantime keeps its change.
fn import_sxcu(uploader: &Uploader, store: &SettingsStore, text: &str) -> CommandResult<SxcuOutcome> {
    let custom = glimpsfy_upload::providers::CUSTOM;
    let imported = match uploader.import_sxcu(text, store.get().upload.providers.get(custom)) {
        Ok(imported) => imported,
        Err(UploadError::InvalidConfig { field, reason, .. }) if reason.starts_with("upload.") => {
            return Ok(SxcuOutcome { imported: None, refusal: Some(Issue::error(&field, &reason)) });
        }
        Err(error) => return Err(error.into()),
    };
    let values = serde_json::to_value(&imported.values).map_err(|e| CommandError::internal(e.to_string()))?;
    store.update(|settings| {
        settings.upload.providers.insert(custom.to_string(), values);
    })?;
    Ok(SxcuOutcome { imported: Some(imported), refusal: None })
}

#[cfg(test)]
mod tests {
    //! The command side of an upload: what reaches the pages, the clipboard, the log and the
    //! settings file. A provider without network stands in for the hosts (the crate's own tests
    //! run the real ones against a local server).

    use std::sync::{Arc, Mutex};

    use glimpsfy_upload::http::Connection;
    use glimpsfy_upload::{
        AccountNeed, DeletionKind, Destination, FieldInput, FormShape, Provider, ProviderConfig, SettingField,
        UploadRequest, UploadResult,
    };

    use super::*;
    use crate::settings::secret_fields;

    const SECRET: &str = "token-6f3a1d-do-not-leak";

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Outcome {
        Link,
        Refused,
        Cancelled,
    }

    /// Uploads nowhere and answers as told; it quotes its token in a refusal, as a careless host
    /// would.
    struct Scripted(Outcome);

    impl Provider for Scripted {
        fn id(&self) -> ProviderId {
            ProviderId::new("scripted")
        }

        fn describe(&self) -> ProviderDescription {
            ProviderDescription {
                id: self.id(),
                name: "Scripted".into(),
                monogram: "S".into(),
                shape: FormShape::Key,
                account: AccountNeed::ApiKey,
                domain: Some("scripted.example".into()),
                operator: None,
                website: None,
                key_url: None,
                facts: None,
                max_file_bytes: None,
                links_expire: false,
                supports_delete: false,
                deletion: DeletionKind::None,
                testable: false,
                settings: vec![SettingField {
                    key: "token".into(),
                    label: "upload.field.apiKey".into(),
                    help: None,
                    section: None,
                    required: true,
                    secret: true,
                    rules: Vec::new(),
                    input: FieldInput::Secret { placeholder: None },
                }],
            }
        }

        fn validate(&self, _: &ProviderConfig) -> Vec<Issue> {
            Vec::new()
        }

        fn destination(&self, _: &ProviderConfig) -> glimpsfy_upload::Result<Destination> {
            Ok(Destination { domain: "scripted.example".into(), bucket: None, origins: Vec::new(), fixed: true })
        }

        fn upload(
            &self,
            _: &Connection,
            request: UploadRequest,
            progress: &dyn Fn(u64),
        ) -> glimpsfy_upload::Result<UploadResult> {
            let token = request.config.secret(&self.id(), "token")?.expose().to_string();
            for sent in 1..=200u64 {
                progress(sent);
                if request.cancel.is_cancelled() {
                    return Err(UploadError::Cancelled);
                }
            }
            match self.0 {
                Outcome::Refused => Err(UploadError::Provider {
                    provider: self.id(),
                    status: Some(413),
                    reason: format!("payload too large for token {token}"),
                }),
                _ => Ok(UploadResult {
                    upload_id: request.id,
                    provider: self.id(),
                    url: "https://scripted.example/i/a(1).png".into(),
                    deletion: None,
                    uploaded_at_ms: 0,
                    expires_at_ms: None,
                }),
            }
        }

        fn delete(
            &self,
            _: &Connection,
            _: &DeletionHandle,
            _: &ProviderConfig,
        ) -> glimpsfy_upload::Result<DeleteOutcome> {
            Err(UploadError::NotImplemented("deleting"))
        }

        fn test(&self, _: &Connection, _: &ProviderConfig) -> glimpsfy_upload::Result<ProviderTestResult> {
            Err(UploadError::NotImplemented("testing"))
        }
    }

    #[derive(Default)]
    struct Recorded {
        progress: Vec<UploadProgress>,
        done: Vec<UploadDone>,
        failed: Vec<UploadFailed>,
        copied: Vec<String>,
        opened: Vec<String>,
        log: Vec<String>,
    }

    /// Records everything; presses Cancel on the first progress when `cancel` holds the engine.
    struct Recorder<'a> {
        seen: Mutex<Recorded>,
        cancel: Option<&'a Uploader>,
    }

    impl Sink for Recorder<'_> {
        fn progress(&self, progress: &UploadProgress) {
            if let (Some(uploader), true) = (self.cancel, progress.sent_bytes > 0) {
                uploader.cancel(&progress.upload_id);
            }
            self.seen.lock().unwrap().progress.push(progress.clone());
        }
        fn done(&self, done: &UploadDone) {
            self.seen.lock().unwrap().done.push(done.clone());
        }
        fn failed(&self, failed: &UploadFailed) {
            self.seen.lock().unwrap().failed.push(failed.clone());
        }
        fn copy_text(&self, text: &str) -> Result<(), String> {
            self.seen.lock().unwrap().copied.push(text.to_string());
            Ok(())
        }
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.seen.lock().unwrap().opened.push(url.to_string());
            Ok(())
        }
        fn log(&self, line: &str) {
            self.seen.lock().unwrap().log.push(line.to_string());
        }
    }

    fn uploader_with(outcome: Outcome) -> Uploader {
        let mut providers = glimpsfy_upload::builtin_providers();
        providers.push(Arc::new(Scripted(outcome)));
        let uploader = Uploader::new(providers, SecretStore::in_memory().unwrap(), HttpClient::for_tests());
        uploader.set_secret(&ProviderId::new("scripted"), "token", SECRET).unwrap();
        uploader
    }

    /// A refused or cancelled upload ends with one `failed` event and nothing else: no link on
    /// the clipboard, no browser, no `done` for the history. A finished one copies the link as
    /// the settings say. The token reaches neither an event nor the log. A capture staged only
    /// for Retry (with the history off it is the only copy of the pixels) survives a refusal and
    /// is named in the event with its entry, and goes once the upload succeeded or was cancelled.
    #[test]
    fn only_a_finished_upload_reports_a_link_and_no_report_carries_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        for outcome in [Outcome::Link, Outcome::Refused, Outcome::Cancelled] {
            let staged = dir.path().join(format!("{outcome:?}")).join("Glimpsfy [1].png");
            std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
            std::fs::write(&staged, [1u8; 200]).unwrap();
            let entry = EntryId(format!("entry-{outcome:?}"));
            let source = Source { entry_id: Some(entry.clone()), path: Some(staged.clone()), staged: true };
            let uploader = uploader_with(outcome);
            let recorder = Recorder {
                seen: Mutex::new(Recorded::default()),
                cancel: (outcome == Outcome::Cancelled).then_some(&uploader),
            };
            let provider = ProviderId::new("scripted");
            let confirmed = vec!["scripted".to_string()];
            let pending = uploader
                .begin(
                    &provider,
                    Stored { values: None, confirmed: &confirmed },
                    UploadData::Bytes(vec![1u8; 200].into()),
                    "Glimpsfy [1].png",
                    "image/png",
                )
                .unwrap();
            let id = pending.id.clone();
            run(&uploader, pending, &recorder, After { copy: CopyAfterUpload::Markdown, open: true }, source);

            let seen = recorder.seen.into_inner().unwrap();
            assert_eq!(seen.progress.first().map(|p| p.sent_bytes), Some(0), "{outcome:?}: no first progress");
            match outcome {
                Outcome::Link => {
                    assert!(seen.failed.is_empty());
                    assert_eq!(seen.done.len(), 1);
                    assert_eq!(seen.done[0].copied, Some(CopyAfterUpload::Markdown));
                    assert_eq!(
                        seen.copied,
                        vec!["![Glimpsfy \\[1\\]](<https://scripted.example/i/a(1).png>)".to_string()]
                    );
                    assert_eq!(seen.opened, vec!["https://scripted.example/i/a(1).png".to_string()]);
                    assert_eq!(seen.done[0].entry_id, Some(entry));
                    assert!(!staged.exists(), "the staged copy outlived a finished upload");
                }
                Outcome::Refused | Outcome::Cancelled => {
                    assert!(seen.done.is_empty(), "{outcome:?} reported a link");
                    assert!(seen.copied.is_empty() && seen.opened.is_empty(), "{outcome:?} copied or opened a link");
                    assert_eq!(seen.failed.len(), 1);
                    let failed = &seen.failed[0];
                    assert_eq!(failed.entry_id, Some(entry));
                    if outcome == Outcome::Refused {
                        assert_eq!((failed.kind, failed.status), ("provider", Some(413)));
                        assert!(failed.error.message.contains("payload too large"), "{}", failed.error.message);
                        assert!(staged.exists(), "a refused upload lost the only copy Retry sends");
                        assert_eq!(failed.path.as_deref(), Some(staged.to_string_lossy().as_ref()));
                    } else {
                        assert_eq!(failed.kind, "cancelled");
                        assert_eq!(failed.error.code, ErrorCode::Cancelled);
                        assert!(!staged.exists(), "the staged copy outlived a cancelled upload");
                    }
                }
            }
            assert!(!uploader.cancel(&id), "{outcome:?}: the upload is still registered");
            let reports = format!(
                "{} {} {} {}",
                serde_json::to_string(&seen.progress).unwrap(),
                serde_json::to_string(&seen.done).unwrap(),
                serde_json::to_string(&seen.failed).unwrap(),
                seen.log.join("\n")
            );
            assert!(!reports.contains(SECRET), "{outcome:?}: a report carries the token: {reports}");
        }
    }

    /// Importing a `.sxcu` writes the custom provider's settings to the file and its header and
    /// argument values to the keychain only. Confirming a destination records its key, and a
    /// key the dialog showed for an older address records nothing.
    #[test]
    fn imports_and_confirmations_write_no_secret_to_the_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let uploader = uploader_with(Outcome::Link);
        let store = SettingsStore::open_with(path.clone(), secret_fields(&uploader.describe_all()));
        let sxcu = serde_json::json!({
            "Version": "17.0.0", "DestinationType": "ImageUploader", "RequestMethod": "POST",
            "RequestURL": "https://share.example.dev/api/upload", "Body": "MultipartFormData", "FileFormName": "file",
            "Headers": { "Authorization": format!("Bearer {SECRET}") }, "Arguments": { "token": SECRET },
            "URL": "{json:url}"
        });
        let outcome = import_sxcu(&uploader, &store, &sxcu.to_string()).unwrap();
        assert!(outcome.refusal.is_none() && outcome.imported.is_some(), "{outcome:?}");
        let custom = ProviderId::new(glimpsfy_upload::providers::CUSTOM);
        assert!(uploader.secret_status(&custom, "headers:Authorization").unwrap());
        assert!(uploader.secret_status(&custom, "arguments:token").unwrap());

        let stale = "custom@old.example.dev";
        assert!(confirm(&uploader, &store, &custom, stale).is_err());
        let key = uploader.consent_key(&custom, store.get().upload.providers.get("custom")).unwrap();
        assert_eq!(key, "custom@share.example.dev");
        let settings = confirm(&uploader, &store, &custom, &key).unwrap();
        assert_eq!(settings.upload.confirmed_providers, vec![key]);

        let file = std::fs::read_to_string(&path).unwrap();
        assert!(
            file.contains("share.example.dev") && file.contains("Authorization"),
            "the import was not saved: {file}"
        );
        assert!(!file.contains(SECRET), "the settings file holds a secret: {file}");
    }
}
