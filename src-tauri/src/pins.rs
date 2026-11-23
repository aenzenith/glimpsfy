//! Pinned captures: one borderless, always-on-top window per pin (`pin-<id>`) that shows an
//! image from a capture, a history entry or a file. Owner: app shell (milestone 2).
//!
//! The image is copied into the cache folder when the pin opens, so the pin keeps showing it when
//! the history entry or the file goes away, and the page fetches it as bytes (`pin_image`), never
//! as base64. Pins live until they are closed and do not survive a restart: a pin is a
//! reference for the task at hand, the history already keeps every capture for good, and pins
//! that came back on another day, possibly on a display that is gone, would cover apps the user
//! no longer works in. The cache folder is emptied at every launch.
//!
//! A locked pin lets every click through to the app below. Holding ⌥ takes the pointer back for
//! as long as the key is down, so the pin can be unlocked without a menu.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use glimpsfy_capture::{selection_to_frame, LogicalRect, PngEffort};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::capture::Selection;
use crate::commands::error::{CommandError, CommandResult};
use crate::AppState;

pub const PREFIX: &str = "pin-";

/// The transparent margin around the image, in points: room for the float shadow, which fades
/// with the image while the controls stay solid, so the page draws it instead of the window.
pub const MARGIN: f64 = 24.0;

/// Where a pin's image comes from.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PinSource {
    /// A selection of the open capture. The overlay closes the capture itself afterwards.
    Capture {
        selection: Selection,
    },
    History {
        entry_id: glimpsfy_history::EntryId,
    },
    File {
        path: PathBuf,
        /// Pixels per point of the image, when the caller knows it (the editor). Without it the
        /// pin takes the scale of the screen under the pointer.
        #[serde(default)]
        scale: Option<f64>,
    },
    /// A capture with drawings, which the overlay rendered and staged as a PNG
    /// (`output_stage_rendered`): the pin shows the file where `selection` was captured, at the
    /// selection's size. Call it before the capture closes; once it has, the pin opens centred.
    Rendered {
        path: PathBuf,
        selection: Selection,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageType {
    Png,
    Jpeg,
}

impl ImageType {
    pub fn extension(self) -> &'static str {
        match self {
            ImageType::Png => "png",
            ImageType::Jpeg => "jpg",
        }
    }
}

/// What a pin page needs to draw itself; `pin_info` returns it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinInfo {
    pub id: u64,
    /// A name for the pin: the capture's or the file's.
    pub name: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Pixels per point of the image: the image shows at its own size at zoom 1.
    pub scale: f64,
    /// The image's place on the desktop when the pin opened, in global points from the top-left
    /// of the primary display.
    pub image: LogicalRect,
    /// The transparent margin around the image (`MARGIN`).
    pub margin: f64,
    pub image_type: ImageType,
    /// The history entry the pin shows, which the editor can open; `None` for a capture or a file.
    pub entry_id: Option<glimpsfy_history::EntryId>,
    /// Whether the pin shows on every Space (it does when it opens).
    pub all_spaces: bool,
}

struct Pin {
    info: PinInfo,
    file: PathBuf,
    locked: bool,
}

/// The open pins by id.
static PINS: Mutex<BTreeMap<u64, Pin>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

fn pins() -> std::sync::MutexGuard<'static, BTreeMap<u64, Pin>> {
    PINS.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn label(id: u64) -> String {
    format!("{PREFIX}{id}")
}

pub fn id_of(window: &WebviewWindow) -> CommandResult<u64> {
    window
        .label()
        .strip_prefix(PREFIX)
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| CommandError::invalid("this command works only in a pin window"))
}

fn cache_dir(app: &AppHandle) -> CommandResult<PathBuf> {
    Ok(crate::paths::cache_dir(app).map_err(|e| CommandError::internal(e.to_string()))?.join("pins"))
}

/// Empties the pin cache: pins never outlive the run that opened them. Once, at start.
pub fn clear_cache(app: &AppHandle) {
    if let Ok(dir) = cache_dir(app) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Opens a pin and returns its id. Blocking work (encoding, reading files) runs on the caller's
/// thread, which is an async command's worker.
pub async fn open(app: &AppHandle, source: PinSource) -> CommandResult<PinInfo> {
    let entry_id = match &source {
        PinSource::History { entry_id } => Some(entry_id.clone()),
        _ => None,
    };
    let (bytes, name, anchor, scale) = match source {
        PinSource::Capture { selection } => capture_image(app, &selection)?,
        PinSource::History { entry_id } => {
            let entry = crate::commands::history::history_get(app.clone(), entry_id).await?;
            // A saved capture has no copy of its own in the history, only the user's file.
            let file = entry
                .media_path()
                .filter(|p| p.is_file())
                .ok_or_else(|| CommandError::not_found("the image of this history entry"))?;
            let bytes = std::fs::read(file)?;
            (bytes, entry.name.clone(), None, entry.scale.unwrap_or(0.0))
        }
        PinSource::File { path, scale } => {
            let bytes = std::fs::read(&path)?;
            let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (bytes, name, None, scale.filter(|s| s.is_finite()).unwrap_or(0.0))
        }
        PinSource::Rendered { path, selection } => {
            let bytes = std::fs::read(&path)?;
            let (name, anchor) = match captured_at(app, &selection) {
                Some((name, at)) => (name, Some(at)),
                None => (path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), None),
            };
            // No scale: the rendered pixels fill the selection they were drawn over.
            (bytes, name, anchor, 0.0)
        }
    };
    open_bytes(app, bytes, name, anchor, scale, entry_id)
}

/// Opens a pin of `bytes`, a PNG or a JPEG, under `name`: at `anchor` when the image was captured
/// there, else centred on the screen under the pointer. A `scale` of 0 takes the anchor's or that
/// screen's pixels per point. Blocking, like [`open`].
pub fn open_bytes(
    app: &AppHandle,
    bytes: Vec<u8>,
    name: String,
    anchor: Option<LogicalRect>,
    scale: f64,
    entry_id: Option<glimpsfy_history::EntryId>,
) -> CommandResult<PinInfo> {
    let (image_type, pixel_width, pixel_height) = image_size(&bytes).ok_or_else(|| {
        CommandError::invalid(
            "Glimpsfy can pin PNG and JPEG images only, so open this file in Preview and export it as PNG",
        )
    })?;

    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let dir = cache_dir(app)?;
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{id}.{}", image_type.extension()));
    std::fs::write(&file, &bytes)?;

    let screen = screen_at(app, anchor.map(|r| (r.x, r.y)));
    let scale = match anchor {
        _ if scale > 0.0 => scale,
        Some(at) if at.width > 0.0 => f64::from(pixel_width) / at.width,
        _ => screen.scale,
    };
    let image = place(anchor, f64::from(pixel_width) / scale, f64::from(pixel_height) / scale, screen.visible);
    let info = PinInfo {
        id,
        name,
        pixel_width,
        pixel_height,
        scale,
        image,
        margin: MARGIN,
        image_type,
        entry_id,
        all_spaces: true,
    };
    pins().insert(id, Pin { info: info.clone(), file, locked: false });

    if let Err(error) = build(app, &info) {
        forget(app, id);
        return Err(error.into());
    }
    Ok(info)
}

/// The selection's pixels as a PNG, its place on the desktop and its scale.
fn capture_image(app: &AppHandle, selection: &Selection) -> CommandResult<(Vec<u8>, String, Option<LogicalRect>, f64)> {
    let state = app.state::<AppState>();
    let id = selection.capture_id;
    let session = state.sessions.get(id).ok_or_else(|| CommandError::not_found(format!("capture #{}", id.0)))?;
    let entry = session
        .display(selection.display_id)
        .ok_or_else(|| CommandError::not_found(format!("display {}", selection.display_id.0)))?;
    let rect = selection_to_frame(selection.rect, entry.display.frame.size(), entry.frame.size)
        .ok_or_else(|| CommandError::invalid("the selection is empty or outside its display"))?;
    let frame = entry.frame.crop(rect)?;
    let png = frame.encode_png(PngEffort::Fast)?;
    let (name, at) =
        captured_at(app, selection).ok_or_else(|| CommandError::not_found(format!("capture #{}", id.0)))?;
    Ok((png, name, Some(at), entry.display.scale.0))
}

/// Where a selection of an open capture lies on the desktop, and the name the capture would be
/// saved under, as a history entry carries it. An area has no window title, so the template's
/// `{title}` stays empty. `None` once the capture has closed.
fn captured_at(app: &AppHandle, selection: &Selection) -> Option<(String, LogicalRect)> {
    let state = app.state::<AppState>();
    let session = state.sessions.get(selection.capture_id)?;
    let entry = session.display(selection.display_id)?;
    let display = entry.display.frame;
    let at = LogicalRect {
        x: display.x + selection.rect.x,
        y: display.y + selection.rect.y,
        width: selection.rect.width,
        height: selection.rect.height,
    };
    let scale = entry.display.scale.0;
    let (date, time) = crate::output::Naming::clock(chrono::Local::now());
    let naming = crate::output::Naming {
        date,
        time,
        app: session.front_app.as_deref(),
        text: None,
        width: (at.width * scale).round() as u32,
        height: (at.height * scale).round() as u32,
    };
    let template = state.settings.get().output.file_name_template;
    Some((crate::output::file_stem(&template, &naming, 1), at))
}

/// The pin's own copy of its image, for the editor.
pub fn file(id: u64) -> CommandResult<PathBuf> {
    pins().get(&id).map(|pin| pin.file.clone()).ok_or_else(|| CommandError::not_found(format!("pin #{id}")))
}

/// The image's first place: where it was captured, or centred on the screen under the pointer and
/// shrunk to fit four fifths of it.
fn place(anchor: Option<LogicalRect>, width: f64, height: f64, visible: LogicalRect) -> LogicalRect {
    if let Some(at) = anchor {
        return LogicalRect { width, height, ..at };
    }
    let fit = (visible.width * 0.8 / width).min(visible.height * 0.8 / height).min(1.0);
    let (width, height) = ((width * fit).round(), (height * fit).round());
    // Each new pin lands a little lower and to the right of the last, so none hides another.
    let step = (pins().len() % 8) as f64 * 24.0;
    LogicalRect {
        x: (visible.x + (visible.width - width) / 2.0 + step).round(),
        y: (visible.y + (visible.height - height) / 2.0 + step).round(),
        width,
        height,
    }
}

struct ScreenArea {
    visible: LogicalRect,
    scale: f64,
}

/// The pixels per point of the screen under the pointer, for an image that carries no resolution
/// of its own (the clipboard's, `clipboard.rs`).
pub(crate) fn pointer_scale(app: &AppHandle) -> f64 {
    screen_at(app, None).scale
}

/// The visible area and scale of the screen that holds `point` (or the pointer), in global
/// top-left points.
fn screen_at(app: &AppHandle, point: Option<(f64, f64)>) -> ScreenArea {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::macos::on_main(app, move |mtm| {
            let _ = tx.send(crate::macos::screen_area(point, mtm));
        });
        if let Ok(Some((visible, scale))) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            return ScreenArea { visible, scale };
        }
    }
    #[cfg(not(target_os = "macos"))]
    if let Some((visible, scale)) = crate::desktop::screen_area(point) {
        return ScreenArea { visible, scale };
    }
    let _ = (app, point);
    ScreenArea { visible: LogicalRect { x: 0.0, y: 0.0, width: 1440.0, height: 900.0 }, scale: 2.0 }
}

/// The pin window: borderless, transparent, on top of other apps' windows and on every Space.
fn build(app: &AppHandle, info: &PinInfo) -> tauri::Result<()> {
    let frame = window_frame(&info.image);
    let build = || {
        WebviewWindowBuilder::new(app, label(info.id), WebviewUrl::App("index.html".into()))
            .title(format!("{} · Glimpsfy", info.name))
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .resizable(false)
            .skip_taskbar(true)
            .always_on_top(true)
            .visible_on_all_workspaces(true)
            .accept_first_mouse(true)
            .disable_drag_drop_handler()
            .inner_size(frame.width, frame.height)
            .position(frame.x, frame.y)
            .visible(false)
            .focused(false)
            .build()
    };
    #[cfg(target_os = "macos")]
    {
        let window = crate::macos::without_activating(build)?;
        crate::macos::on_main(app, move |mtm| crate::macos::setup_pin(&window, frame, mtm));
    }
    #[cfg(not(target_os = "macos"))]
    crate::desktop::setup_pin(&build()?, frame);
    Ok(())
}

/// The window around an image: the image and the margin on every side.
fn window_frame(image: &LogicalRect) -> LogicalRect {
    LogicalRect {
        x: image.x - MARGIN,
        y: image.y - MARGIN,
        width: image.width + 2.0 * MARGIN,
        height: image.height + 2.0 * MARGIN,
    }
}

pub fn info(id: u64) -> CommandResult<PinInfo> {
    pins().get(&id).map(|p| p.info.clone()).ok_or_else(|| CommandError::not_found(format!("pin #{id}")))
}

pub fn image(id: u64) -> CommandResult<(Vec<u8>, ImageType)> {
    let (file, kind) = {
        let pins = pins();
        let pin = pins.get(&id).ok_or_else(|| CommandError::not_found(format!("pin #{id}")))?;
        (pin.file.clone(), pin.info.image_type)
    };
    Ok((std::fs::read(file)?, kind))
}

/// Moves and sizes a pin's window (global top-left points), as the page drags a corner, zooms
/// or collapses the pin.
pub fn set_frame(app: &AppHandle, id: u64, frame: LogicalRect) -> CommandResult<()> {
    let window = window(app, id)?;
    if !(frame.width >= 1.0 && frame.height >= 1.0 && frame.width.is_finite() && frame.height.is_finite()) {
        return Err(CommandError::invalid("a pin needs a size of at least one point"));
    }
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::set_frame(&window, frame, mtm));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        crate::desktop::set_frame(&window, frame);
    }
    Ok(())
}

/// The window's frame now, in global top-left points.
pub fn frame(app: &AppHandle, id: u64) -> CommandResult<LogicalRect> {
    let window = window(app, id)?;
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let read = window.clone();
        crate::macos::on_main(app, move |mtm| {
            let _ = tx.send(crate::macos::frame_of(&read, mtm));
        });
        if let Ok(Some(frame)) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            return Ok(frame);
        }
    }
    // Tao's own answer divides by the window's scale alone, which is not where a pin sits when
    // Windows displays of different scales meet; a pin that cannot be read is better than one
    // moved to the wrong place.
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        crate::desktop::frame_of(&window).ok_or_else(|| CommandError::internal("the pin's frame could not be read"))
    }
    #[cfg(target_os = "macos")]
    {
        let scale = window.scale_factor()?;
        let position = window.outer_position()?.to_logical::<f64>(scale);
        let size = window.outer_size()?.to_logical::<f64>(scale);
        Ok(LogicalRect { x: position.x, y: position.y, width: size.width, height: size.height })
    }
}

/// Locks or unlocks a pin. A locked pin lets clicks through until ⌥ is held.
pub fn set_locked(app: &AppHandle, id: u64, locked: bool) -> CommandResult<()> {
    let window = window(app, id)?;
    if let Some(pin) = pins().get_mut(&id) {
        pin.locked = locked;
    }
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::set_click_through(&window, locked, mtm));
    #[cfg(not(target_os = "macos"))]
    crate::desktop::set_click_through(&window, locked);
    if locked {
        watch_option_key(app);
    }
    Ok(())
}

/// Shows or hides a pin on every Space.
pub fn set_all_spaces(app: &AppHandle, id: u64, all: bool) -> CommandResult<()> {
    window(app, id)?.set_visible_on_all_workspaces(all)?;
    if let Some(pin) = pins().get_mut(&id) {
        pin.info.all_spaces = all;
    }
    Ok(())
}

pub fn copy(id: u64) -> CommandResult<()> {
    let (bytes, kind) = image(id)?;
    #[cfg(target_os = "macos")]
    return crate::macos::copy_image(&bytes, kind == ImageType::Jpeg).map_err(CommandError::internal);
    #[cfg(not(target_os = "macos"))]
    crate::desktop::copy_image(&bytes, kind == ImageType::Jpeg).map_err(CommandError::internal)
}

/// Saves the pin's image into the save folder under the file name template, never over a file.
pub fn save(app: &AppHandle, id: u64) -> CommandResult<PathBuf> {
    let (bytes, kind) = image(id)?;
    let info = info(id)?;
    let output = app.state::<AppState>().settings.get().output;
    // The pin's own copy is a quick PNG (or the file it opened); the saved file is made smaller
    // the way a capture's is. A JPEG is written as it is.
    let bytes = if kind == ImageType::Png && output.optimise_png {
        glimpsfy_capture::encode::optimise_png(bytes, None)
    } else {
        bytes
    };
    let (date, time) = crate::output::Naming::clock(chrono::Local::now());
    let naming =
        crate::output::Naming { date, time, app: None, text: None, width: info.pixel_width, height: info.pixel_height };
    let path =
        crate::output::save_unique(&output.save_folder, &output.file_name_template, &naming, kind.extension(), &bytes)?;
    let mut toast = crate::windows::Toast::new(crate::windows::ToastKind::Saved);
    toast.format = Some(match kind {
        ImageType::Png => "png",
        ImageType::Jpeg => "jpeg",
    });
    toast.width = Some(info.pixel_width);
    toast.height = Some(info.pixel_height);
    toast.bytes = Some(bytes.len() as u64);
    toast.folder = Path::new(&path).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
    toast.path = Some(path.to_string_lossy().into_owned());
    crate::windows::toast(app, None, toast);
    Ok(path)
}

pub fn close(app: &AppHandle, id: u64) {
    if let Some(window) = app.get_webview_window(&label(id)) {
        #[cfg(target_os = "macos")]
        crate::macos::destroy_panel(app, window);
        #[cfg(not(target_os = "macos"))]
        crate::desktop::destroy_panel(window);
    }
    forget(app, id);
}

pub fn close_all(app: &AppHandle) {
    let ids: Vec<u64> = pins().keys().copied().collect();
    for id in ids {
        close(app, id);
    }
}

fn forget(app: &AppHandle, id: u64) {
    if let Some(pin) = pins().remove(&id) {
        let _ = std::fs::remove_file(pin.file);
    }
    let _ = app;
}

fn window(app: &AppHandle, id: u64) -> CommandResult<WebviewWindow> {
    app.get_webview_window(&label(id)).ok_or_else(|| CommandError::not_found(format!("pin #{id}")))
}

/// To a pin window: ⌥ went down or up while the pin is locked. Payload: `{ held: bool }`.
pub const OPTION_KEY: &str = "pin://option-key";

static WATCHING: AtomicBool = AtomicBool::new(false);

/// While any pin is locked, watches ⌥: held, the locked pins take the pointer and show their
/// unlock button; released, clicks go through them again. One thread, which ends when no pin is
/// locked.
fn watch_option_key(app: &AppHandle) {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let mut held = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(60));
            let locked: Vec<u64> = pins().iter().filter(|(_, p)| p.locked).map(|(id, _)| *id).collect();
            if locked.is_empty() {
                WATCHING.store(false, Ordering::SeqCst);
                return;
            }
            #[cfg(target_os = "macos")]
            let now = crate::macos::option_key_down();
            #[cfg(not(target_os = "macos"))]
            let now = crate::desktop::option_key_down();
            if now == held {
                continue;
            }
            held = now;
            for id in locked {
                if let Some(window) = app.get_webview_window(&label(id)) {
                    let _ = window.emit(OPTION_KEY, serde_json::json!({ "held": held }));
                    #[cfg(target_os = "macos")]
                    crate::macos::on_main(&app, move |mtm| crate::macos::set_click_through(&window, !held, mtm));
                    #[cfg(not(target_os = "macos"))]
                    crate::desktop::set_click_through(&window, !held);
                }
            }
        }
    });
}

/// The kind and pixel size of a PNG or JPEG, read from its header.
pub fn image_size(bytes: &[u8]) -> Option<(ImageType, u32, u32)> {
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR" {
        let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return (width > 0 && height > 0).then_some((ImageType::Png, width, height));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        // Walk the segments to the first start-of-frame marker, which holds the size.
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xFF {
                return None;
            }
            let marker = bytes[at + 1];
            let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
            let is_frame = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
            if is_frame {
                let height = u32::from(u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]));
                let width = u32::from(u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]));
                return (width > 0 && height > 0).then_some((ImageType::Jpeg, width, height));
            }
            at += 2 + length;
        }
    }
    None
}
