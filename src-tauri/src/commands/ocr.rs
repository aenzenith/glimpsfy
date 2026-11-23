//! Text recognition commands: "Copy text", the selectable text layer, QR codes and the PP-OCR
//! models. Owner: OCR (milestone 3).
//!
//! # Where the pixels come from
//!
//! A command reads one of three sources, and never captures again:
//!
//! - a selection of an open capture (`OcrSource::Selection`): the crop of its frozen frame at the
//!   physical resolution, the way `output_copy` takes it;
//! - an image file (`OcrSource::File`), which the system decodes (PNG, JPEG, HEIC, WebP…);
//! - raw RGBA pixels from the page (`ocr_recognize_pixels`, `ocr_detect_codes_pixels`), sent as
//!   the call's raw body with the `x-glimpsfy-ocr` header, never as base64. The text layer uses
//!   these, because the page flattens blurs and pixelations into the pixels first, so a redacted
//!   area is never read.
//!
//! Every box in a result is in pixels of the image that was read (`OcrResult::image_size`).
//!
//! # Threads and cancellation
//!
//! Engines block for up to seconds, so every recognition runs on a blocking worker thread. A
//! call may name a job (`job`); `ocr_cancel` stops it, and closing a capture stops every job that
//! reads from it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use glimpsfy_capture::{
    selection_to_frame, ColorInfo, ColorSpace, DisplayId, Frame, PhysicalSize, PixelFormat, ScaleFactor,
};
use glimpsfy_ocr::models::{self, ModelInfo, ModelStore, Progress};
use glimpsfy_ocr::{
    CancelToken, CodeResult, EngineId, Entity, EntityKind, LanguageInfo, LanguageTag, OcrEngine, OcrError, OcrRequest,
    OcrResult,
};
use serde::{Deserialize, Serialize};
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Emitter, Listener, Manager};

use crate::commands::capture::{CaptureId, Selection};
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::settings::OcrEngineChoice;
use crate::windows::{Toast, ToastKind};
use crate::{events, windows, AppState};

/// Events this area owns. The TypeScript copy is in `src/lib/api/ocr.ts`.
pub mod ocr_events {
    /// A model download made progress. Payload: `glimpsfy_ocr::models::Progress`.
    pub const MODEL_PROGRESS: &str = "ocr://model-progress";
}

/// The header that describes the raw pixels of `ocr_recognize_pixels` and
/// `ocr_detect_codes_pixels`: JSON (`PixelsHeader`), percent-encoded, because a header takes
/// ASCII only.
pub const PIXELS_HEADER: &str = "x-glimpsfy-ocr";

impl From<OcrError> for CommandError {
    fn from(error: OcrError) -> Self {
        // A capture failure keeps the capture area's own code.
        let error = match error {
            OcrError::Capture(capture) => return capture.into(),
            other => other,
        };
        let code = match &error {
            OcrError::NotImplemented(_) => ErrorCode::NotImplemented,
            OcrError::EngineUnavailable(_) | OcrError::LanguageUnavailable(_) | OcrError::ModelMissing(_) => {
                ErrorCode::NotFound
            }
            OcrError::InvalidImage(_) => ErrorCode::InvalidArgument,
            OcrError::Capture(_) => ErrorCode::Capture,
            OcrError::Engine(_) => ErrorCode::Internal,
            OcrError::Cancelled => ErrorCode::Cancelled,
            OcrError::Download(_) => ErrorCode::Provider,
            OcrError::ChecksumMismatch { .. } | OcrError::Io(_) => ErrorCode::Io,
        };
        CommandError::new(code, error.to_string())
    }
}

/// Where the pixels to read come from.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OcrSource {
    /// A selection of an open capture, read from its frozen frame.
    Selection { selection: Selection },
    /// An image file the system can decode.
    File { path: PathBuf },
}

/// What the page sends with raw pixels.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelsHeader {
    /// The image's size in pixels; the body holds `width × height × 4` bytes of RGBA.
    pub width: u32,
    pub height: u32,
    pub color_space: ColorSpace,
    /// The capture the pixels belong to, so that closing it cancels the recognition.
    #[serde(default)]
    pub capture_id: Option<CaptureId>,
    /// A name for the job, for `ocr_cancel`.
    #[serde(default)]
    pub job: Option<String>,
}

/// What "Copy text" put on the clipboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyTextResult {
    /// The text as it was copied. An empty text means nothing was recognised, and the
    /// clipboard was left as it was.
    pub text: String,
    pub line_count: u32,
    pub language: Option<LanguageTag>,
}

// -------------------------------------------------------------------------------- commands

/// Reads the text of a selection or an image file with the engine and languages of Settings ›
/// Text recognition.
/// A file's result is kept while the file and the settings stay the same, so the history can
/// ask for the same capture again without reading it again.
#[tauri::command]
pub async fn ocr_recognize(app: AppHandle, source: OcrSource, job: Option<String>) -> CommandResult<OcrResult> {
    off_main(move || {
        let cached = match &source {
            OcrSource::File { path } => FileKey::of(&app, path),
            OcrSource::Selection { .. } => None,
        };
        if let Some(found) = cached.as_ref().and_then(FileKey::lookup) {
            return Ok(found);
        }
        let (frame, capture) = load(&app, &source)?;
        let result = recognize(&app, frame, job, capture)?;
        if let Some(key) = cached {
            key.store(&result);
        }
        Ok(result)
    })
    .await
}

/// Reads the text of pixels the page sends: the text layer's area with every redaction already
/// flattened in. Body: RGBA bytes; header: `x-glimpsfy-ocr`.
#[tauri::command]
pub async fn ocr_recognize_pixels(app: AppHandle, request: Request<'_>) -> CommandResult<OcrResult> {
    let (header, frame) = read_pixels(&request)?;
    off_main(move || recognize(&app, Arc::new(frame), header.job, header.capture_id)).await
}

/// Reads the text of a selection or an image file and puts it on the clipboard in one step,
/// without an image, then shows the "copied N lines" toast. The capture stays open; the overlay
/// closes it when it is done (the `copyText` capture mode calls this, then `capture_close`).
/// When nothing is recognised, the answer's text is empty, the clipboard keeps what it had and
/// no toast appears, so the overlay can say so and keep the selection.
#[tauri::command]
pub async fn ocr_copy_text(app: AppHandle, source: OcrSource, job: Option<String>) -> CommandResult<CopyTextResult> {
    off_main(move || {
        let display = match &source {
            OcrSource::Selection { selection } => Some(selection.display_id),
            OcrSource::File { .. } => None,
        };
        let (frame, capture) = load(&app, &source)?;
        let result = recognize(&app, frame, job, capture)?;
        copy(&app, result.text, result.language, display)
    })
    .await
}

/// Puts text the user chose in the text layer on the clipboard (a selection with ⌘C, or Copy
/// all text) and shows the "copied N lines" toast.
#[tauri::command]
pub async fn ocr_copy(
    app: AppHandle,
    text: String,
    language: Option<LanguageTag>,
    display_id: Option<DisplayId>,
) -> CommandResult<CopyTextResult> {
    off_main(move || copy(&app, text, language, display_id)).await
}

/// Finds QR codes and barcodes in a selection or an image file, for the chip under each code.
/// Answers an empty list when Settings › Text recognition › Detect QR codes is off.
#[tauri::command]
pub async fn ocr_detect_codes(
    app: AppHandle,
    source: OcrSource,
    job: Option<String>,
) -> CommandResult<Vec<CodeResult>> {
    off_main(move || {
        let (frame, capture) = load(&app, &source)?;
        detect(&app, frame, job, capture)
    })
    .await
}

/// Finds QR codes and barcodes in pixels the page sends. Body: RGBA bytes; header:
/// `x-glimpsfy-ocr`.
#[tauri::command]
pub async fn ocr_detect_codes_pixels(app: AppHandle, request: Request<'_>) -> CommandResult<Vec<CodeResult>> {
    let (header, frame) = read_pixels(&request)?;
    off_main(move || detect(&app, Arc::new(frame), header.job, header.capture_id)).await
}

/// Finds the phone numbers, email addresses, links and dates of `kinds` in a recognised text,
/// for the underlined pieces of the text layer (`glimpsfy_ocr::entities`). It reads the text
/// only; no request leaves the machine. A date without a year is read against the local clock.
#[tauri::command]
pub async fn ocr_detect_entities(result: OcrResult, kinds: Vec<EntityKind>) -> CommandResult<Vec<Entity>> {
    off_main(move || Ok(glimpsfy_ocr::find_entities(&result, &kinds, chrono::Local::now().naive_local()))).await
}

/// Stops a recognition, a code search or a model download that was started with this job name.
/// Stopping a job that already ended does nothing.
#[tauri::command]
pub fn ocr_cancel(job: String) {
    cancel_job(&job);
}

/// Stops the job named `job`, whichever command started it (`subject_cancel` stops a subject
/// search this way).
pub(super) fn cancel_job(job: &str) {
    if let Some(found) = jobs().lock().unwrap_or_else(|p| p.into_inner()).get(job) {
        found.cancel.cancel();
    }
}

/// The languages the chosen engine reads on this machine, read from the engine at run time.
#[tauri::command]
pub async fn ocr_languages(app: AppHandle) -> CommandResult<Vec<LanguageInfo>> {
    off_main(move || Ok(engine(&app)?.supported_languages()?)).await
}

/// The engines of this platform and build (`available`) and those that read text now (`ready`);
/// Settings › Text recognition offers only a ready engine.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrEngines {
    pub available: Vec<EngineId>,
    pub ready: Vec<EngineId>,
}

#[tauri::command]
pub fn ocr_engines() -> OcrEngines {
    OcrEngines { available: glimpsfy_ocr::available_engines(), ready: glimpsfy_ocr::ready_engines() }
}

/// The PP-OCR models of the reviewed table and whether each is downloaded.
#[tauri::command]
pub async fn ocr_models(app: AppHandle) -> CommandResult<Vec<ModelInfo>> {
    off_main(move || Ok(model_store(&app)?.list())).await
}

/// Downloads a PP-OCR model from the reviewed table, reporting `ocr://model-progress`. Only a
/// user's click calls this; nothing in the app downloads a model on its own. Fails with
/// `not_implemented` in a build without the PP-OCR engine, so no one downloads a model the app
/// cannot use.
#[tauri::command]
pub async fn ocr_model_download(app: AppHandle, id: String, job: Option<String>) -> CommandResult<ModelInfo> {
    if !glimpsfy_ocr::available_engines().contains(&EngineId::PpOcr) {
        return Err(CommandError::not_implemented("the PP-OCR engine in this build"));
    }
    off_main(move || {
        let store = model_store(&app)?;
        let guard = JobGuard::start(&app, job, None);
        let emitter = app.clone();
        let mut progress = |p: Progress| {
            let _ = emitter.emit(ocr_events::MODEL_PROGRESS, p);
        };
        // The offline switch (card A8) is not in the app yet, so the download is always allowed.
        store.download(&id, &mut progress, &guard.cancel, &|| true)?;
        store.list().into_iter().find(|m| m.id == id).ok_or_else(|| CommandError::not_found(format!("model {id}")))
    })
    .await
}

/// Deletes a downloaded PP-OCR model.
#[tauri::command]
pub async fn ocr_model_remove(app: AppHandle, id: String) -> CommandResult<()> {
    off_main(move || Ok(model_store(&app)?.remove(&id)?)).await
}

// ------------------------------------------------------------------------------- the work

async fn off_main<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| CommandError::internal(e.to_string()))?
}

/// The folder of the PP-OCR models, `<app data>/models`, which the engine reads too.
fn model_store(app: &AppHandle) -> CommandResult<ModelStore> {
    let root = crate::paths::data_dir(app).map_err(|e| CommandError::internal(e.to_string()))?.join("models");
    models::set_shared_root(root.clone());
    Ok(ModelStore::new(root))
}

/// The engine Settings › Text recognition chose.
fn engine(app: &AppHandle) -> CommandResult<Arc<dyn OcrEngine>> {
    let choice = app.state::<AppState>().settings.get().ocr.engine;
    Ok(match choice {
        OcrEngineChoice::System => glimpsfy_ocr::default_engine(),
        OcrEngineChoice::PpOcr => {
            model_store(app)?;
            glimpsfy_ocr::engine(EngineId::PpOcr)?
        }
    })
}

/// The pixels of a source, and the capture they belong to. A selection that is still a picked
/// window reads the window's own image, as Copy delivers it, so text that another window covered
/// in the frozen frame is read too; any other selection reads its crop of the frozen frame.
fn load(app: &AppHandle, source: &OcrSource) -> CommandResult<(Arc<Frame>, Option<CaptureId>)> {
    match source {
        OcrSource::Selection { selection } => {
            let state = app.state::<AppState>();
            let id = selection.capture_id;
            let session =
                state.sessions.get(id).ok_or_else(|| CommandError::not_found(format!("capture #{}", id.0)))?;
            if let Some(picked) = session.picked_for(selection) {
                return Ok((picked.frame.clone(), Some(id)));
            }
            let entry = session
                .display(selection.display_id)
                .ok_or_else(|| CommandError::not_found(format!("display {}", selection.display_id.0)))?;
            let rect = selection_to_frame(selection.rect, entry.display.frame.size(), entry.frame.size)
                .ok_or_else(|| CommandError::invalid("the selection is empty or outside its display"))?;
            Ok((Arc::new(entry.frame.crop(rect)?), Some(id)))
        }
        OcrSource::File { path } => Ok((Arc::new(glimpsfy_ocr::load_image(path)?), None)),
    }
}

/// Names a file's recognition: the file as it is on disk and the settings it was read with.
#[derive(Clone, Debug, PartialEq)]
struct FileKey {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    len: u64,
    settings: String,
}

/// The most recent file results kept; the history indexes one capture at a time.
const FILE_CACHE: usize = 16;

fn file_cache() -> &'static Mutex<std::collections::VecDeque<(FileKey, OcrResult)>> {
    static CACHE: OnceLock<Mutex<std::collections::VecDeque<(FileKey, OcrResult)>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

impl FileKey {
    fn of(app: &AppHandle, path: &std::path::Path) -> Option<FileKey> {
        let meta = std::fs::metadata(path).ok()?;
        let ocr = app.state::<AppState>().settings.get().ocr;
        let settings = format!("{:?}|{:?}|{}|{}", ocr.engine, ocr.languages, ocr.detect_language, ocr.keep_line_breaks);
        Some(FileKey { path: path.to_path_buf(), modified: meta.modified().ok(), len: meta.len(), settings })
    }

    fn lookup(&self) -> Option<OcrResult> {
        let cache = file_cache().lock().unwrap_or_else(|p| p.into_inner());
        cache.iter().find(|(key, _)| key == self).map(|(_, result)| result.clone())
    }

    fn store(self, result: &OcrResult) {
        let mut cache = file_cache().lock().unwrap_or_else(|p| p.into_inner());
        cache.retain(|(key, _)| key.path != self.path);
        cache.push_back((self, result.clone()));
        while cache.len() > FILE_CACHE {
            cache.pop_front();
        }
    }
}

/// `%XX` escapes back to bytes, as `encodeURIComponent` wrote them.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The header and the pixels of a raw call, checked against each other.
fn read_pixels(request: &Request<'_>) -> CommandResult<(PixelsHeader, Frame)> {
    let raw = request
        .headers()
        .get(PIXELS_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| CommandError::invalid(format!("the call has no {PIXELS_HEADER} header")))?;
    let json =
        percent_decode(raw).ok_or_else(|| CommandError::invalid("the pixels header is not percent-encoded UTF-8"))?;
    let header: PixelsHeader = serde_json::from_str(&json)
        .map_err(|e| CommandError::invalid(format!("the pixels header does not parse: {e}")))?;
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(CommandError::invalid("the pixels must be sent as raw bytes"));
    };
    let expected = header.width as u64 * header.height as u64 * 4;
    if header.width == 0 || header.height == 0 || bytes.len() as u64 != expected {
        return Err(CommandError::invalid(format!(
            "{} bytes do not hold {} × {} RGBA pixels",
            bytes.len(),
            header.width,
            header.height
        )));
    }
    let frame = Frame::new(
        PhysicalSize::new(header.width, header.height),
        header.width as usize * 4,
        PixelFormat::Rgba8,
        ColorInfo::for_space(header.color_space),
        ScaleFactor::ONE,
        // The body belongs to the call, and the engine reads on another thread.
        bytes.clone(),
    )?;
    Ok((header, frame))
}

pub(crate) fn recognize(
    app: &AppHandle,
    frame: Arc<Frame>,
    job: Option<String>,
    capture: Option<CaptureId>,
) -> CommandResult<OcrResult> {
    let settings = app.state::<AppState>().settings.get().ocr;
    let engine = engine(app)?;
    let guard = JobGuard::start(app, job, capture);
    let mut request = OcrRequest::new(frame);
    request.languages = settings.languages.iter().map(|l| LanguageTag::new(l.clone())).collect();
    request.detect_language = settings.detect_language;
    request.keep_line_breaks = settings.keep_line_breaks;
    request.cancel = guard.cancel.clone();
    Ok(glimpsfy_ocr::recognize(engine.as_ref(), &request)?)
}

fn detect(
    app: &AppHandle,
    frame: Arc<Frame>,
    job: Option<String>,
    capture: Option<CaptureId>,
) -> CommandResult<Vec<CodeResult>> {
    if !app.state::<AppState>().settings.get().ocr.detect_codes {
        return Ok(Vec::new());
    }
    let engine = engine(app)?;
    let guard = JobGuard::start(app, job, capture);
    Ok(glimpsfy_ocr::detect_codes(engine.as_ref(), &frame, &guard.cancel)?)
}

/// Puts the text on the clipboard and shows the toast that quotes it. An empty text leaves the
/// clipboard alone and shows no toast; the caller says that nothing was found.
fn copy(
    app: &AppHandle,
    text: String,
    language: Option<LanguageTag>,
    display: Option<DisplayId>,
) -> CommandResult<CopyTextResult> {
    let line_count = text.lines().filter(|l| !l.trim().is_empty()).count() as u32;
    if text.trim().is_empty() {
        return Ok(CopyTextResult { text: String::new(), line_count: 0, language });
    }
    crate::output::copy_text(&text)?;
    let mut toast = Toast::new(ToastKind::TextCopied);
    toast.text = Some(text.clone());
    windows::toast(app, display, toast);
    Ok(CopyTextResult { text, line_count, language })
}

// ------------------------------------------------------------------------------- the jobs

struct Job {
    capture: Option<CaptureId>,
    cancel: CancelToken,
}

fn jobs() -> &'static Mutex<HashMap<String, Job>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}

/// A running job, registered while it lives so that `ocr_cancel` and a closing capture can stop
/// it. The subject commands (`commands/subject.rs`) register theirs here too.
pub(super) struct JobGuard {
    key: String,
    pub(super) cancel: CancelToken,
}

impl JobGuard {
    pub(super) fn start(app: &AppHandle, job: Option<String>, capture: Option<CaptureId>) -> JobGuard {
        watch_captures(app);
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let key = job.unwrap_or_else(|| format!("#{}", NEXT.fetch_add(1, Ordering::SeqCst)));
        let cancel = CancelToken::new();
        // The capture may have closed before the job started; a closed capture's job ends at once.
        if capture.is_some_and(|id| app.state::<AppState>().sessions.get(id).is_none()) {
            cancel.cancel();
        }
        let mut map = jobs().lock().unwrap_or_else(|p| p.into_inner());
        // A job started again under the same name replaces the old one, which stops.
        if let Some(old) = map.insert(key.clone(), Job { capture, cancel: cancel.clone() }) {
            old.cancel.cancel();
        }
        JobGuard { key, cancel }
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        let mut map = jobs().lock().unwrap_or_else(|p| p.into_inner());
        // Only this job's own entry goes; a newer job under the same name stays.
        if map.get(&self.key).is_some_and(|job| job.cancel.same(&self.cancel)) {
            map.remove(&self.key);
        }
    }
}

/// Cancels the jobs of a capture when it closes. The listener is registered on first use.
fn watch_captures(app: &AppHandle) {
    static WATCHING: OnceLock<()> = OnceLock::new();
    WATCHING.get_or_init(|| {
        app.listen_any(events::CAPTURE_CLOSED, |event| {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Closed {
                capture_id: CaptureId,
            }
            let Ok(closed) = serde_json::from_str::<Closed>(event.payload()) else { return };
            let map = jobs().lock().unwrap_or_else(|p| p.into_inner());
            for job in map.values().filter(|job| job.capture == Some(closed.capture_id)) {
                job.cancel.cancel();
            }
        });
    });
}
