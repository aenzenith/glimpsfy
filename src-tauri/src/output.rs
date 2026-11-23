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
//! - **Beautify.** An image whose origin carries a style (`output/beautify.rs`) gets its padding,
//!   background, corner, shadow and frame in [`finish`], after the scaling to 1×, so a copy, a
//!   save, a path for Send to app and an upload of it hold the same pixels.
//! - **Files.** A save never replaces a file (`output/naming.rs`). A save folder that is not
//!   there (an unplugged drive), read-only or full sends the file to the default folder
//!   (Pictures › Glimpsfy) instead, and the toast names the folder that failed.

pub mod beautify;
mod naming;
mod platform;

pub use platform::copy_text;

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use glimpsfy_annotate::BeautifyStyle;
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

pub use naming::{file_stem, file_stem_titled, save_named, save_unique, save_unique_titled, Naming};
pub(crate) use platform::bring_forward;

/// The toast's thumbnail fits in this many pixels (2× its 64 × 44 point slot).
const THUMBNAIL: (u32, u32) = (128, 88);
/// The folder in the app data directory where clipboard-only captures wait for the history.
const INBOX: &str = "history-inbox";
/// A file in the inbox older than this is deleted by the next sweep.
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
    /// The beautify style the image leaves with, or `None` for the image as it is. A window
    /// (`mode` is `Window`) gets only its padding and background.
    pub beautify: Option<BeautifyStyle>,
    /// The format of a Save chosen for this image alone (the beautify panel's Save as PNG this
    /// time), over the one of the settings.
    pub format: Option<ImageFormat>,
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
            beautify: None,
            format: None,
        }
    }
}

/// What a delivery tells besides `output://copied` and `output://saved`. Send to app copies
/// without the Copied toast, because its own toast follows the paste (`send.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// A Copied, Saved or failure toast.
    pub toast: bool,
    /// `output://handoff`, from which the history records the image.
    pub handoff: bool,
}

impl Report {
    pub const ALL: Report = Report { toast: true, handoff: true };
}

/// Delivers one image: the single entry point for pixels leaving the app (see the module
/// documentation). The capture of `origin` is closed first when it is still open.
pub fn deliver(app: &AppHandle, frame: Frame, origin: Origin, action: OutputAction) -> CommandResult<OutputDone> {
    deliver_reported(app, frame, origin, action, Report::ALL)
}

/// [`deliver`] with a choice of what is reported.
pub fn deliver_reported(
    app: &AppHandle,
    frame: Frame,
    origin: Origin,
    action: OutputAction,
    report: Report,
) -> CommandResult<OutputDone> {
    deliver_many(app, vec![(frame, origin)], action, report)?
        .into_iter()
        .next()
        .ok_or_else(|| CommandError::internal("no image was delivered"))
}

/// Delivers several images of one capture together (all displays): one clipboard write with an
/// item per image, or one file per image, and one toast and one hand-off per image. A file that
/// cannot be written costs only its own image, which gets a failure toast; the answer is an error
/// only when no image was delivered.
pub fn deliver_many(
    app: &AppHandle,
    images: Vec<(Frame, Origin)>,
    action: OutputAction,
    report_to: Report,
) -> CommandResult<Vec<OutputDone>> {
    deliver_all(app, images, action, report_to, None)
}

/// Copies the subjects lifted out of a selection (Copy subject, `commands/subject.rs`) as one
/// transparent PNG, as [`deliver`] copies any image, with the Subject copied toast that says how
/// many `subjects` it holds. A cut-out leaves without the capture's beautify style, which would
/// fill the transparency that makes it one.
pub fn deliver_subjects(app: &AppHandle, frame: Frame, origin: Origin, subjects: u32) -> CommandResult<OutputDone> {
    let origin = Origin { beautify: None, ..origin };
    deliver_all(app, vec![(frame, origin)], OutputAction::Copy, Report::ALL, Some(subjects))?
        .into_iter()
        .next()
        .ok_or_else(|| CommandError::internal("no image was delivered"))
}

fn deliver_all(
    app: &AppHandle,
    images: Vec<(Frame, Origin)>,
    action: OutputAction,
    report_to: Report,
    subjects: Option<u32>,
) -> CommandResult<Vec<OutputDone>> {
    let Some(first) = images.first().map(|(_, origin)| origin.clone()) else {
        return Ok(Vec::new());
    };
    let settings = app.state::<AppState>().settings.get();
    let reason = if action == OutputAction::Copy { CloseReason::Copied } else { CloseReason::Saved };
    let images = finish(app, images, reason);

    let result = match &action {
        OutputAction::Copy => copy_images(app, &settings.output, &images).map(|all| all.into_iter().map(Ok).collect()),
        OutputAction::Save => Ok(save_images(app, &settings.output, &images, None)),
        OutputAction::SaveAs(Some(path)) => Ok(save_images(app, &settings.output, &images, Some(path))),
        OutputAction::SaveAs(None) => {
            let (frame, origin) = &images[0];
            pick_path(app, &settings.output, frame, origin)
                .map(|path| save_images(app, &settings.output, &images, Some(&path)))
        }
    };
    report(app, &images, &first, result, report_to, subjects)
}

/// What every image goes through before it leaves, whichever way it leaves (a delivery, or a
/// path or a file for Send to app): its capture closes before anything slow, and before a save
/// panel the overlay would cover; the shutter plays; and a Retina image is scaled to 1× when the
/// settings say so. A path therefore shows the same pixels a copy of the image would.
pub(crate) fn finish(app: &AppHandle, images: Vec<(Frame, Origin)>, reason: CloseReason) -> Vec<(Frame, Origin)> {
    let settings = app.state::<AppState>().settings.get();
    for id in images.iter().map(|(_, o)| o.capture_id).filter(|id| id.0 != 0) {
        session::close(app, id, reason);
    }
    if let (true, Some((_, first))) = (settings.general.shutter_sound, images.first()) {
        shutter_once(first.capture_id);
    }
    images.into_iter().map(|(frame, origin)| (finish_pixels(frame, &origin, settings.output.retina), origin)).collect()
}

/// The pixels an image leaves with: scaled to 1× when the settings say so, then beautified when
/// its origin carries a style, so the style's points become pixels of the delivered image.
pub(crate) fn finish_pixels(frame: Frame, origin: &Origin, retina: RetinaScale) -> Frame {
    let frame = match retina {
        RetinaScale::ScaleTo1x => frame.at_1x().unwrap_or(frame),
        RetinaScale::Keep2x => frame,
    };
    match &origin.beautify {
        Some(style) => beautify::apply(&frame, style, origin.mode == CaptureMode::Window),
        None => frame,
    }
}

// ------------------------------------------------------------------------- selection flows

/// Copies the selection (⌘C, Return), or the picked window when the selection is still its.
pub fn copy(app: &AppHandle, selection: Selection) -> CommandResult<OutputDone> {
    let (frame, origin) = take(app, &selection)?;
    deliver(app, frame, origin, OutputAction::Copy)
}

/// Saves the selection as a new file (⌘S), in `format` instead of the settings' when it is set.
pub fn save(app: &AppHandle, selection: Selection, format: Option<ImageFormat>) -> CommandResult<OutputDone> {
    let (frame, origin) = take(app, &selection)?;
    deliver(app, frame, Origin { format, ..origin }, OutputAction::Save)
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
        // A beautified window's shadow lies over the style's background instead of the fill.
        let frame = match picked.fill {
            Some(fill) if origin.beautify.is_none() && !picked.frame.is_opaque() => picked.frame.over(fill),
            _ => (*picked.frame).clone(),
        };
        return Ok((frame, origin));
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
    /// The PNG of a copy that left no file behind (the history is off), kept a while in case the
    /// toast's Send asks for a file (`recent`).
    png: Option<Arc<Vec<u8>>>,
}

fn copy_images(app: &AppHandle, output: &Output, images: &[(Frame, Origin)]) -> CommandResult<Vec<Delivered>> {
    let pngs = images
        .iter()
        .map(|(frame, _)| frame.encode(ImageEncoding::Png(PngEffort::Fast)))
        .collect::<Result<Vec<_>, _>>()?;
    let frames: Vec<&Frame> = images.iter().map(|(frame, _)| frame).collect();
    platform::copy_pngs(app, &pngs, &frames)?;

    let inbox = (output.history_retention != HistoryRetention::Off).then(|| inbox_dir(app)).flatten();
    Ok(images
        .iter()
        .zip(pngs)
        .enumerate()
        .map(|(i, ((frame, origin), png))| {
            let image_path = inbox.as_deref().and_then(|dir| write_inbox(dir, origin, i, &png));
            Delivered {
                done: done(origin, frame, png.len(), None, "png", None),
                copied: true,
                png: image_path.is_none().then(|| Arc::new(png)),
                image_path,
            }
        })
        .collect())
}

/// One result per image: each is written on its own, because a file already on disk must still
/// be reported and handed to the history when a later one fails.
fn save_images(
    app: &AppHandle,
    output: &Output,
    images: &[(Frame, Origin)],
    target: Option<&Path>,
) -> Vec<CommandResult<Delivered>> {
    let mut results = Vec::with_capacity(images.len());
    // The PNGs for the clipboard, with their frames and the results they belong to.
    let (mut pngs, mut frames, mut copied) = (Vec::new(), Vec::new(), Vec::new());
    for (i, (frame, origin)) in images.iter().enumerate() {
        let format = target.and_then(format_of).or(origin.format).unwrap_or(output.format);
        let (bytes, path, fallback_from) = match write_image(output, frame, origin, i, format, target) {
            Ok(written) => written,
            Err(error) => {
                results.push(Err(error));
                continue;
            }
        };
        if output.copy_after_save {
            let png = if format == ImageFormat::Png {
                Ok(bytes.clone())
            } else {
                frame.encode(ImageEncoding::Png(PngEffort::Fast))
            };
            match png {
                Ok(png) => {
                    pngs.push(png);
                    frames.push(frame);
                    copied.push(results.len());
                }
                Err(error) => trace::log(format!("[glimpsfy] copy after save: {error}")),
            }
        }
        results.push(Ok(Delivered {
            done: done(origin, frame, bytes.len(), Some(path), format_name(format), fallback_from),
            copied: false,
            image_path: None,
            png: None,
        }));
    }
    if !pngs.is_empty() {
        match platform::copy_pngs(app, &pngs, &frames) {
            Ok(()) => {
                for index in copied {
                    if let Some(Ok(delivered)) = results.get_mut(index) {
                        delivered.copied = true;
                    }
                }
            }
            // The files are written; a clipboard that refused is worth a line, not a failure.
            Err(error) => trace::log(format!("[glimpsfy] copy after save: {}", error.message)),
        }
    }
    results
}

/// Encodes one image and writes its file: the bytes, the path and the folder that failed.
fn write_image(
    output: &Output,
    frame: &Frame,
    origin: &Origin,
    index: usize,
    format: ImageFormat,
    target: Option<&Path>,
) -> CommandResult<(Vec<u8>, PathBuf, Option<String>)> {
    let bytes = frame.encode(encoding(output, format))?;
    let (path, fallback_from) = match target {
        Some(path) => (write_picked(path, index, format, &bytes)?, None),
        None => write_in_folder(output, frame, origin, format, &bytes)?,
    };
    Ok((bytes, path, fallback_from))
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

// ------------------------------------------------------------------------- interval series

/// The folder a new interval series writes into: a new folder named `name` in the save folder of
/// Settings › Output, or in the default folder when that one cannot take it (unmounted, gone,
/// read-only). An existing folder is never reused (`naming::create_unique_dir`).
pub(crate) fn series_folder(output: &Output, name: &str) -> CommandResult<PathBuf> {
    let attempt = |parent: &Path| -> Result<PathBuf, String> {
        naming::check_folder(parent).map_err(|problem| problem.to_string())?;
        naming::create_unique_dir(parent, name).map_err(|error| error.to_string())
    };
    let chosen = &output.save_folder;
    match attempt(chosen) {
        Ok(folder) => Ok(folder),
        Err(first) => {
            let default = Output::default().save_folder;
            if &default == chosen {
                return Err(folder_error(chosen, &first));
            }
            attempt(&default).map_err(|_| folder_error(chosen, &first))
        }
    }
}

/// Writes one frame of an interval series into its folder: scaled to 1× when Settings › Output
/// says so, encoded in the format of Settings › Output, named `stem` and never replacing a file.
/// A series passes through none of `deliver`: no toast, no shutter, no clipboard. The folder is
/// not made again when it is gone (the series was deleted, which moved it to the Trash); the
/// write then fails with `NotFound`. Answers the file, its bytes and its pixel size.
pub(crate) fn write_series_frame(
    output: &Output,
    folder: &Path,
    stem: &str,
    frame: Frame,
) -> std::io::Result<(PathBuf, u64, glimpsfy_capture::PhysicalSize)> {
    if !folder.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "the series' folder is gone"));
    }
    let frame = match output.retina {
        RetinaScale::ScaleTo1x => frame.at_1x().unwrap_or(frame),
        RetinaScale::Keep2x => frame,
    };
    let bytes = frame.encode(encoding(output, output.format)).map_err(std::io::Error::other)?;
    let path = naming::save_named(folder, stem, output.format.extension(), &bytes)?;
    Ok((path, bytes.len() as u64, frame.size))
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
    let (folder, stem, extension) = split_picked(path, format_of(path).is_some(), format.extension())?;
    let stem = if index == 0 { stem } else { format!("{stem} {}", index + 1) };
    naming::save_named(folder, &stem, &extension, bytes).map_err(|error| folder_error(folder, &error.to_string()))
}

/// Copies the file `source` under the name the user picked (Save as of a history entry) by the
/// rule of [`write_picked`]: a taken name gets " (2)" even when the panel asked about replacing
/// it, so no file is ever replaced. Picking `source` itself is refused, since the panel opens on
/// a recording's own name and a copy onto itself would empty the file.
pub fn copy_picked(path: &Path, source: &Path) -> CommandResult<PathBuf> {
    let same = matches!((fs::canonicalize(path), fs::canonicalize(source)), (Ok(a), Ok(b)) if a == b);
    if same {
        return Err(CommandError::invalid(
            "The chosen file is this capture itself, so it was left as it was. Choose another name or folder.",
        ));
    }
    let own = source.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
    let keeps = path.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(&own));
    let (folder, stem, extension) = split_picked(path, keeps, &own)?;
    Ok(naming::copy_named(folder, &stem, &extension, source)?)
}

/// The folder, stem and extension of a path the save panel answered. An extension the caller
/// keeps stays as typed; any other stays part of the stem, and `fallback` is added.
fn split_picked<'a>(path: &'a Path, keep_extension: bool, fallback: &str) -> CommandResult<(&'a Path, String, String)> {
    let folder = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| CommandError::invalid("the path names no folder"))?;
    let text = |part: Option<&std::ffi::OsStr>| part.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(if keep_extension {
        let extension = path.extension().map(|e| e.to_string_lossy().into_owned());
        (folder, text(path.file_stem()), extension.unwrap_or_else(|| fallback.into()))
    } else {
        (folder, text(path.file_name()), fallback.into())
    })
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

/// Tells the pages, the toast and the history how the delivery ended: `result` fails as a whole
/// (the clipboard refused, the panel was cancelled) or holds one result per image.
fn report(
    app: &AppHandle,
    images: &[(Frame, Origin)],
    first: &Origin,
    result: CommandResult<Vec<CommandResult<Delivered>>>,
    report_to: Report,
    subjects: Option<u32>,
) -> CommandResult<Vec<OutputDone>> {
    let results = match result {
        Ok(results) => results,
        Err(error) => {
            report_failure(app, first, None, &error, report_to.toast);
            return Err(error);
        }
    };

    let mut dones = Vec::with_capacity(results.len());
    let mut failure = None;
    for (result, (frame, origin)) in results.into_iter().zip(images) {
        let delivered = match result {
            Ok(delivered) => delivered,
            Err(error) => {
                // Among several images, the toast shows the one that failed.
                report_failure(app, origin, (images.len() > 1).then_some(frame), &error, report_to.toast);
                failure.get_or_insert(error);
                continue;
            }
        };
        let done = delivered.done;
        let saved = done.path.is_some();
        let _ = app.emit(if saved { events::OUTPUT_SAVED } else { events::OUTPUT_COPIED }, done.clone());
        let small = thumbnail(frame);
        remember(Recent {
            capture_id: origin.capture_id,
            origin: origin.clone(),
            size: (done.width, done.height),
            saved_path: done.path.as_ref().map(PathBuf::from),
            png: delivered.png.clone(),
            file: None,
            thumbnail: small.clone(),
        });

        if report_to.toast {
            let kind = match (saved, subjects) {
                (true, _) => ToastKind::Saved,
                (false, Some(_)) => ToastKind::SubjectCopied,
                (false, None) => ToastKind::Copied,
            };
            let mut toast = Toast::new(kind);
            toast.subjects = subjects;
            toast.capture_id = (origin.capture_id.0 != 0).then_some(origin.capture_id);
            toast.thumbnail = small;
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
        }
        if !report_to.handoff {
            dones.push(done);
            continue;
        }

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
    match failure {
        Some(error) if dones.is_empty() => Err(error),
        _ => Ok(dones),
    }
}

/// `output://failed`, and with `toast` a failure toast unless the user cancelled.
fn report_failure(app: &AppHandle, origin: &Origin, frame: Option<&Frame>, error: &CommandError, toast: bool) {
    let _ = app.emit(
        events::OUTPUT_FAILED,
        OutputFailed { capture_id: origin.capture_id, code: error.code, message: error.message.clone() },
    );
    if error.code == ErrorCode::Cancelled || !toast {
        return;
    }
    trace::log(format!("[glimpsfy] output failed: {}", error.message));
    let mut toast = Toast::new(ToastKind::Failed);
    toast.error_code = Some(error.code);
    toast.message = Some(error.message.clone());
    toast.thumbnail = frame.and_then(thumbnail);
    windows::toast(app, origin.display.as_ref().map(|d| d.id), toast);
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

/// The inbox folder, made when missing. It holds clipboard-only captures until the history takes
/// them, and the files Send to app makes while the history is off; [`sweep_inbox`] empties it of
/// day-old files.
pub(crate) fn inbox_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = crate::paths::data_dir(app).ok()?.join(INBOX);
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Deletes the plain files in `dir` that were last written more than a day before `now`, and
/// answers what it deleted. A link is never followed nor deleted, and a folder is left alone, so
/// nothing outside the inbox can be reached through it. Runs at start and every hour
/// (`send.rs`, the `glimpsfy-send` thread), whether the history is on or not.
pub fn sweep_inbox(dir: &Path, now: SystemTime) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        // `DirEntry::metadata` does not traverse a link, so a link reads as a link here.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|written| now.duration_since(written).ok())
            .is_some_and(|age| age > INBOX_KEEP);
        if old && fs::remove_file(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    removed
}

/// Writes `png` into the inbox under the name the file name template gives the capture, for
/// Send to app while the history is off. A taken name gets " (2)".
pub(crate) fn write_inbox_named(
    app: &AppHandle,
    origin: &Origin,
    size: (u32, u32),
    png: &[u8],
) -> CommandResult<PathBuf> {
    let dir =
        inbox_dir(app).ok_or_else(|| CommandError::new(ErrorCode::Io, "the history inbox folder could not be made"))?;
    let template = app.state::<AppState>().settings.get().output.file_name_template;
    let (date, time) = Naming::clock(chrono::DateTime::<chrono::Local>::from(origin.taken_at));
    let naming = Naming { date, time, app: origin.app_name.as_deref(), width: size.0, height: size.1 };
    Ok(save_unique_titled(&dir, &template, &naming, origin.window_title.as_deref(), "png", png)?)
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

// ------------------------------------------------------------------------ recent deliveries

/// A delivered image, remembered for a while so the Send button of its toast, which knows only
/// the capture, can find a file for it (`send::resolve_file`).
#[derive(Clone)]
pub(crate) struct Recent {
    pub capture_id: CaptureId,
    pub origin: Origin,
    /// The delivered image's size in pixels, after any scaling to 1×.
    pub size: (u32, u32),
    pub saved_path: Option<PathBuf>,
    /// The PNG of a copy that left no file behind (the history is off).
    pub png: Option<Arc<Vec<u8>>>,
    /// The file Send to app found or wrote for it, so a second send names the same file.
    pub file: Option<PathBuf>,
    /// The toast's picture of it, for the toast of a send.
    pub thumbnail: Option<String>,
}

/// How many deliveries are remembered, and how many of them keep their PNG in memory.
const RECENT: usize = 8;
const RECENT_PNGS: usize = 3;
static RECENTS: Mutex<VecDeque<Recent>> = Mutex::new(VecDeque::new());

/// Remembers a delivery. The first image of a capture stands for it: a capture of all displays
/// has one toast per image but one capture id.
pub(crate) fn remember(recent: Recent) {
    if recent.capture_id.0 == 0 {
        return;
    }
    let mut list = RECENTS.lock().unwrap_or_else(|p| p.into_inner());
    if list.iter().any(|r| r.capture_id == recent.capture_id) {
        return;
    }
    list.push_front(recent);
    list.truncate(RECENT);
    for older in list.iter_mut().skip(RECENT_PNGS) {
        older.png = None;
    }
}

pub(crate) fn recent(capture_id: CaptureId) -> Option<Recent> {
    RECENTS.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|r| r.capture_id == capture_id).cloned()
}

/// Notes the file a send used for a remembered delivery.
pub(crate) fn remember_file(capture_id: CaptureId, file: &Path) {
    let mut list = RECENTS.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(recent) = list.iter_mut().find(|r| r.capture_id == capture_id) {
        recent.file = Some(file.to_path_buf());
    }
}

// -------------------------------------------------------------------------------- thumbnail

/// A small PNG of the capture as a data URL, for the toast.
pub(crate) fn thumbnail(frame: &Frame) -> Option<String> {
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
            // The colours are weighed by their alpha, so the colour a transparent pixel still
            // holds (the background a lifted subject left) does not tint the edge it shares with
            // an opaque one. For an opaque frame this is the plain average.
            let (mut sum, mut alpha, mut n) = ([0u32; 3], 0u32, 0u32);
            for y in (y0..y1).step_by(sy as usize) {
                for x in (x0..x1).step_by(sx as usize) {
                    if let Some(px) = frame.rgba_at(x, y) {
                        let a = px[3] as u32;
                        for (s, v) in sum.iter_mut().zip(px) {
                            *s += v as u32 * a;
                        }
                        alpha += a;
                        n += 1;
                    }
                }
            }
            let rgb = sum.map(|s| s.checked_div(alpha).unwrap_or(0) as u8);
            data.extend([rgb[0], rgb[1], rgb[2], (alpha / n.max(1)) as u8]);
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

pub(crate) fn base64(bytes: &[u8]) -> String {
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
// editor, and the origin it built with the image's beautify style; they run `deliver`.

/// Puts a rendered image on the clipboard as PNG.
pub fn copy_frame(app: &AppHandle, origin: Origin, frame: Frame) -> CommandResult<OutputDone> {
    deliver(app, frame, origin, OutputAction::Copy)
}

/// Saves a rendered image as a new file: in the folder, format and name template of
/// Settings › Output, or, for Save as, under the name the user picked in `target`. Either way an
/// existing file is never replaced; a taken name gets " (2)".
pub fn save_frame(
    app: &AppHandle,
    mut origin: Origin,
    frame: Frame,
    front_app: Option<&str>,
    target: Option<&Path>,
) -> CommandResult<OutputDone> {
    if let Some(front_app) = front_app {
        origin.app_name = Some(front_app.to_string());
    }
    let action = match target {
        Some(target) => OutputAction::SaveAs(Some(target.to_path_buf())),
        None => OutputAction::Save,
    };
    deliver(app, frame, origin, action)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inbox sweep deletes the inbox's own files once they are a day old and nothing else: a
    /// file written an hour ago stays, a link in the inbox stays and so does the history's file it
    /// points to, and a folder in the inbox is not entered. A sweep that followed a link or went
    /// past the inbox would delete the user's history.
    #[cfg(unix)]
    #[test]
    fn the_inbox_sweep_deletes_only_its_own_old_files() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("history-inbox");
        let captures = root.path().join("history").join("captures");
        fs::create_dir_all(inbox.join("folder")).unwrap();
        fs::create_dir_all(&captures).unwrap();
        let now = SystemTime::now();
        let aged = |path: &Path, hours: u64| {
            fs::write(path, b"png").unwrap();
            let file = fs::File::options().write(true).open(path).unwrap();
            file.set_modified(now - Duration::from_secs(hours * 60 * 60)).unwrap();
        };
        let old = inbox.join("old.png");
        let fresh = inbox.join("fresh.png");
        let kept = captures.join("entry.png");
        let nested = inbox.join("folder").join("old.png");
        aged(&old, 25);
        aged(&fresh, 1);
        aged(&kept, 25);
        aged(&nested, 25);
        let link = inbox.join("link.png");
        std::os::unix::fs::symlink(&kept, &link).unwrap();
        let away = inbox.join("away");
        std::os::unix::fs::symlink(&captures, &away).unwrap();

        let removed = sweep_inbox(&inbox, now);

        assert_eq!(removed, vec![old.clone()]);
        assert!(!old.exists());
        for stays in [&fresh, &kept, &nested] {
            assert!(stays.exists(), "{} was deleted", stays.display());
        }
        for link in [&link, &away] {
            assert!(fs::symlink_metadata(link).is_ok(), "{} was deleted", link.display());
        }
    }

    /// Save as of a history entry never loses a file. The panel opens on a recording's own name,
    /// so picking the capture itself (and confirming Replace) is refused and leaves it whole,
    /// whichever way the path reaches it; picking another file that exists keeps that file and
    /// writes the copy beside it as " (2)".
    #[test]
    fn copying_a_capture_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Recording 2026-11-18.mov");
        fs::write(&source, b"the recording").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        for picked in [source.clone(), dir.path().join("sub").join("..").join("Recording 2026-11-18.mov")] {
            let refused = copy_picked(&picked, &source).unwrap_err();
            assert_eq!(refused.code, ErrorCode::InvalidArgument, "{}", picked.display());
            assert_eq!(fs::read(&source).unwrap(), b"the recording", "{}", picked.display());
        }

        // The same file under another case, on a volume that ignores case.
        let _ = copy_picked(&dir.path().join("RECORDING 2026-11-18.MOV"), &source);
        assert_eq!(fs::read(&source).unwrap(), b"the recording");

        let other = dir.path().join("Clip.mov");
        fs::write(&other, b"mine").unwrap();
        let saved = copy_picked(&other, &source).unwrap();
        assert_eq!(saved.file_name().unwrap().to_string_lossy(), "Clip (2).mov");
        assert_eq!(fs::read(&other).unwrap(), b"mine");
        assert_eq!(fs::read(&saved).unwrap(), b"the recording");
    }
}
