//! What happens to the pixels of a capture: they go to the clipboard as PNG, to a new file in the
//! format of Settings › Output, or to a file the user names (Save as), and the pages, the toast
//! and the history hear about it.
//!
//! Every image leaves through one entry point, [`deliver`] (several at once through
//! [`deliver_many`], for all displays): the overlay's selection, a picked window, a display
//! captured without the overlay and the editor's flattened drawing alike. It closes the capture
//! first, so the overlay never waits for an encoder, then plays the shutter, scales to 1× when
//! the settings say so, encodes, writes, and reports: `output://copied` or `output://saved`, a
//! toast, and `output://handoff` for the history.
//!
//! - **Colour.** Nothing is converted: the frame keeps the display's colour space, and its ICC
//!   profile goes into every format (PNG `iCCP`, JPEG `APP2`, WebP `ICCP`), so a Display P3
//!   capture stays Display P3 wherever it is opened.
//! - **Formats.** PNG at balanced effort for files and fast effort for the clipboard; JPEG at the
//!   quality of the settings (50–100), over white where the image is transparent; WebP lossless
//!   (no pure-Rust encoder makes lossy WebP), which ignores the quality.
//! - **Files.** A save never replaces a file (`output/naming.rs`). A save folder that is not
//!   there (an unplugged drive), read-only or full sends the file to the default folder
//!   (Pictures › Glimpsfy) instead, and the toast names the folder that failed.

mod naming;
mod platform;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use glimpsfy_capture::{selection_to_frame, ColorInfo, Display, Frame, ImageEncoding, PixelFormat, PngEffort};
use glimpsfy_history::EntryKind;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::capture::{CaptureId, CaptureMode, CloseReason, Selection};
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::commands::output::events::{CaptureHandoff, HANDOFF};
use crate::commands::output::{OutputDone, OutputFailed};
use crate::settings::{HistoryRetention, ImageFormat, Output, RetinaScale};
use crate::windows::{Toast, ToastKind};
use crate::{events, session, trace, windows, AppState};

pub use naming::{file_stem, file_stem_titled, save_unique, save_unique_titled, Naming};

/// The toast's thumbnail fits in this many pixels (2× its 64 × 44 point slot).
const THUMBNAIL: (u32, u32) = (128, 88);
/// The folder in the app data directory where clipboard-only captures wait for the history.
const INBOX: &str = "history-inbox";
/// A file in the inbox older than this is deleted by the next copy.
const INBOX_KEEP: Duration = Duration::from_secs(24 * 60 * 60);

// ------------------------------------------------------------------------------- the contract

/// What to do with an image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputAction {
    /// To the clipboard as PNG.
    Copy,
    /// To a new file in the save folder, named by the template, in the format of the settings.
    Save,
    /// To the path the user picked; `None` asks with the system's save panel first. The format
    /// follows the extension (`.png`, `.jpg`, `.jpeg`, `.webp`), the settings' otherwise.
    SaveAs(Option<PathBuf>),
}

/// Where an image came from: what the file name, the toast and the history say about it.
#[derive(Clone, Debug)]
pub struct Origin {
    /// The capture; `CaptureId(0)` for an image that belongs to none (the editor).
    pub capture_id: CaptureId,
    pub mode: CaptureMode,
    /// The display the pixels came from.
    pub display: Option<Display>,
    /// The frontmost app at capture time, or the captured window's app (`{app}`).
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    /// The captured window's title, or the frontmost window's (`{title}`).
    pub window_title: Option<String>,
    /// When the pixels were taken (`{date}`, `{time}`).
    pub taken_at: SystemTime,
}

impl Origin {
    /// The origin of an image of `selection`'s capture, whether it is still open or was closed a
    /// moment ago (the annotation flow closes it before it delivers). An unknown capture, such
    /// as the editor's `CaptureId(0)`, gives an origin with nothing but the time.
    pub fn of_selection(app: &AppHandle, selection: &Selection) -> Origin {
        session::origin(app, selection.capture_id, selection.display_id)
            .unwrap_or_else(|| Origin::unknown(selection.capture_id))
    }

    pub fn unknown(capture_id: CaptureId) -> Origin {
        Origin {
            capture_id,
            mode: CaptureMode::Area,
            display: None,
            app_name: None,
            bundle_id: None,
            window_title: None,
            taken_at: SystemTime::now(),
        }
    }
}

/// Delivers one image: the single entry point for pixels leaving the app (see the module
/// documentation). The capture of `origin` is closed first when it is still open.
pub fn deliver(app: &AppHandle, frame: Frame, origin: Origin, action: OutputAction) -> CommandResult<OutputDone> {
    deliver_many(app, vec![(frame, origin)], action)?
        .into_iter()
        .next()
        .ok_or_else(|| CommandError::internal("no image was delivered"))
}

/// Delivers several images of one capture together (all displays): one clipboard write with an
/// item per image, or one file per image, and one toast and one hand-off per image.
pub fn deliver_many(
    app: &AppHandle,
    images: Vec<(Frame, Origin)>,
    action: OutputAction,
) -> CommandResult<Vec<OutputDone>> {
    let Some(first) = images.first().map(|(_, origin)| origin.clone()) else {
        return Ok(Vec::new());
    };
    let settings = app.state::<AppState>().settings.get();

    // The overlay goes before anything slow, and before a save panel that it would cover.
    let reason = if action == OutputAction::Copy { CloseReason::Copied } else { CloseReason::Saved };
    for id in images.iter().map(|(_, o)| o.capture_id).filter(|id| id.0 != 0) {
        session::close(app, id, reason);
    }
    if settings.general.shutter_sound {
        shutter_once(first.capture_id);
    }

    let images: Vec<(Frame, Origin)> = images
        .into_iter()
        .map(|(frame, origin)| match settings.output.retina {
            RetinaScale::ScaleTo1x => (frame.at_1x().unwrap_or(frame), origin),
            RetinaScale::Keep2x => (frame, origin),
        })
        .collect();

    let result = match &action {
        OutputAction::Copy => copy_images(app, &settings.output, &images),
        OutputAction::Save => save_images(app, &settings.output, &images, None),
        OutputAction::SaveAs(Some(path)) => save_images(app, &settings.output, &images, Some(path)),
        OutputAction::SaveAs(None) => {
            let (frame, origin) = &images[0];
            pick_path(app, &settings.output, frame, origin)
                .and_then(|path| save_images(app, &settings.output, &images, Some(&path)))
        }
    };
    report(app, &images, &first, result)
}

// ------------------------------------------------------------------------- selection flows

/// Copies the selection (⌘C, Return), or the picked window when the selection is still its.
pub fn copy(app: &AppHandle, selection: Selection) -> CommandResult<OutputDone> {
    let (frame, origin) = take(app, &selection)?;
    deliver(app, frame, origin, OutputAction::Copy)
}

/// Saves the selection as a new file (⌘S).
pub fn save(app: &AppHandle, selection: Selection) -> CommandResult<OutputDone> {
    let (frame, origin) = take(app, &selection)?;
    deliver(app, frame, origin, OutputAction::Save)
}

/// Saves the selection under a path (⇧⌘S). Without one, the overlays step aside for the save
/// panel; a cancelled panel brings them back with the selection as it was.
pub fn save_as(app: &AppHandle, selection: Selection, path: Option<PathBuf>) -> CommandResult<OutputDone> {
    let (frame, origin) = take(app, &selection)?;
    let path = match path {
        Some(path) => path,
        None => {
            windows::hide_overlays(app);
            let settings = app.state::<AppState>().settings.get();
            match pick_path(app, &settings.output, &frame, &origin) {
                Ok(path) => path,
                Err(error) => {
                    session::reshow(app, selection.capture_id);
                    return Err(error);
                }
            }
        }
    };
    deliver(app, frame, origin, OutputAction::SaveAs(Some(path)))
}

/// The pixels of a selection: the picked window's own image when the selection is still that
/// window, the crop of the frozen frame otherwise. The capture stays open; `deliver` closes it.
/// Upload takes its pixels here too, so an upload and a copy of one selection are the same image.
pub(crate) fn take(app: &AppHandle, selection: &Selection) -> CommandResult<(Frame, Origin)> {
    let state = app.state::<AppState>();
    let id = selection.capture_id;
    let session = state.sessions.get(id).ok_or_else(|| CommandError::not_found(format!("capture #{}", id.0)))?;
    let entry = session
        .display(selection.display_id)
        .ok_or_else(|| CommandError::not_found(format!("display {}", selection.display_id.0)))?;
    let mut origin = session.origin(selection.display_id);

    if let Some(picked) = session.picked_for(selection) {
        origin.mode = CaptureMode::Window;
        origin.app_name = Some(picked.app_name.clone());
        origin.bundle_id = picked.bundle_id.clone();
        origin.window_title = picked.title.clone();
        return Ok(((*picked.frame).clone(), origin));
    }

    let rect = selection_to_frame(selection.rect, entry.display.frame.size(), entry.frame.size)
        .ok_or_else(|| CommandError::invalid("the selection is empty or outside its display"))?;
    let frame = entry.frame.crop(rect)?;
    session::remember_area(app, &session, selection);
    Ok((frame, origin))
}

// ------------------------------------------------------------------------------ delivering

/// One delivered image, before it is reported.
struct Delivered {
    done: OutputDone,
    copied: bool,
    image_path: Option<PathBuf>,
}

fn copy_images(app: &AppHandle, output: &Output, images: &[(Frame, Origin)]) -> CommandResult<Vec<Delivered>> {
    let pngs = images
        .iter()
        .map(|(frame, _)| frame.encode(ImageEncoding::Png(PngEffort::Fast)))
        .collect::<Result<Vec<_>, _>>()?;
    let frames: Vec<&Frame> = images.iter().map(|(frame, _)| frame).collect();
    platform::copy_pngs(app, &pngs, &frames)?;

    let inbox = (output.history_retention != HistoryRetention::Off).then(|| inbox(app)).flatten();
    Ok(images
        .iter()
        .zip(&pngs)
        .enumerate()
        .map(|(i, ((frame, origin), png))| Delivered {
            done: done(origin, frame, png.len(), None, "png", None),
            copied: true,
            image_path: inbox.as_deref().and_then(|dir| write_inbox(dir, origin, i, png)),
        })
        .collect())
}

fn save_images(
    app: &AppHandle,
    output: &Output,
    images: &[(Frame, Origin)],
    target: Option<&Path>,
) -> CommandResult<Vec<Delivered>> {
    let mut delivered = Vec::with_capacity(images.len());
    let mut clipboard = Vec::new();
    for (i, (frame, origin)) in images.iter().enumerate() {
        let format = target.and_then(format_of).unwrap_or(output.format);
        let bytes = frame.encode(encoding(output, format))?;
        let (path, fallback_from) = match target {
            Some(path) => (write_picked(path, i, format, &bytes)?, None),
            None => write_in_folder(output, frame, origin, format, &bytes)?,
        };
        if output.copy_after_save {
            clipboard.push(if format == ImageFormat::Png {
                bytes.clone()
            } else {
                frame.encode(ImageEncoding::Png(PngEffort::Fast))?
            });
        }
        delivered.push(Delivered {
            done: done(origin, frame, bytes.len(), Some(path), format_name(format), fallback_from),
            copied: false,
            image_path: None,
        });
    }
    if output.copy_after_save {
        let frames: Vec<&Frame> = images.iter().map(|(frame, _)| frame).collect();
        match platform::copy_pngs(app, &clipboard, &frames) {
            Ok(()) => delivered.iter_mut().for_each(|d| d.copied = true),
            // The files are written; a clipboard that refused is worth a line, not a failure.
            Err(error) => trace::log(format!("[glimpsfy] copy after save: {}", error.message)),
        }
    }
    Ok(delivered)
}

fn encoding(output: &Output, format: ImageFormat) -> ImageEncoding {
    match format {
        ImageFormat::Png => ImageEncoding::Png(PngEffort::Balanced),
        ImageFormat::Jpeg => {
            ImageEncoding::Jpeg { quality: output.quality.clamp(50, 100) as u8, background: [255, 255, 255] }
        }
        ImageFormat::Webp => ImageEncoding::WebpLossless,
    }
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Webp => "webp",
    }
}

/// The format a picked path asks for by its extension.
fn format_of(path: &Path) -> Option<ImageFormat> {
    match path.extension()?.to_string_lossy().to_ascii_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// Writes into the save folder of the settings, or into the default folder when that one cannot
/// take the file; the second value is the folder that failed.
fn write_in_folder(
    output: &Output,
    frame: &Frame,
    origin: &Origin,
    format: ImageFormat,
    bytes: &[u8],
) -> CommandResult<(PathBuf, Option<String>)> {
    let (date, time) = Naming::clock(chrono::DateTime::<chrono::Local>::from(origin.taken_at));
    let naming =
        Naming { date, time, app: origin.app_name.as_deref(), width: frame.size.width, height: frame.size.height };
    let attempt = |folder: &Path| -> Result<PathBuf, (bool, String)> {
        naming::check_folder(folder).map_err(|problem| (true, problem.to_string()))?;
        save_unique_titled(
            folder,
            &output.file_name_template,
            &naming,
            origin.window_title.as_deref(),
            format.extension(),
            bytes,
        )
        .map_err(|error| (naming::folder_failed(&error), error.to_string()))
    };

    let chosen = &output.save_folder;
    match attempt(chosen) {
        Ok(path) => Ok((path, None)),
        Err((true, first)) => {
            let default = Output::default().save_folder;
            if &default == chosen {
                return Err(folder_error(chosen, &first));
            }
            match attempt(&default) {
                Ok(path) => {
                    trace::log(format!(
                        "[glimpsfy] save folder {} failed ({first}); saved to {} instead",
                        chosen.display(),
                        default.display()
                    ));
                    Ok((path, Some(chosen.to_string_lossy().into_owned())))
                }
                Err((_, second)) => Err(CommandError::new(
                    ErrorCode::Io,
                    format!(
                        "The capture could not be saved in “{}” ({first}) or in “{}” ({second}). Choose another folder in Settings › Output.",
                        chosen.display(),
                        default.display()
                    ),
                )),
            }
        }
        Err((false, error)) => Err(folder_error(chosen, &error)),
    }
}

fn folder_error(folder: &Path, reason: &str) -> CommandError {
    CommandError::new(
        ErrorCode::Io,
        format!(
            "The capture could not be saved in “{}” ({reason}). Choose another folder in Settings › Output.",
            folder.display()
        ),
    )
}

/// Writes under the name the user picked; the `index`-th image of several gets " 2", " 3"…
fn write_picked(path: &Path, index: usize, format: ImageFormat, bytes: &[u8]) -> CommandResult<PathBuf> {
    let folder = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| CommandError::invalid("the path names no folder"))?;
    let (stem, extension) = match format_of(path) {
        Some(_) => (
            path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| format.extension().into()),
        ),
        None => {
            (path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), format.extension().into())
        }
    };
    let stem = if index == 0 { stem } else { format!("{stem} {}", index + 1) };
    naming::save_named(folder, &stem, &extension, bytes).map_err(|error| folder_error(folder, &error.to_string()))
}

/// Asks for a path with the system's save panel, starting in the save folder with the name the
/// template gives. A cancelled panel answers `cancelled`.
fn pick_path(app: &AppHandle, output: &Output, frame: &Frame, origin: &Origin) -> CommandResult<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    if app.try_state::<tauri_plugin_dialog::Dialog<tauri::Wry>>().is_none() {
        return Err(CommandError::not_implemented("the save panel"));
    }
    let (date, time) = Naming::clock(chrono::DateTime::<chrono::Local>::from(origin.taken_at));
    let naming =
        Naming { date, time, app: origin.app_name.as_deref(), width: frame.size.width, height: frame.size.height };
    let stem = file_stem_titled(&output.file_name_template, &naming, origin.window_title.as_deref(), 1);
    let mut dialog = app.dialog().file().set_file_name(format!("{stem}.{}", output.format.extension()));
    if output.save_folder.is_dir() {
        dialog = dialog.set_directory(&output.save_folder);
    }
    // The settings' format first, so the panel starts with it.
    let mut filters: [(ImageFormat, &str, &[&str]); 3] = [
        (ImageFormat::Png, "PNG", &["png"]),
        (ImageFormat::Jpeg, "JPEG", &["jpg", "jpeg"]),
        (ImageFormat::Webp, "WebP", &["webp"]),
    ];
    filters.sort_by_key(|(format, ..)| *format != output.format);
    for (_, name, extensions) in filters {
        dialog = dialog.add_filter(name, extensions);
    }
    // A menu bar app is not active, and an inactive app's panel opens behind the front app.
    platform::bring_forward(app);
    let picked = dialog
        .blocking_save_file()
        .ok_or_else(|| CommandError::new(ErrorCode::Cancelled, "the save panel was cancelled"))?;
    picked.into_path().map_err(|e| CommandError::invalid(e.to_string()))
}

// --------------------------------------------------------------------------------- reporting

fn done(
    origin: &Origin,
    frame: &Frame,
    bytes: usize,
    path: Option<PathBuf>,
    format: &'static str,
    fallback_from: Option<String>,
) -> OutputDone {
    OutputDone {
        capture_id: origin.capture_id,
        width: frame.size.width,
        height: frame.size.height,
        bytes: bytes as u64,
        path: path.map(|p| p.to_string_lossy().into_owned()),
        format,
        fallback_from,
    }
}

/// Tells the pages, the toast and the history how the delivery ended.
fn report(
    app: &AppHandle,
    images: &[(Frame, Origin)],
    first: &Origin,
    result: CommandResult<Vec<Delivered>>,
) -> CommandResult<Vec<OutputDone>> {
    let delivered = match result {
        Ok(delivered) => delivered,
        Err(error) => {
            let _ = app.emit(
                events::OUTPUT_FAILED,
                OutputFailed { capture_id: first.capture_id, code: error.code, message: error.message.clone() },
            );
            if error.code != ErrorCode::Cancelled {
                trace::log(format!("[glimpsfy] output failed: {}", error.message));
                windows::toast_failure(app, first.display.as_ref().map(|d| d.id), &error);
            }
            return Err(error);
        }
    };

    let mut dones = Vec::with_capacity(delivered.len());
    for (delivered, (frame, origin)) in delivered.into_iter().zip(images) {
        let done = delivered.done;
        let saved = done.path.is_some();
        let _ = app.emit(if saved { events::OUTPUT_SAVED } else { events::OUTPUT_COPIED }, done.clone());

        let mut toast = Toast::new(if saved { ToastKind::Saved } else { ToastKind::Copied });
        toast.thumbnail = thumbnail(frame);
        toast.width = Some(done.width);
        toast.height = Some(done.height);
        toast.bytes = Some(done.bytes);
        toast.path = done.path.clone();
        toast.folder = done
            .path
            .as_ref()
            .and_then(|p| Path::new(p).parent()?.file_name().map(|n| n.to_string_lossy().into_owned()));
        toast.format = Some(done.format);
        toast.fallback_from = done.fallback_from.clone();
        windows::toast(app, origin.display.as_ref().map(|d| d.id), toast);

        let handoff = CaptureHandoff {
            kind: EntryKind::Screenshot,
            capture_id: origin.capture_id,
            mode: origin.mode,
            copied: delivered.copied,
            saved_path: done.path.as_ref().map(PathBuf::from),
            image_path: delivered.image_path,
            format: done.format,
            width: done.width,
            height: done.height,
            scale: frame.scale.0,
            bytes: done.bytes,
            color_space: frame.color.space,
            app_name: origin.app_name.clone(),
            bundle_id: origin.bundle_id.clone(),
            window_title: origin.window_title.clone(),
            display_id: origin.display.as_ref().map(|d| d.id.0),
            display_name: origin.display.as_ref().map(|d| d.name.clone()),
            created_at_ms: origin.taken_at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64),
        };
        let _ = app.emit(HANDOFF, handoff);
        dones.push(done);
    }
    Ok(dones)
}

/// The shutter plays once per capture, at its first delivery (a copy after an annotation counts
/// as that capture's), and never for the editor's images.
fn shutter_once(id: CaptureId) {
    static LAST: AtomicU64 = AtomicU64::new(0);
    if id.0 != 0 && LAST.swap(id.0, Ordering::SeqCst) != id.0 {
        platform::shutter();
    }
}

// -------------------------------------------------------------------------------- the inbox

/// The inbox folder, made when missing, with files older than a day deleted.
fn inbox(app: &AppHandle) -> Option<PathBuf> {
    let dir = crate::paths::data_dir(app).ok()?.join(INBOX);
    fs::create_dir_all(&dir).ok()?;
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > INBOX_KEEP);
            if old {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Some(dir)
}

fn write_inbox(dir: &Path, origin: &Origin, index: usize, png: &[u8]) -> Option<PathBuf> {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let path = dir.join(format!("capture-{}-{index}-{ms}.png", origin.capture_id.0));
    match fs::write(&path, png) {
        Ok(()) => Some(path),
        Err(error) => {
            trace::log(format!("[glimpsfy] history inbox {}: {error}", path.display()));
            None
        }
    }
}

// -------------------------------------------------------------------------------- thumbnail

/// A small PNG of the capture as a data URL, for the toast.
fn thumbnail(frame: &Frame) -> Option<String> {
    let (w, h) = (frame.size.width.max(1) as f64, frame.size.height.max(1) as f64);
    let scale = (THUMBNAIL.0 as f64 / w).min(THUMBNAIL.1 as f64 / h).min(1.0);
    let (tw, th) = (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1));
    let mut data = Vec::with_capacity((tw * th * 4) as usize);
    for ty in 0..th {
        let (y0, y1) =
            (ty * frame.size.height / th, ((ty + 1) * frame.size.height / th).max(ty * frame.size.height / th + 1));
        for tx in 0..tw {
            let (x0, x1) =
                (tx * frame.size.width / tw, ((tx + 1) * frame.size.width / tw).max(tx * frame.size.width / tw + 1));
            // Up to 4 × 4 samples of the block are enough for a thumbnail.
            let (sx, sy) = (((x1 - x0) / 4).max(1), ((y1 - y0) / 4).max(1));
            let (mut sum, mut n) = ([0u32; 4], 0u32);
            for y in (y0..y1).step_by(sy as usize) {
                for x in (x0..x1).step_by(sx as usize) {
                    if let Some(px) = frame.rgba_at(x, y) {
                        for (s, v) in sum.iter_mut().zip(px) {
                            *s += v as u32;
                        }
                        n += 1;
                    }
                }
            }
            data.extend(sum.map(|s| (s / n.max(1)) as u8));
        }
    }
    let color = ColorInfo { space: frame.color.space, icc_profile: frame.color.icc_profile.clone() };
    let small = Frame::new(
        glimpsfy_capture::PhysicalSize::new(tw, th),
        (tw * 4) as usize,
        PixelFormat::Rgba8,
        color,
        frame.scale,
        data,
    )
    .ok()?;
    let png = small.encode_png(PngEffort::Fast).ok()?;
    Some(format!("data:image/png;base64,{}", base64(&png)))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ------------------------------------------------------------------- the old entry points
// `commands/render.rs` (annotation) calls these with the flattened image of the overlay or the
// editor. They keep their first signatures and run `deliver`.

/// Puts a rendered image on the clipboard as PNG.
pub fn copy_frame(app: &AppHandle, origin: &Selection, frame: Frame) -> CommandResult<OutputDone> {
    deliver(app, frame, Origin::of_selection(app, origin), OutputAction::Copy)
}

/// Saves a rendered image as a new file: in the folder, format and name template of
/// Settings › Output, or, for Save as, under the name the user picked in `target`. Either way an
/// existing file is never replaced; a taken name gets " (2)".
pub fn save_frame(
    app: &AppHandle,
    origin: &Selection,
    frame: Frame,
    front_app: Option<&str>,
    target: Option<&Path>,
) -> CommandResult<OutputDone> {
    let mut origin = Origin::of_selection(app, origin);
    if let Some(front_app) = front_app {
        origin.app_name = Some(front_app.to_string());
    }
    let action = match target {
        Some(target) => OutputAction::SaveAs(Some(target.to_path_buf())),
        None => OutputAction::Save,
    };
    deliver(app, frame, origin, action)
}
