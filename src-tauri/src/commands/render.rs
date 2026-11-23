//! Rendered output and the editor's image. Owner: annotation (milestone 2).
//!
//! The page is the only renderer of an annotated capture (`src/annotate/compose.ts`), so the file
//! a user gets is exactly what the canvas showed. When a capture has drawings, the page renders
//! the exported area at the frame's own resolution and colour space and sends the RGBA bytes as
//! the raw body of the call, never as base64; the `x-glimpsfy-render` header carries the rest
//! (`RenderHeader`: JSON, percent-encoded, because a header takes ASCII only). The core then
//! encodes and delivers the pixels with `output.rs`. A capture without drawings still takes the
//! fast path of `output_copy` and `output_save`, which crop in Rust.
//!
//! The editor window shows one image at a time: a file (`editor_open`, from the history or a
//! file dropped on the window), a capture handed over by the overlay with its drawings
//! (`editor_open_capture`), or the history's screenshots laid onto one new image
//! (`editor_combine`). The core keeps the image and gives the page its bytes as the raw binary
//! response of `editor_image`, again never as base64.
//!
//! Every image of the editor carries an `EditorRecord` of the history, and each Copy, Save and
//! Save as of it names that record (`history::expect_editor_output`) with the document the page
//! sent last (`editor_set_document`), so all the outputs of one image make one history entry
//! that keeps its drawings editable.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use glimpsfy_annotate::BeautifyStyle;
use glimpsfy_capture::{
    ColorInfo, ColorSpace, DisplayId, Frame, LogicalRect, PhysicalSize, PixelFormat, PngEffort, ScaleFactor,
};
use glimpsfy_history::{Entry, EntryId, EntryKind};
use glimpsfy_upload::ProviderId;
use serde::{Deserialize, Serialize};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Emitter, EventTarget, Manager};

use super::capture::{CaptureId, CloseReason, Selection};
use super::error::{CommandError, CommandResult, ErrorCode};
use super::history::{EditorBase, EditorOrigin, EditorRecord};
use super::output::OutputDone;
use super::upload::UploadStarted;
use super::windows::WindowKind;
use crate::settings::ImageFormat;
use crate::{session, AppState};

/// Events this area owns. The TypeScript copy is in `src/lib/api/render.ts`.
pub mod events {
    /// To the editor window: a new image is open; read its bytes with `editor_image`. Payload:
    /// `EditorImage`.
    pub const EDITOR_OPENED: &str = "editor://opened";
}

/// The header that describes the raw pixels of a rendered call.
pub const RENDER_HEADER: &str = "x-glimpsfy-render";

/// What the page rendered.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderHeader {
    pub source: RenderSource,
    /// The image's size in pixels; the body holds `width × height × 4` bytes of RGBA.
    pub width: u32,
    pub height: u32,
    /// The colour space the page rendered in, which must be the source's.
    pub color_space: ColorSpace,
    /// For `upload_capture_rendered`: the provider the page checked with `upload_preflight`, or
    /// none for the default one.
    #[serde(default)]
    pub provider_id: Option<ProviderId>,
    /// The part of the source the pixels show, in its pixels: from the overlay, the selection's
    /// crop of the frame, which Repeat Last Area captures again. A page that leaves it out
    /// still renders; its outputs only leave the last area as it was.
    #[serde(default)]
    pub area: Option<RenderedArea>,
    /// The format to use instead of the settings' (Save as PNG this time, under a JPEG output
    /// format that would fill a removed background or a beautified corner with white): a Save
    /// writes it, and Save as suggests it in its panel and lists it first.
    #[serde(default)]
    pub format: Option<ImageFormat>,
    /// For the editor's image: the beautify style of its document, which Copy, Save, Save as and
    /// Upload put around the pixels. An overlay's style is its capture's (`capture_set_beautify`),
    /// so the overlay leaves this out.
    #[serde(default)]
    pub beautify: Option<BeautifyStyle>,
    /// For `send_rendered`: the bundle identifier of the target to send to; none copies the path.
    #[serde(default)]
    pub send_target: Option<String>,
}

/// A rectangle in the source's pixels. The numbers are `f64` so that a fractional value from the
/// page cannot make the whole header fail to parse.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct RenderedArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where the rendered pixels came from.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RenderSource {
    /// The overlay of one display of an open capture.
    Capture { capture_id: CaptureId, display_id: DisplayId },
    /// The editor's image, named by the token of its `EditorImage`.
    Editor { token: String },
}

/// The image the editor window shows, without its bytes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorImage {
    /// Changes with every image, so a page never mixes up the bytes of two images.
    pub token: String,
    /// The window's title: the file's or the history entry's name, or the captured app's.
    pub name: String,
    /// The file the image came from; `None` for a capture.
    pub path: Option<String>,
    /// `rgba`: `editor_image` answers raw pixels of `width × height`. `file`: it answers the
    /// file's own bytes, which the page decodes (`width` and `height` are known for PNG only).
    pub encoding: EditorEncoding,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The colour space the page draws in; the output is tagged with it.
    pub color_space: ColorSpace,
    /// Image pixels per point: 2 for a Retina capture, so drawings keep their size in points.
    pub scale: f64,
    /// The annotation document (JSON) to draw over the image: the one the overlay handed over,
    /// or an edit's own when the history reopened it.
    pub document: Option<String>,
    /// The history entry the image is, when the history opened it.
    pub entry_id: Option<EntryId>,
    /// How many captures the history's Combine laid into the image; `None` for any other image.
    /// Such an image has no file and no entry until its first output.
    pub combined_from: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EditorEncoding {
    Rgba,
    File,
}

#[derive(Clone)]
enum Bytes {
    /// A capture's frame; its pixels become RGBA rows when the page asks for them.
    Frame(Arc<Frame>),
    /// The file as it is on disk.
    File(Arc<Vec<u8>>),
}

struct EditorSource {
    image: EditorImage,
    bytes: Bytes,
    color: ColorInfo,
    front_app: Option<String>,
    /// The history entry its outputs make or revise.
    record: Arc<EditorRecord>,
    /// The annotation document (JSON) the page sent last, kept with the entry at an output.
    document: Option<String>,
    /// Where Save as suggests "<stem> edited": the folder of the file the image came from, or
    /// `None` for the settings' save folder (never the history's own folders).
    suggest: Option<(Option<PathBuf>, String)>,
}

/// The editor's images, newest last. The one before the newest stays until the page lets it go,
/// so a capture handed over while the editor holds unsaved drawings never takes the old image
/// away before the user decided whether to save it.
static EDITOR: Mutex<Vec<EditorSource>> = Mutex::new(Vec::new());

fn editor() -> MutexGuard<'static, Vec<EditorSource>> {
    EDITOR.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn find<'a>(images: &'a [EditorSource], token: &str) -> CommandResult<&'a EditorSource> {
    images
        .iter()
        .find(|s| s.image.token == token)
        .ok_or_else(|| CommandError::not_found("the editor's image is no longer open"))
}

/// What the subject commands need of the editor image `token`: its colour, its pixels per point
/// and its own history record.
pub(super) fn editor_facts(token: &str) -> CommandResult<(ColorInfo, f64, Arc<EditorRecord>)> {
    let images = editor();
    let source = find(&images, token)?;
    Ok((source.color.clone(), source.image.scale, source.record.clone()))
}

/// The image backgrounds the documents of the editor's images name (`frames.rs` serves their
/// previews).
pub(crate) fn editor_backgrounds() -> Vec<String> {
    let images = editor();
    images
        .iter()
        .flat_map(|s| [s.document.as_deref(), s.image.document.as_deref()])
        .flatten()
        .filter_map(|json| glimpsfy_annotate::Document::from_json(json).ok()?.beautify)
        .filter_map(|style| match style.background {
            glimpsfy_annotate::Background::Image { path, .. } => Some(path),
            _ => None,
        })
        .collect()
}

/// The tokens of the images the editor holds.
pub(super) fn editor_tokens() -> Vec<String> {
    editor().iter().map(|s| s.image.token.clone()).collect()
}

/// Makes `source` the editor's image and keeps the one before it.
fn keep(source: EditorSource) {
    let mut images = editor();
    images.push(source);
    let extra = images.len().saturating_sub(2);
    images.drain(..extra);
}

fn token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("image-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

async fn off_main<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| CommandError::internal(e.to_string()))?
}

// ------------------------------------------------------------------------- rendered output

/// `%XX` escapes back to bytes, as `encodeURIComponent` wrote them.
pub(crate) fn percent_decode(text: &str) -> Option<String> {
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

/// The header and the pixels of a rendered call, checked against each other.
pub(super) fn read_rendered(request: &Request<'_>) -> CommandResult<(RenderHeader, Vec<u8>)> {
    let raw = request
        .headers()
        .get(RENDER_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| CommandError::invalid(format!("the call has no {RENDER_HEADER} header")))?;
    let json =
        percent_decode(raw).ok_or_else(|| CommandError::invalid("the render header is not percent-encoded UTF-8"))?;
    let header: RenderHeader = serde_json::from_str(&json)
        .map_err(|e| CommandError::invalid(format!("the render header does not parse: {e}")))?;
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(CommandError::invalid("the rendered pixels must be sent as raw bytes"));
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
    // The body belongs to the call, and the encoding runs on another thread.
    Ok((header, bytes.clone()))
}

/// What a delivery needs: the pixels as a frame tagged with the source's colour, where they came
/// from, and the app the capture was taken in.
pub(super) struct Prepared {
    pub(super) frame: Frame,
    pub(super) origin: Selection,
    front_app: Option<String>,
    /// For the editor's image: its history record and the page's document at this output.
    editor: Option<(Arc<EditorRecord>, Option<String>)>,
    /// The style the outputs put around the pixels: the capture's, or the editor document's.
    beautify: Option<BeautifyStyle>,
    /// The header's format, for a Save.
    format: Option<ImageFormat>,
}

impl Prepared {
    /// Where the pixels came from, with the style and the format they leave with.
    pub(super) fn output_origin(&self, app: &AppHandle) -> crate::output::Origin {
        let mut origin = crate::output::Origin::of_selection(app, &self.origin);
        origin.beautify = self.beautify.clone();
        origin.format = self.format;
        origin
    }
}

/// Builds the output frame. With `close`, a capture is closed here, before the encoding, so the
/// overlays go away at once, as they do for a copy without drawings. With `remember`, a
/// capture's rendered area becomes the last area, as the outputs of `output::take` make it.
pub(super) fn prepare(
    app: &AppHandle,
    header: RenderHeader,
    pixels: Vec<u8>,
    close: Option<CloseReason>,
    remember: bool,
) -> CommandResult<Prepared> {
    let nowhere = LogicalRect::new(0.0, 0.0, 0.0, 0.0);
    let mut editor_record = None;
    let mut beautify = header.beautify.clone();
    let (color, scale, origin, front_app) = match &header.source {
        RenderSource::Capture { capture_id, display_id } => {
            let state = app.state::<AppState>();
            let session = state
                .sessions
                .get(*capture_id)
                .ok_or_else(|| CommandError::not_found(format!("capture #{}", capture_id.0)))?;
            let entry = session
                .display(*display_id)
                .ok_or_else(|| CommandError::not_found(format!("display {}", display_id.0)))?;
            let found = (entry.frame.color.clone(), entry.frame.scale, session.front_app.clone());
            beautify = session.beautify();
            if let (true, Some(area)) = (remember, header.area) {
                remember_area(app, &session, *capture_id, *display_id, area);
            }
            if let Some(reason) = close {
                session::close(app, *capture_id, reason);
            }
            let origin = Selection { capture_id: *capture_id, display_id: *display_id, rect: nowhere };
            (found.0, found.1, origin, found.2)
        }
        RenderSource::Editor { token } => {
            let images = editor();
            let source = find(&images, token)?;
            // The editor's output has no capture; the toast shows on the main display.
            let origin = Selection { capture_id: CaptureId(0), display_id: DisplayId(0), rect: nowhere };
            // With its background removed, the image is a new one with a record of its own
            // (`commands/subject.rs`), so the original's entry keeps its pixels.
            let record = super::subject::record_for(token).unwrap_or_else(|| source.record.clone());
            editor_record = Some((record, source.document.clone()));
            (source.color.clone(), ScaleFactor(source.image.scale), origin, source.front_app.clone())
        }
    };
    if color.space != header.color_space {
        return Err(CommandError::invalid(format!(
            "the pixels were rendered in {}, and the image is in {}",
            header.color_space.css_name(),
            color.space.css_name()
        )));
    }
    let size = PhysicalSize::new(header.width, header.height);
    let frame = Frame::new(size, header.width as usize * 4, PixelFormat::Rgba8, color, scale, pixels)?;
    Ok(Prepared { frame, origin, front_app, editor: editor_record, beautify, format: header.format })
}

/// Makes the overlay's rendered crop the area Repeat Last Area captures again. The crop arrives
/// in frame pixels and the last area is kept in the display's own points, so it is mapped back
/// by the inverse of `selection_to_frame`. A picked window stays out, as it does in
/// `output::take`, since Repeat Last Area repeats areas and not windows.
fn remember_area(
    app: &AppHandle,
    session: &session::CaptureSession,
    capture_id: CaptureId,
    display_id: DisplayId,
    area: RenderedArea,
) {
    let Some(entry) = session.display(display_id) else {
        return;
    };
    let (sx, sy) = ScaleFactor::between(entry.display.frame.size(), entry.frame.size);
    let rect = LogicalRect::new(area.x / sx.0, area.y / sy.0, area.width / sx.0, area.height / sy.0);
    let usable = [rect.x, rect.y, rect.width, rect.height].iter().all(|v| v.is_finite())
        && rect.width > 0.0
        && rect.height > 0.0;
    if !usable {
        return;
    }
    let selection = Selection { capture_id, display_id, rect };
    if session.picked_for(&selection).is_none() {
        session::remember_area(app, session, &selection);
    }
}

/// Runs one delivery of `ready`, and for the editor's image tells the history first which
/// record the hand-off that follows belongs to. The editor's deliveries run one at a time, so
/// the hand-offs come in the order of the history's tickets.
fn deliver(
    app: &AppHandle,
    ready: Prepared,
    run: impl FnOnce(crate::output::Origin, Frame, Option<&str>) -> CommandResult<OutputDone>,
) -> CommandResult<OutputDone> {
    let origin = ready.output_origin(app);
    let Some((record, document)) = ready.editor else {
        return run(origin, ready.frame, ready.front_app.as_deref());
    };
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let ticket = super::history::expect_editor_output(record, document);
    let result = run(origin, ready.frame, ready.front_app.as_deref());
    if result.is_err() {
        super::history::forget_editor_output(ticket);
    }
    result
}

/// Puts a rendered image on the clipboard as PNG (⌘C with drawings). From the overlay it also
/// closes the capture. Body: RGBA bytes; header: `x-glimpsfy-render`.
#[tauri::command]
pub async fn output_copy_rendered(app: AppHandle, request: Request<'_>) -> CommandResult<OutputDone> {
    let (header, pixels) = read_rendered(&request)?;
    off_main(move || {
        let ready = prepare(&app, header, pixels, Some(CloseReason::Copied), true)?;
        deliver(&app, ready, |origin, frame, _| crate::output::copy_frame(&app, origin, frame))
    })
    .await
}

/// Saves a rendered image as a new file in the folder of Settings › Output, named by its template
/// (⌘S with drawings). From the overlay it also closes the capture. An existing file is never
/// replaced. Body: RGBA bytes; header: `x-glimpsfy-render`.
#[tauri::command]
pub async fn output_save_rendered(app: AppHandle, request: Request<'_>) -> CommandResult<OutputDone> {
    let (header, pixels) = read_rendered(&request)?;
    off_main(move || {
        let ready = prepare(&app, header, pixels, Some(CloseReason::Saved), true)?;
        deliver(&app, ready, |origin, frame, front_app| crate::output::save_frame(&app, origin, frame, front_app, None))
    })
    .await
}

/// Asks where to save a rendered image with the system's save panel and saves it there as PNG
/// (Save as…, ⇧⌘S). A name that is taken gets " (2)" instead of replacing the file. From the
/// overlay, the overlays step aside for the panel as they do in `output::save_as`, and a
/// cancelled panel answers `cancelled` and brings them back with the capture still open. Body:
/// RGBA bytes; header: `x-glimpsfy-render`.
#[tauri::command]
pub async fn output_save_as_rendered(app: AppHandle, request: Request<'_>) -> CommandResult<OutputDone> {
    let (header, pixels) = read_rendered(&request)?;
    off_main(move || {
        let (folder, name) = suggestion(&app, &header.source, header.format);
        // The overlays sit at the screen saver level, above any panel, so the panel would open
        // where nobody can see it.
        let capture = match &header.source {
            RenderSource::Capture { capture_id, .. } => Some(*capture_id),
            RenderSource::Editor { .. } => None,
        };
        if capture.is_some() {
            crate::windows::hide_overlays(&app);
        }
        let target = match pick_save_path(&app, folder, &name, header.format) {
            Ok(target) => target,
            Err(error) => {
                if let Some(id) = capture {
                    session::reshow(&app, id);
                }
                return Err(error);
            }
        };
        let ready = match prepare(&app, header, pixels, Some(CloseReason::Saved), true) {
            Ok(ready) => ready,
            Err(error) => {
                // Without this the capture stayed open with every overlay ordered out, where
                // Esc could not reach it. A capture that `prepare` closed already stays closed.
                if let Some(id) = capture {
                    session::reshow(&app, id);
                }
                return Err(error);
            }
        };
        deliver(&app, ready, |origin, frame, front_app| {
            crate::output::save_frame(&app, origin, frame, front_app, Some(&target))
        })
    })
    .await
}

/// Sends the overlay's rendering with drawings to an app of Settings › Output (Send to app, ⌘↩
/// with drawings), or copies its file's path when the header names no target (⌥⌘C). The capture
/// closes; a target that cannot be sent to (`not_found`, `not_implemented`) leaves it open, as a
/// send without drawings does. Body: RGBA bytes; header: `x-glimpsfy-render` with `sendTarget`.
#[tauri::command]
pub async fn send_rendered(app: AppHandle, request: Request<'_>) -> CommandResult<super::send::Sent> {
    let (header, pixels) = read_rendered(&request)?;
    if !matches!(header.source, RenderSource::Capture { .. }) {
        return Err(CommandError::invalid("only the overlay sends a rendered image"));
    }
    off_main(move || {
        let target = header.send_target.as_deref().map(|id| crate::send::target(&app, id)).transpose()?;
        let ready = prepare(&app, header, pixels, Some(CloseReason::Copied), true)?;
        let origin = ready.output_origin(&app);
        crate::send::send_pixels(&app, ready.frame, origin, target)
    })
    .await
}

/// Writes a rendered image to a PNG in the app's cache and returns its path, for the commands
/// that take a file: `pin_open` with a `file` source, `upload_file`. A capture stays open. The
/// files are deleted an hour later, by a later call.
#[tauri::command]
pub async fn output_stage_rendered(app: AppHandle, request: Request<'_>) -> CommandResult<String> {
    let (header, pixels) = read_rendered(&request)?;
    off_main(move || stage(&app, header, pixels, false).map(|path| path.to_string_lossy().into_owned())).await
}

/// Uploads a rendered image (⌘U with drawings, and the editor's Upload) as a PNG to the provider
/// of the header, or to the default one. The checks of `upload_file` run before anything leaves,
/// so a refusal (`permission_denied` for a destination the user has not confirmed yet,
/// `invalid_argument` for a missing key or no provider) leaves a capture open; once the upload is
/// admitted, a capture closes as `uploaded` and the history records the rendered image before the
/// upload starts, as with `upload_capture`. Returns the upload's id; the outcome arrives as the
/// upload area's events. Body: RGBA bytes; header: `x-glimpsfy-render`.
#[tauri::command]
pub async fn upload_capture_rendered(app: AppHandle, request: Request<'_>) -> CommandResult<UploadStarted> {
    let (header, pixels) = read_rendered(&request)?;
    let provider = header.provider_id.clone();
    let capture = match header.source {
        RenderSource::Capture { capture_id, display_id } => {
            Some(Selection { capture_id, display_id, rect: LogicalRect::new(0.0, 0.0, 0.0, 0.0) })
        }
        RenderSource::Editor { .. } => None,
    };
    let path = {
        let app = app.clone();
        off_main(move || stage(&app, header, pixels, true)).await?
    };
    match capture {
        Some(selection) => {
            off_main(move || super::upload::upload_rendered_capture(&app, path, provider, selection)).await
        }
        None => super::upload::upload_file(app, path, provider, None).await,
    }
}

/// Encodes a rendered image as a new PNG in `<cache>/rendered/`, named by the template of
/// Settings › Output. `for_upload` gives it the pixels an upload of a capture has: scaled to 1×
/// when the settings say so, and beautified. A pin shows the drawing as it is.
fn stage(app: &AppHandle, header: RenderHeader, pixels: Vec<u8>, for_upload: bool) -> CommandResult<PathBuf> {
    let ready = prepare(app, header, pixels, None, for_upload)?;
    let settings = app.state::<AppState>().settings.get();
    let origin = ready.output_origin(app);
    let frame = if for_upload {
        crate::output::finish_pixels(ready.frame, &origin, settings.output.retina)
    } else {
        ready.frame
    };
    let folder = crate::paths::cache_dir(app).map_err(|e| CommandError::internal(e.to_string()))?.join("rendered");
    prune(&folder, std::time::Duration::from_secs(3600));
    let png = frame.encode_png(PngEffort::Fast)?;
    let (date, time) = crate::output::Naming::clock(chrono::Local::now());
    let naming = crate::output::Naming {
        date,
        time,
        app: ready.front_app.as_deref(),
        width: frame.size.width,
        height: frame.size.height,
    };
    Ok(crate::output::save_unique(&folder, &settings.output.file_name_template, &naming, "png", &png)?)
}

/// Deletes the files in `folder` older than `age`; a file that cannot be read or deleted stays.
/// The editor's renders and the calendar's event files (`calendar.rs`) are pruned with it.
pub(crate) fn prune(folder: &Path, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let modified = entry.metadata().and_then(|m| m.modified());
        if modified.ok().and_then(|t| t.elapsed().ok()).is_some_and(|elapsed| elapsed > age) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The folder and the name the save panel suggests: "<name> edited" beside the file the image
/// came from (in the save folder for a history entry the user never saved), or a dated name in
/// the save folder of the settings. A file in the app's own folders (a pin's copy in the cache)
/// gets the dated name in the save folder, since its folder is emptied without asking and its
/// name is the app's, not the user's.
fn suggestion(app: &AppHandle, source: &RenderSource, format: Option<ImageFormat>) -> (PathBuf, String) {
    let output = app.state::<AppState>().settings.get().output;
    let (save_folder, ext) = (output.save_folder, format.unwrap_or(output.format).extension());
    let suggest = match source {
        RenderSource::Editor { token } => find(&editor(), token).ok().and_then(|s| s.suggest.clone()),
        RenderSource::Capture { .. } => None,
    }
    .filter(|(folder, _)| !folder.as_deref().is_some_and(|f| app_owned(app, f)));
    match suggest {
        Some((folder, stem)) => (folder.filter(|f| f.is_dir()).unwrap_or(save_folder), format!("{stem} edited.{ext}")),
        None => (save_folder, format!("Glimpsfy {}.{ext}", chrono::Local::now().format("%Y-%m-%d at %H.%M.%S"))),
    }
}

/// Whether `folder` lies in the app's cache or data folder. The pins' cache is deleted at every
/// launch and the history inbox after a day, so a file the user saved there would vanish.
fn app_owned(app: &AppHandle, folder: &Path) -> bool {
    // A symlink or a `..` must not hide the folder's real place.
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let folder = real(folder);
    [crate::paths::cache_dir(app), crate::paths::data_dir(app)]
        .into_iter()
        .flatten()
        .any(|own| folder.starts_with(real(&own)))
}

/// The save panel of the dialog plugin, offering PNG, JPEG and WebP with the settings' format
/// first; the extension the user keeps picks the format (`output::save_frame`). It runs on a
/// worker thread and blocks only that thread until the user answers.
fn pick_save_path(app: &AppHandle, folder: PathBuf, name: &str, format: Option<ImageFormat>) -> CommandResult<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    // `dialog()` panics when the plugin is not registered, and registering it is the app shell's.
    if app.try_state::<tauri_plugin_dialog::Dialog<tauri::Wry>>().is_none() {
        return Err(CommandError::not_implemented("the save panel"));
    }
    let format = format.unwrap_or_else(|| app.state::<AppState>().settings.get().output.format);
    let mut filters: [(ImageFormat, &str, &[&str]); 3] = [
        (ImageFormat::Png, "PNG", &["png"]),
        (ImageFormat::Jpeg, "JPEG", &["jpg", "jpeg"]),
        (ImageFormat::Webp, "WebP", &["webp"]),
    ];
    filters.sort_by_key(|(f, ..)| *f != format);
    let mut dialog = app.dialog().file().set_file_name(name);
    if folder.is_dir() {
        dialog = dialog.set_directory(folder);
    }
    for (_, label, extensions) in filters {
        dialog = dialog.add_filter(label, extensions);
    }
    // A menu bar app is not active, and an inactive app's panel opens behind the front app.
    crate::output::bring_forward(app);
    let picked = dialog
        .blocking_save_file()
        .ok_or_else(|| CommandError::new(ErrorCode::Cancelled, "the save panel was cancelled"))?;
    picked.into_path().map_err(|e| CommandError::invalid(e.to_string()))
}

// ------------------------------------------------------------------------------- the editor

/// What a PNG tells about itself: its colour space, its size, and its pixels per point when it
/// carries a resolution (macOS writes 144 dpi into a Retina screenshot).
fn png_facts(bytes: &[u8]) -> Option<(ColorSpace, (u32, u32), Option<f64>)> {
    let reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
    let info = reader.info();
    let space = match info.icc_profile.as_deref() {
        Some(profile) if icc_is_wide(profile) => ColorSpace::DisplayP3,
        _ => ColorSpace::Srgb,
    };
    let scale = info.pixel_dims.and_then(|dims| {
        // 2834.6 pixels per metre is 72 dpi, one pixel per point.
        let per_point = (dims.unit == png::Unit::Meter).then(|| dims.xppu as f64 / 2834.6)?;
        let rounded = (per_point * 4.0).round() / 4.0;
        (1.0..=4.0).contains(&rounded).then_some(rounded)
    });
    Some((space, (info.width, info.height), scale))
}

/// Whether an ICC profile describes a gamut wider than sRGB (Display P3, a Mac display's own
/// profile). Its red primary is read from the `rXYZ` tag: sRGB's sits at x = 0.648, Display P3's
/// at x = 0.682. A profile whose description names P3 counts as wide too.
fn icc_is_wide(profile: &[u8]) -> bool {
    let be32 = |at: usize| profile.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let Some(count) = be32(128) else {
        return false;
    };
    for i in 0..count.min(64) {
        let entry = 132 + i * 12;
        let (Some(signature), Some(offset), Some(size)) =
            (profile.get(entry..entry + 4), be32(entry + 4), be32(entry + 8))
        else {
            return false;
        };
        let Some(tag) = profile.get(offset..offset.saturating_add(size).min(profile.len())) else {
            continue;
        };
        match signature {
            b"rXYZ" if tag.len() >= 20 => {
                let fixed =
                    |at: usize| i32::from_be_bytes([tag[at], tag[at + 1], tag[at + 2], tag[at + 3]]) as f64 / 65536.0;
                let (x, y, z) = (fixed(8), fixed(12), fixed(16));
                if x + y + z > 0.0 && x / (x + y + z) > 0.665 {
                    return true;
                }
            }
            b"desc" => {
                // Version 2 profiles store the description as ASCII, version 4 as UTF-16.
                let units: Vec<u16> = tag.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                if String::from_utf8_lossy(tag).contains("P3") || String::from_utf16_lossy(&units).contains("P3") {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Shows the editor window and tells its page that a new image is open.
fn show_editor(app: &AppHandle, image: &EditorImage) {
    crate::windows::open(app, WindowKind::Editor);
    let _ = app.emit_to(EventTarget::webview_window(WindowKind::Editor.label()), events::EDITOR_OPENED, image.clone());
}

/// What a file's bytes say about drawing on them: their colour space, their size when known,
/// and their pixels per point when the file carries a resolution. A PNG keeps its own colour
/// space; any other format is drawn in Display P3, which holds every sRGB colour, so nothing of
/// a wide-gamut JPEG or HEIC is clipped.
fn file_facts(bytes: &[u8]) -> (ColorSpace, Option<(u32, u32)>, Option<f64>) {
    match png_facts(bytes) {
        Some((space, size, scale)) => (space, Some(size), scale),
        None => (ColorSpace::DisplayP3, None, None),
    }
}

/// Opens an image file in the editor: a file dropped on the editor, a pin's image, or, with
/// `entry_id`, an entry of the history (see [`editor_open_entry`]). Every format the web view
/// decodes is accepted (PNG, JPEG, HEIC, WebP, TIFF…).
#[tauri::command]
pub async fn editor_open(app: AppHandle, path: PathBuf, entry_id: Option<EntryId>) -> CommandResult<EditorImage> {
    if let Some(id) = entry_id {
        let entry = super::history::history_get(app.clone(), id).await?;
        return editor_open_entry(app, entry).await;
    }
    let image = off_main(move || {
        if !std::fs::metadata(&path)?.is_file() {
            return Err(CommandError::invalid(format!("{} is not a file", path.display())));
        }
        let bytes = Arc::new(std::fs::read(&path)?);
        let (space, size, file_scale) = file_facts(&bytes);
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Glimpsfy".into());
        let image = EditorImage {
            token: token(),
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| stem.clone()),
            path: Some(path.to_string_lossy().into_owned()),
            encoding: EditorEncoding::File,
            width: size.map(|s| s.0),
            height: size.map(|s| s.1),
            color_space: space,
            scale: file_scale.unwrap_or(1.0),
            document: None,
            entry_id: None,
            combined_from: None,
        };
        let origin = EditorOrigin { name: Some(stem.clone()), ..EditorOrigin::default() };
        keep(EditorSource {
            image: image.clone(),
            record: EditorRecord::new(EditorBase::Bytes(bytes.clone()), origin, None, None),
            bytes: Bytes::File(bytes),
            color: ColorInfo::for_space(space),
            front_app: None,
            document: None,
            suggest: Some((path.parent().map(Path::to_path_buf), stem)),
        });
        Ok(image)
    })
    .await?;
    show_editor(&app, &image);
    Ok(image)
}

/// Opens a history entry in the editor under its name and with its pixels per point. An entry
/// the editor made comes back as the image under its drawings with its document, so the drawings
/// are still objects, and the editor's outputs revise that entry. Any other capture opens as its
/// image; an output with drawings adds an entry of its own and leaves the capture as it was, and
/// an output without any updates the capture's entry.
pub async fn editor_open_entry(app: AppHandle, entry: Entry) -> CommandResult<EditorImage> {
    let handle = app.clone();
    let image = off_main(move || {
        let app = handle;
        let opened = super::history::editor_source(&entry)?;
        let bytes = Arc::new(std::fs::read(&opened.image)?);
        let (space, size, file_scale) = file_facts(&bytes);
        // The page reads the format from the path, so an edit shows the format of its file and
        // not of the image kept under its drawings.
        let shown = match (&opened.edit_of, entry.media_path()) {
            (Some(_), Some(media)) if media.is_file() => media.to_path_buf(),
            _ => opened.image.clone(),
        };
        let stem = super::history::file_stem_of(&app, &entry);
        let image = EditorImage {
            token: token(),
            name: super::history::display_name(&app, &entry),
            path: Some(shown.to_string_lossy().into_owned()),
            encoding: EditorEncoding::File,
            width: size.map(|s| s.0),
            height: size.map(|s| s.1),
            color_space: space,
            scale: entry.scale.or(file_scale).filter(|s| *s >= 1.0).unwrap_or(1.0),
            document: opened.document.clone(),
            entry_id: Some(entry.id.clone()),
            combined_from: None,
        };
        let origin = EditorOrigin {
            name: Some(entry.name.clone()).filter(|n| !n.trim().is_empty()),
            app_name: entry.source.app_name.clone(),
            bundle_id: entry.source.bundle_id.clone(),
            window_title: entry.source.window_title.clone(),
            display_id: entry.display.as_ref().map(|d| d.id),
            display_name: entry.display.as_ref().and_then(|d| d.name.clone()),
            created_at_ms: None,
        };
        let source = opened.edit_of.is_none().then(|| entry.id.clone());
        let record = EditorRecord::new(EditorBase::Bytes(bytes.clone()), origin, source, opened.edit_of);
        keep(EditorSource {
            image: image.clone(),
            record,
            bytes: Bytes::File(bytes),
            color: ColorInfo::for_space(space),
            front_app: entry.source.app_name.clone(),
            document: opened.document,
            suggest: Some((entry.saved_path.as_deref().and_then(Path::parent).map(Path::to_path_buf), stem)),
        });
        Ok(image)
    })
    .await?;
    show_editor(&app, &image);
    Ok(image)
}

/// Hands the overlay's capture to the editor with its drawings: the whole frame of the display,
/// so the crop can still be widened there, and the document as the overlay had it, whose crop is
/// the selection. The capture closes as `editor` and the editor window opens. The history
/// records the capture at the editor's first output, with what the capture said about its app,
/// window, display and time; an image closed without an output is not recorded, as a capture
/// cancelled in the overlay is not.
#[tauri::command]
pub fn editor_open_capture(
    app: AppHandle,
    capture_id: CaptureId,
    display_id: DisplayId,
    document: String,
) -> CommandResult<EditorImage> {
    glimpsfy_annotate::Document::from_json(&document).map_err(|e| CommandError::invalid(e.to_string()))?;
    let state = app.state::<AppState>();
    let session =
        state.sessions.get(capture_id).ok_or_else(|| CommandError::not_found(format!("capture #{}", capture_id.0)))?;
    let entry =
        session.display(display_id).ok_or_else(|| CommandError::not_found(format!("display {}", display_id.0)))?;
    let frame = entry.frame.clone();
    let taken = session.origin(display_id);
    let origin = EditorOrigin {
        name: None,
        app_name: taken.app_name,
        bundle_id: taken.bundle_id,
        window_title: taken.window_title,
        display_id: taken.display.as_ref().map(|d| d.id.0),
        display_name: taken.display.as_ref().map(|d| d.name.clone()),
        created_at_ms: taken.taken_at.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64),
    };
    let image = EditorImage {
        token: token(),
        name: session.front_app.clone().unwrap_or_else(|| super::history::unnamed(&app, EntryKind::Screenshot, None)),
        path: None,
        encoding: EditorEncoding::Rgba,
        width: Some(frame.size.width),
        height: Some(frame.size.height),
        color_space: frame.color.space,
        scale: frame.scale.0,
        document: Some(document),
        entry_id: None,
        combined_from: None,
    };
    keep(EditorSource {
        image: image.clone(),
        color: frame.color.clone(),
        record: EditorRecord::new(EditorBase::Frame(frame.clone()), origin, None, None),
        bytes: Bytes::Frame(frame),
        front_app: session.front_app.clone(),
        document: image.document.clone(),
        suggest: None,
    });
    session::close(&app, capture_id, CloseReason::Editor);
    show_editor(&app, &image);
    Ok(image)
}

// ------------------------------------------------------------------------------ the combine

/// The longest side a combined image may have. Past it a clipboard and an upload host struggle,
/// so the sheet lays the pieces out at 1x first and refuses only what is too large even there.
pub const COMBINE_MAX_SIDE: u32 = 16_384;

/// What the history's Combine asks for: the size of the combined image and where each capture
/// lies on it, in its pixels, as `src/history/combine.ts` laid them out for the sheet's preview,
/// so the image is the preview. Pieces are drawn in their order, a later one over an earlier one.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CombineRequest {
    pub width: u32,
    pub height: u32,
    /// Pixels per point of the combined image: the highest of the captures', or 1.
    pub scale: f64,
    pub background: CombineBackground,
    pub pieces: Vec<CombinePiece>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CombineBackground {
    Clear,
    White,
    Gray,
    Dark,
}

impl CombineBackground {
    /// The colour under the pieces. White, the grey and the dark are neutral, and a neutral has
    /// the same values in sRGB and in Display P3, so one set serves both.
    fn rgba(self) -> [u8; 4] {
        match self {
            CombineBackground::Clear => [0, 0, 0, 0],
            CombineBackground::White => [255, 255, 255, 255],
            CombineBackground::Gray => [0xE9, 0xE9, 0xEE, 255],
            CombineBackground::Dark => [0x1C, 0x1C, 0x1E, 255],
        }
    }
}

/// One capture on the combined image, in its pixels. The numbers are `f64` so that a fractional
/// value from the page cannot make the request fail to parse; they are rounded here.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinePiece {
    pub entry_id: EntryId,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The colour space a combined image is drawn in. On macOS a capture is sRGB or Display P3, and
/// Display P3 holds both without clipping; elsewhere every frame is sRGB.
fn combine_space() -> ColorSpace {
    if cfg!(target_os = "macos") {
        ColorSpace::DisplayP3
    } else {
        ColorSpace::Srgb
    }
}

/// Lays the history's screenshots onto one new image and opens it in the editor as an image
/// without a file or an entry; its first output adds an entry of its own, and the captures stay
/// as they were. Each capture is read in the combined image's colour space and, when its piece is
/// another size (a 1x capture in a 2x image), resized to it: an exact 2x repeats every pixel.
/// Fails with `invalid_argument` for fewer than two pieces, a piece that is not a screenshot or a
/// size past [`COMBINE_MAX_SIDE`], and with `not_found` for a capture whose file is gone.
#[tauri::command]
pub async fn editor_combine(app: AppHandle, request: CombineRequest) -> CommandResult<EditorImage> {
    let handle = app.clone();
    let image = off_main(move || combine(&handle, request)).await?;
    show_editor(&app, &image);
    Ok(image)
}

fn combine(app: &AppHandle, request: CombineRequest) -> CommandResult<EditorImage> {
    let (width, height) = (request.width, request.height);
    if width == 0 || height == 0 || width > COMBINE_MAX_SIDE || height > COMBINE_MAX_SIDE {
        return Err(CommandError::invalid(format!(
            "a combined image of {width} × {height} pixels is past the limit of {COMBINE_MAX_SIDE}"
        )));
    }
    if !(request.scale.is_finite() && (1.0..=4.0).contains(&request.scale)) {
        return Err(CommandError::invalid(format!("{} pixels per point cannot be combined", request.scale)));
    }
    if request.pieces.len() < 2 {
        return Err(CommandError::invalid("combining needs at least two screenshots"));
    }
    let store = super::history::store(app)?;
    let space = combine_space();
    let mut canvas = Canvas::new(width, height, request.background.rgba());
    for piece in &request.pieces {
        let place = [piece.x, piece.y, piece.width, piece.height].map(f64::round);
        if place.iter().any(|v| !v.is_finite()) || place[2] < 1.0 || place[3] < 1.0 {
            return Err(CommandError::invalid(format!("the piece of {} has no size", piece.entry_id)));
        }
        let entry = store.get(&piece.entry_id)?;
        if entry.kind != EntryKind::Screenshot {
            return Err(CommandError::invalid(format!("{} is not a screenshot", piece.entry_id)));
        }
        let path = entry
            .media_path()
            .filter(|p| p.is_file())
            .ok_or_else(|| CommandError::not_found(format!("the file of {}", piece.entry_id)))?;
        let mut frame = glimpsfy_ocr::load_image_as(path, space)?;
        let size = PhysicalSize::new(
            place[2].min(COMBINE_MAX_SIDE as f64) as u32,
            place[3].min(COMBINE_MAX_SIDE as f64) as u32,
        );
        if frame.size != size {
            frame = frame.resized(size);
        }
        canvas.draw(&frame, place[0] as i64, place[1] as i64);
    }
    let frame = Arc::new(Frame::new(
        PhysicalSize::new(width, height),
        width as usize * 4,
        PixelFormat::Rgba8,
        ColorInfo::for_space(space),
        ScaleFactor(request.scale),
        canvas.data,
    )?);

    let language = app.state::<AppState>().settings.get().general.language;
    let (date, time) = crate::output::Naming::clock(chrono::Local::now());
    let name = crate::i18n::text(language, "editor.combinedName", &[("date", &date), ("time", &time)]);
    let image = EditorImage {
        token: token(),
        name,
        path: None,
        encoding: EditorEncoding::Rgba,
        width: Some(width),
        height: Some(height),
        color_space: space,
        scale: request.scale,
        document: None,
        entry_id: None,
        combined_from: Some(request.pieces.len() as u32),
    };
    keep(EditorSource {
        image: image.clone(),
        color: frame.color.clone(),
        // No source entry: the combined image is a picture of its own, so its first output adds
        // a new entry, even one without drawings.
        record: EditorRecord::new(EditorBase::Frame(frame.clone()), EditorOrigin::default(), None, None),
        bytes: Bytes::Frame(frame),
        front_app: None,
        document: None,
        suggest: None,
    });
    Ok(image)
}

/// The combined image while its pieces are laid on: tight RGBA rows with straight alpha.
struct Canvas {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32, fill: [u8; 4]) -> Canvas {
        let data = fill.repeat(width as usize * height as usize);
        Canvas { width, height, data }
    }

    /// Lays `frame` (RGBA) with its top left at `x`, `y` over what is there (source-over with
    /// straight alpha); the part outside the canvas is cut off.
    fn draw(&mut self, frame: &Frame, x: i64, y: i64) {
        let left = x.max(0);
        let top = y.max(0);
        let right = (x + frame.size.width as i64).min(self.width as i64);
        let bottom = (y + frame.size.height as i64).min(self.height as i64);
        if left >= right || top >= bottom {
            return;
        }
        let span = (right - left) as usize * 4;
        for row in top..bottom {
            let from = (row - y) as usize * frame.bytes_per_row + (left - x) as usize * 4;
            let to = (row as usize * self.width as usize + left as usize) * 4;
            let source = &frame.data[from..from + span];
            let target = &mut self.data[to..to + span];
            for (d, s) in target.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                over(d, s);
            }
        }
    }
}

/// `source` over `target`, both RGBA with straight alpha.
fn over(target: &mut [u8], source: &[u8]) {
    let sa = source[3] as u32;
    if sa == 255 || target[3] == 0 {
        target.copy_from_slice(source);
        return;
    }
    if sa == 0 {
        return;
    }
    // In 255ths: the result's alpha, then each channel weighted by how much of it shows.
    let da = target[3] as u32 * (255 - sa) / 255;
    let a = sa + da;
    for (t, s) in target[..3].iter_mut().zip(&source[..3]) {
        *t = ((*s as u32 * sa + *t as u32 * da + a / 2) / a) as u8;
    }
    target[3] = a.min(255) as u8;
}

/// Keeps the page's annotation document (JSON) for the editor's image `token`, so the next Copy,
/// Save or Save as stores it with the history entry and the drawings can be edited again. The
/// page sends it right before each of them; `None` forgets it.
#[tauri::command]
pub fn editor_set_document(token: String, document: Option<String>) -> CommandResult<()> {
    if let Some(document) = &document {
        glimpsfy_annotate::Document::from_json(document).map_err(|e| CommandError::invalid(e.to_string()))?;
    }
    let mut images = editor();
    let source = images
        .iter_mut()
        .find(|s| s.image.token == token)
        .ok_or_else(|| CommandError::not_found("the editor's image is no longer open"))?;
    source.document = document;
    Ok(())
}

/// The image the editor shows, for a page that loads after `editor://opened` was sent.
#[tauri::command]
pub fn editor_current() -> Option<EditorImage> {
    editor().last().map(|s| s.image.clone())
}

/// The bytes of the editor's image as a raw binary response: RGBA rows for a capture, the file's
/// own bytes for a file (see `EditorImage::encoding`).
#[tauri::command]
pub async fn editor_image(token: String) -> CommandResult<Response> {
    let bytes = find(&editor(), &token)?.bytes.clone();
    off_main(move || {
        Ok(Response::new(match bytes {
            Bytes::Frame(frame) => frame.to_rgba(),
            Bytes::File(bytes) => bytes.as_ref().clone(),
        }))
    })
    .await
}

/// Forgets one of the editor's images: the old one once the page moved on, every one when the
/// window closes, so a 5K frame does not stay in memory.
#[tauri::command]
pub fn editor_close(token: String) {
    super::subject::forget(&token);
    editor().retain(|s| s.image.token != token);
}
