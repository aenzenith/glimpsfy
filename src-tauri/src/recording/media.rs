//! Finished recordings as the trim window sees them: the takes this run knows (and history
//! entries opened for trimming), the `glimpsfy-recording://` scheme that serves their file, frames
//! and waveform, and the MP4 and GIF exports that keep running when the window closes.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glimpsfy_capture::{ImageEncoding, PhysicalSize};
use glimpsfy_record::{
    ExportFormat, ExportOptions, ExportPurpose, ExportResult, MediaInfo, RecordingId, RecordingResult, TrackInfo,
    TrackKind, VideoCodec,
};
use serde::{Deserialize, Serialize};
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, UriSchemeContext, UriSchemeResponder};

use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::commands::record::events;
use crate::AppState;

/// Serves recordings, their frames and their waveforms to the pages.
pub const SCHEME: &str = "glimpsfy-recording";
/// The largest part of a file one response carries; the player asks for the rest.
const MAX_CHUNK: u64 = 8 * 1024 * 1024;
/// How often an export reports its progress at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// A recording the trim window can open.
#[derive(Clone, Debug)]
pub struct Known {
    pub result: RecordingResult,
    pub entry_id: Option<String>,
}

/// The takes and history entries this run has opened, by recording id. The scheme serves only
/// these, so a URL can never name an arbitrary file.
#[derive(Default)]
pub struct Library {
    known: Mutex<HashMap<String, Known>>,
    exports: Mutex<HashMap<String, Arc<AtomicBool>>>,
    current: Mutex<Option<RecordingInfo>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioPresence {
    pub microphone: bool,
    pub system: bool,
    pub mixed: bool,
}

/// The trim window's recording (`record://trim`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    pub result: RecordingResult,
    pub url: String,
    pub folder_name: String,
    pub audio: AudioPresence,
    pub entry_id: Option<String>,
    pub start_format: StartFormat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StartFormat {
    #[default]
    Mp4,
    Gif,
}

/// What `record_open_trim` opens.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, rename_all = "camelCase")]
pub enum TrimSource {
    Recording {
        #[serde(rename = "recordingId")]
        recording_id: RecordingId,
    },
    Entry {
        #[serde(rename = "entryId")]
        entry_id: String,
    },
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Library {
    pub fn remember(&self, result: RecordingResult, entry_id: Option<String>) {
        lock(&self.known).insert(result.id.0.clone(), Known { result, entry_id });
    }

    pub fn set_entry(&self, id: &RecordingId, entry_id: String) {
        if let Some(known) = lock(&self.known).get_mut(&id.0) {
            known.entry_id = Some(entry_id);
        }
    }

    pub fn get(&self, id: &str) -> Option<Known> {
        lock(&self.known).get(id).cloned()
    }

    pub fn forget(&self, id: &str) -> Option<Known> {
        lock(&self.known).remove(id)
    }

    pub fn current(&self) -> Option<RecordingInfo> {
        lock(&self.current).clone()
    }
}

fn library(app: &AppHandle) -> &Library {
    &app.state::<super::RecordingApp>().inner().library
}

fn scheme_url(id: &RecordingId) -> String {
    // WebKitGTK's player loads no URI scheme of the app's, so Linux plays from the loopback
    // server (`desktop/loopback.rs`) and keeps the scheme only when that could not start.
    #[cfg(target_os = "linux")]
    if let Some(url) = crate::desktop::loopback::recording_url(&id.0) {
        return url;
    }
    if cfg!(target_os = "windows") {
        format!("http://{SCHEME}.localhost/file/{}", id.0)
    } else {
        format!("{SCHEME}://file/{}", id.0)
    }
}

fn presence(tracks: &[TrackInfo]) -> AudioPresence {
    let has = |kind| tracks.iter().any(|t| t.kind == kind);
    AudioPresence {
        microphone: has(TrackKind::Microphone),
        system: has(TrackKind::SystemAudio),
        mixed: has(TrackKind::MixedAudio),
    }
}

fn info(app: &AppHandle, known: &Known, format: StartFormat) -> RecordingInfo {
    let folder = app.state::<AppState>().settings.get().recording.recordings_folder;
    RecordingInfo {
        url: scheme_url(&known.result.id),
        folder_name: folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        audio: presence(&known.result.tracks),
        entry_id: known.entry_id.clone(),
        start_format: format,
        result: known.result.clone(),
    }
}

/// The recording id a history entry opens under: its own id with a prefix, so it never meets a
/// take's id and passes [`RecordingId::is_well_formed`].
fn entry_recording_id(entry_id: &str) -> RecordingId {
    let clean: String = entry_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(60).collect();
    RecordingId(format!("h-{clean}"))
}

/// A history entry of kind recording, described from its file (or from the entry when the file
/// cannot be read yet).
fn from_entry(app: &AppHandle, entry_id: &str) -> CommandResult<Known> {
    use glimpsfy_history::{EntryId, EntryKind};
    let store = crate::commands::history::store(app)?;
    let entry = store.get(&EntryId(entry_id.to_string())).map_err(|e| CommandError::not_found(e.to_string()))?;
    if entry.kind != EntryKind::Recording {
        return Err(CommandError::invalid("the history entry is not a recording, so it cannot be trimmed"));
    }
    let path = entry
        .media_path()
        .filter(|p| p.is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| CommandError::not_found("the recording's file is missing, so it cannot be trimmed"))?;
    let id = entry_recording_id(entry_id);
    let read = glimpsfy_record::platform::inspect(&path).ok();
    let result = describe(id, path, read.as_ref(), &entry);
    Ok(Known { result, entry_id: Some(entry_id.to_string()) })
}

fn describe(
    id: RecordingId,
    path: PathBuf,
    info: Option<&MediaInfo>,
    entry: &glimpsfy_history::Entry,
) -> RecordingResult {
    let size = info
        .map(|i| i.size)
        .or_else(|| entry.size.map(|s| PhysicalSize { width: s.width, height: s.height }))
        .unwrap_or_default();
    let codec = match info.map(|i| i.video_codec.as_str()) {
        Some("hvc1" | "hev1") => VideoCodec::Hevc,
        _ => VideoCodec::H264,
    };
    let mut tracks =
        vec![TrackInfo { kind: TrackKind::Video, codec: info.map(|i| i.video_codec.clone()).unwrap_or_default() }];
    if let Some(info) = info {
        tracks.extend(info.audio.iter().map(|&kind| TrackInfo { kind, codec: "aac".into() }));
    }
    RecordingResult {
        id,
        duration_ms: info.map(|i| i.duration_ms).or(entry.duration_ms).unwrap_or(0),
        size,
        file_bytes: std::fs::metadata(&path).map(|m| m.len()).or(entry.file_bytes.ok_or(())).unwrap_or(0),
        tracks,
        codec,
        fps: info.map(|i| i.fps.round() as u32).unwrap_or(0),
        created_at_ms: entry.created_at_ms,
        stopped_because: None,
        path,
    }
}

/// Opens the trim window on a finished take or a history entry and tells it which.
pub fn open_trim(app: &AppHandle, source: TrimSource, format: StartFormat) -> CommandResult<RecordingInfo> {
    let lib = library(app);
    let known = match source {
        TrimSource::Recording { recording_id } => {
            lib.get(&recording_id.0).ok_or_else(|| CommandError::not_found("the recording is not known any more"))?
        }
        TrimSource::Entry { entry_id } => {
            let known = from_entry(app, &entry_id)?;
            lib.remember(known.result.clone(), known.entry_id.clone());
            known
        }
    };
    let info = info(app, &known, format);
    *lock(&lib.current) = Some(info.clone());
    crate::windows::open(app, crate::commands::windows::WindowKind::Recorder);
    let _ = app.emit_to(tauri::EventTarget::webview_window("recorder"), events::TRIM, info.clone());
    Ok(info)
}

/// The recording the trim window shows, for its first read.
pub fn current(app: &AppHandle) -> Option<RecordingInfo> {
    library(app).current()
}

/// The file of a take or a history entry this run knows, which the Linux loopback server
/// (`desktop/loopback.rs`) serves as the scheme would.
#[cfg(target_os = "linux")]
pub fn known_file(app: &AppHandle, id: &str) -> Option<PathBuf> {
    if !RecordingId::is_well_formed(id) {
        return None;
    }
    library(app).get(id).map(|known| known.result.path)
}

pub fn recording_info(app: &AppHandle, id: &RecordingId) -> CommandResult<RecordingInfo> {
    let known =
        library(app).get(&id.0).ok_or_else(|| CommandError::not_found("the recording is not known any more"))?;
    let format = library(app).current().filter(|c| c.result.id == *id).map(|c| c.start_format).unwrap_or_default();
    Ok(info(app, &known, format))
}

/// Moves a finished take to the Trash (the trim window's Discard, after its own question).
pub fn trash(app: &AppHandle, id: &RecordingId) -> CommandResult<()> {
    use glimpsfy_record::files::Trash;
    let known =
        library(app).get(&id.0).ok_or_else(|| CommandError::not_found("the recording is not known any more"))?;
    // The history entry goes with the file: the history moves its saved file to the Trash and
    // removes the row, so the window does not keep an entry whose file is gone.
    if let Some(entry) = known.entry_id.clone() {
        let options = glimpsfy_history::DeleteOptions { trash_saved_file: true, delete_hosted_copy: false };
        let ids = vec![glimpsfy_history::EntryId(entry)];
        let deleted =
            tauri::async_runtime::block_on(crate::commands::history::history_delete(app.clone(), ids, options));
        if let Err(error) = deleted {
            // The file still goes to the Trash below; the entry stays, and the history says why
            // when the user opens it.
            eprintln!("[glimpsfy] the history entry of a discarded recording stayed: {}", error.message);
        }
    }
    // Without an entry (the history is off), or when the entry named another file, the file is
    // moved here; it is never unlinked.
    if known.result.path.exists() {
        glimpsfy_record::files::SystemTrash.trash(&known.result.path)?;
    }
    super::transcript::forget(app, &known);
    library(app).forget(&id.0);
    Ok(())
}

// ------------------------------------------------------------------------------- export

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgress {
    pub recording_id: RecordingId,
    pub format: StartFormat,
    pub fraction: f64,
    pub remaining_ms: Option<u64>,
}

/// Writes an MP4 or a GIF of a known recording. It runs on its own thread, so it goes on when the
/// trim window closes; the toast reports a saved file and a failure.
pub async fn export(app: AppHandle, id: RecordingId, options: ExportOptions) -> CommandResult<ExportResult> {
    let known =
        library(&app).get(&id.0).ok_or_else(|| CommandError::not_found("the recording is not known any more"))?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut exports = lock(&library(&app).exports);
        if exports.contains_key(&id.0) {
            return Err(CommandError::new(ErrorCode::Busy, "this recording is already being exported"));
        }
        exports.insert(id.0.clone(), cancel.clone());
    }
    let (tx, rx) = tauri::async_runtime::channel(1);
    let handle = app.clone();
    std::thread::spawn(move || {
        let outcome = run_export(&handle, &known, &options, &cancel);
        lock(&library(&handle).exports).remove(&known.result.id.0);
        report(&handle, &known, &options, &outcome);
        let _ = tx.blocking_send(outcome);
    });
    let mut rx = rx;
    rx.recv().await.unwrap_or_else(|| Err(CommandError::internal("the export stopped without an answer")))
}

pub fn cancel_export(app: &AppHandle, id: &RecordingId) {
    if let Some(flag) = lock(&library(app).exports).get(&id.0) {
        flag.store(true, Ordering::SeqCst);
    }
}

fn run_export(
    app: &AppHandle,
    known: &Known,
    options: &ExportOptions,
    cancel: &AtomicBool,
) -> CommandResult<ExportResult> {
    let settings = app.state::<AppState>().settings.get();
    let folder = match options.purpose {
        ExportPurpose::Save => {
            options.destination.clone().unwrap_or_else(|| settings.recording.recordings_folder.clone())
        }
        ExportPurpose::Copy | ExportPurpose::Upload => {
            crate::paths::cache_dir(app).map_err(|e| CommandError::new(ErrorCode::Io, e.to_string()))?.join("exports")
        }
    };
    // A file with cuts is a trimmed copy too, so it never takes the recording's own name.
    let trimmed = options.trim.is_some() || !options.cuts.is_empty();
    let wanted = glimpsfy_record::export::wanted_path(&folder, &known.result.path, &options.format, trimmed);
    let format = match options.format {
        ExportFormat::Mp4 { .. } => StartFormat::Mp4,
        ExportFormat::Gif { .. } => StartFormat::Gif,
    };
    let id = known.result.id.clone();
    let mut last = Instant::now() - PROGRESS_EVERY;
    let mut progress = |p: glimpsfy_record::export::Progress| {
        if p.fraction < 1.0 && last.elapsed() < PROGRESS_EVERY {
            return;
        }
        last = Instant::now();
        let payload =
            ExportProgress { recording_id: id.clone(), format, fraction: p.fraction, remaining_ms: p.remaining_ms };
        let _ = app.emit(events::EXPORT_PROGRESS, payload);
    };
    let result =
        glimpsfy_record::export::export(&known.result.id, &known.result.path, options, &wanted, cancel, &mut progress)?;
    if options.purpose == ExportPurpose::Copy {
        copy_file(&result.path)?;
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
fn copy_file(path: &Path) -> CommandResult<()> {
    crate::macos::recording::copy_file(path).map_err(|e| CommandError::new(ErrorCode::Io, e))
}

#[cfg(not(target_os = "macos"))]
fn copy_file(path: &Path) -> CommandResult<()> {
    crate::desktop::recording::copy_file(path).map_err(|e| CommandError::new(ErrorCode::Io, e))
}

/// The toast after an export: a saved file with Show in Finder, or why it failed. A cancelled
/// export and a copy or an upload (whose window says the rest) say nothing here.
fn report(app: &AppHandle, known: &Known, options: &ExportOptions, outcome: &CommandResult<ExportResult>) {
    use crate::windows::{toast, toast_failure, Toast, ToastKind};
    match outcome {
        Ok(done) if options.purpose == ExportPurpose::Save => {
            let mut saved = Toast::new(ToastKind::Saved);
            saved.path = Some(done.path.display().to_string());
            saved.folder = done.path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
            saved.width = Some(done.size.width);
            saved.height = Some(done.size.height);
            saved.bytes = Some(done.file_bytes);
            saved.format = Some(match options.format {
                ExportFormat::Mp4 { .. } => "mp4",
                ExportFormat::Gif { .. } => "gif",
            });
            saved.thumbnail = super::thumbnail(&known.result.path);
            toast(app, None, saved);
        }
        Ok(_) => {}
        Err(error) if error.code == ErrorCode::Cancelled => {}
        Err(error) => toast_failure(app, None, error),
    }
}

// ------------------------------------------------------------------------------- the scheme

enum Ask {
    File,
    Frame { at_ms: u64, height: u32 },
    Wave { bins: usize },
}

/// What a URL asks for: `glimpsfy-recording://file/<id>`, `…/frame/<id>?t=&h=` or
/// `…/wave/<id>?n=`, in either form of the scheme (`http://glimpsfy-recording.localhost/…` on
/// Windows).
fn parse(uri: &tauri::http::Uri) -> Option<(Ask, String)> {
    let path = uri.path().trim_matches('/');
    let (kind, id) = match uri.host() {
        Some(host @ ("file" | "frame" | "wave")) => (host, path),
        _ => path.split_once('/')?,
    };
    let id = id.to_string();
    if !RecordingId::is_well_formed(&id) {
        return None;
    }
    let query: HashMap<&str, &str> =
        uri.query().unwrap_or("").split('&').filter_map(|pair| pair.split_once('=')).collect();
    let number = |key: &str| query.get(key).and_then(|v| v.parse::<u64>().ok());
    let ask = match kind {
        "file" => Ask::File,
        "frame" => {
            Ask::Frame { at_ms: number("t").unwrap_or(0), height: number("h").unwrap_or(90).clamp(16, 1080) as u32 }
        }
        "wave" => Ask::Wave { bins: number("n").unwrap_or(400).clamp(1, 4000) as usize },
        _ => return None,
    };
    Some((ask, id))
}

/// The scheme's handler (`register_asynchronous_uri_scheme_protocol`): every answer is read on a
/// thread of its own, so decoding a frame never holds up a page.
pub fn serve(context: UriSchemeContext<'_, tauri::Wry>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = context.app_handle().clone();
    std::thread::spawn(move || {
        let mut response = respond(&app, &request);
        // The pages come from another origin (the dev server, or `tauri://`), and `fetch` (the
        // waveform) refuses an answer that does not allow it; images and video do not ask.
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, header::HeaderValue::from_static("*"));
        responder.respond(response)
    });
}

fn empty(status: StatusCode) -> Response<Cow<'static, [u8]>> {
    Response::builder().status(status).body(Cow::Borrowed(&[][..])).unwrap_or_default()
}

fn respond(app: &AppHandle, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let Some((ask, id)) = parse(request.uri()) else {
        return empty(StatusCode::BAD_REQUEST);
    };
    let Some(known) = library(app).get(&id) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let path = known.result.path;
    match ask {
        Ask::File => file(request, &path),
        Ask::Frame { at_ms, height } => match glimpsfy_record::platform::frame_at(&path, at_ms, height) {
            Ok(frame) => match frame.encode(ImageEncoding::Jpeg { quality: 82, background: [0, 0, 0] }) {
                Ok(jpeg) => Response::builder()
                    .header(header::CONTENT_TYPE, "image/jpeg")
                    .header(header::CACHE_CONTROL, "max-age=31536000, immutable")
                    .body(Cow::Owned(jpeg))
                    .unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR)),
                Err(_) => empty(StatusCode::INTERNAL_SERVER_ERROR),
            },
            Err(_) => empty(StatusCode::INTERNAL_SERVER_ERROR),
        },
        Ask::Wave { bins } => match glimpsfy_record::platform::waveform(&path, bins) {
            Ok(peaks) => {
                let body = serde_json::to_vec(&serde_json::json!({ "peaks": peaks })).unwrap_or_default();
                Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CACHE_CONTROL, "max-age=31536000, immutable")
                    .body(Cow::Owned(body))
                    .unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
            }
            Err(_) => empty(StatusCode::INTERNAL_SERVER_ERROR),
        },
    }
}

/// The byte range of a `Range: bytes=a-b` header, clamped to the file and to [`MAX_CHUNK`].
fn range(header: &str, length: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start, end) = spec.split_once('-')?;
    let last = length.checked_sub(1)?;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", suffix) => (length.saturating_sub(suffix.parse().ok()?), last),
        (start, "") => (start.parse().ok()?, last),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(last)),
    };
    (start <= end && start < length).then(|| (start, end.min(start + MAX_CHUNK - 1)))
}

/// The recording's file, by ranges: a `<video>` asks for the parts it plays, and a recording
/// asked for whole is answered with its first part, so a long one is never read at once.
fn file(request: &Request<Vec<u8>>, path: &Path) -> Response<Cow<'static, [u8]>> {
    let Ok(mut file) = File::open(path) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    let kind = match path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("mov") => "video/quicktime",
        Some("gif") => "image/gif",
        _ => "video/mp4",
    };
    let asked = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let asked = asked.or((length > MAX_CHUNK).then_some("bytes=0-"));
    let (status, start, end) = match asked {
        Some(value) => match range(value, length) {
            Some((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end),
            None => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{length}"))
                    .body(Cow::Borrowed(&[][..]))
                    .unwrap_or_default()
            }
        },
        None => (StatusCode::OK, 0, length.saturating_sub(1)),
    };
    let size = if length == 0 { 0 } else { end - start + 1 };
    let mut body = vec![0u8; size as usize];
    if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_exact(&mut body)).is_err() {
        return empty(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, kind)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-cache");
    let builder = if status == StatusCode::PARTIAL_CONTENT {
        builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{length}"))
    } else {
        builder
    };
    builder.status(status).body(Cow::Owned(body)).unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
}
