//! Capture commands: start a capture, describe displays and windows, and drive the overlay.
//! Owner: capture and overlay (milestone 1). The flow behind them is `session.rs`.

use std::time::Instant;

use glimpsfy_annotate::BeautifyStyle;
use glimpsfy_capture::{
    ColorSpace, Display, DisplayId, LogicalPoint, LogicalRect, PhysicalSize, WindowId, WindowInfo, WindowListOptions,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use super::error::{CommandError, CommandResult};
use crate::frames::FrameFormat;
use crate::{session, AppState};

/// One frozen capture: every display's frame taken at one moment, alive until the overlay closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CaptureId(pub u64);

/// What the user selected in the overlay: a rectangle of one display's frozen frame. The rect is
/// display-local and logical (points from the display's top-left corner); the core maps it to
/// the frame's pixels (`glimpsfy_capture::selection_to_frame`). Output, OCR and upload commands
/// all take it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub capture_id: CaptureId,
    pub display_id: DisplayId,
    pub rect: LogicalRect,
}

/// The capture modes of the menu and the shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureMode {
    /// Select an area (or a window, with Space) in the overlay.
    Area,
    /// Pick a window in the overlay: hover highlights it, a click captures it on its own.
    Window,
    /// The display under the pointer, at once, without the overlay.
    Display,
    /// Every display at once, one image per display, without the overlay.
    AllDisplays,
    /// The last area again on the same display, without the overlay; an area capture when there
    /// is no last area or its display is gone.
    RepeatLastArea,
    /// An area capture after the delay of Settings › General.
    AreaDelayed,
    /// Select an area in the overlay and copy the text in it (`ocr_copy_text`).
    CopyText,
    /// Measure the frozen screen in the overlay (the ruler); it delivers nothing and records
    /// nothing in the history.
    Ruler,
    /// The magnifier alone over the frozen screen, without the dim: a click copies the colour
    /// under the pointer (`color_copy`), and the capture closes as cancelled, so nothing reaches
    /// the history.
    PickColor,
    /// Select an area in the overlay and go straight into the scrolling capture (`scroll.rs`),
    /// as ⌘↓ does from the action bar. macOS only (`scroll_capture`).
    ScrollCapture,
}

impl CaptureMode {
    /// Whether the mode opens the overlay.
    pub fn uses_overlay(self) -> bool {
        matches!(
            self,
            CaptureMode::Area
                | CaptureMode::Window
                | CaptureMode::AreaDelayed
                | CaptureMode::CopyText
                | CaptureMode::Ruler
                | CaptureMode::PickColor
                | CaptureMode::ScrollCapture
        )
    }
}

/// What the overlay does once a selection is made (Settings › General › After a selection, or
/// the mode's own action).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AfterSelection {
    /// Stays open with the tool bars, for annotation.
    Annotate,
    /// Copies the image and closes.
    Copy,
    /// Saves the image and closes.
    Save,
    /// Hands the selection to `ocr_copy_text` (the copy-text mode).
    CopyText,
    /// Hands the selection to `capture_start_scroll` (the scrolling capture mode).
    Scroll,
}

/// To one overlay window with `capture://started`: what it shows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayFrame {
    pub capture_id: CaptureId,
    pub display: Display,
    /// Where to fetch the frozen frame (`frames.rs`).
    pub frame_url: String,
    pub frame_size: PhysicalSize,
    pub format: FrameFormat,
    /// The frame's colour space; the canvas and the `ImageData` use the same one.
    pub color_space: ColorSpace,
    /// Windows on this display for snapping, frontmost first, display-local and logical.
    pub windows: Vec<OverlayWindow>,
    /// The pointer, display-local, when it is on this display; the crosshair starts there.
    pub pointer: Option<LogicalPoint>,
    /// The mode the capture started in.
    pub mode: CaptureMode,
    /// What the page does when the user finishes a selection or clicks a window.
    pub after_selection: AfterSelection,
    /// The beautify style the capture's images leave with (the automatic preset of Settings ›
    /// Beautify, or what the page set since), or `None` for the images as they are.
    pub beautify: Option<BeautifyStyle>,
}

/// A window the overlay can snap to.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayWindow {
    pub id: WindowId,
    pub app_name: String,
    pub title: Option<String>,
    /// Display-local, logical; it may reach past the display's edges.
    pub rect: LogicalRect,
    pub corner_radius: Option<f64>,
    /// The window keeps itself out of captures, so the frame shows what lies behind it; the
    /// ruler hatches it and measures nothing there, and the colour picker reads nothing there.
    pub capture_blocked: bool,
}

/// Why a capture ended, with `capture://closed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseReason {
    Cancelled,
    Copied,
    Saved,
    Failed,
    /// The selection left as an upload; the link, not the image, reaches the clipboard.
    Uploaded,
    /// The capture went on in the editor (⌘O), which records it in the history when it leaves.
    Editor,
    /// The selection became the area of a recording's setup (⌘R).
    Recording,
    /// The selection was pinned (⌘P).
    Pinned,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureClosed {
    pub capture_id: CaptureId,
    pub reason: CloseReason,
}

/// The result of `capture_pick_window`: the window's own image is ready, and Copy or Save on a
/// selection that still covers the window delivers it instead of the crop.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedWindow {
    pub window_id: WindowId,
    /// The window image's size in pixels, shadow included when it is on.
    pub width: u32,
    pub height: u32,
}

/// The state of the delayed capture, for the popover and the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DelayState {
    /// The one-shot delay the next capture waits for (0 is off); it turns itself off once used.
    pub pending_seconds: u32,
    /// The seconds left of a countdown that is running now, if one is.
    pub remaining_seconds: Option<u32>,
    /// The mode that starts when the countdown ends.
    pub mode: Option<CaptureMode>,
}

/// Event names of the capture area and their payloads.
pub mod events {
    /// A delayed capture's countdown started, ticked or ended (cancelled or captured), to every
    /// window. Payload: [`super::DelayState`].
    pub const COUNTDOWN: &str = "capture://countdown";
    /// A scrolling capture's result grew, moved, lost its last movement or stopped scrolling by
    /// itself, to the overlay. Payload: [`super::ScrollProgress`].
    pub const SCROLL: &str = "capture://scroll";
}

/// Which way a scrolling capture's gap lies from what the selection shows now: the user scrolls
/// that way to close it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrollGap {
    Above,
    Below,
}

/// The state of a scrolling capture, with `capture://scroll` and from every `capture_*_scroll`
/// command. Sizes are physical pixels of the result.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollProgress {
    pub capture_id: CaptureId,
    pub display_id: DisplayId,
    pub width: u32,
    pub height: u32,
    /// Frames that brought rows in or moved the view.
    pub frames: u32,
    /// The sticky header and bottom bar, in rows; `found` once a second move showed them again.
    pub header: u32,
    pub header_found: bool,
    pub footer: u32,
    pub footer_found: bool,
    /// Where one piece of the result ends and the next begins, in rows.
    pub gaps: Vec<u32>,
    /// The part of the result the selection shows now: its first row and its height.
    pub view: Option<[u32; 2]>,
    pub gap: Option<ScrollGap>,
    /// The memory the compressed rows take.
    pub bytes: u64,
    /// The tallest result; the mode finishes by itself there.
    pub limit: u32,
    /// Glimpsfy scrolls the page itself.
    pub auto: bool,
    /// Automatic scrolling may run: macOS lets Glimpsfy post scroll events (Accessibility).
    pub auto_allowed: bool,
    /// Automatic scrolling stopped by itself, since the page no longer moved: its end.
    pub ended: bool,
    /// The strip's preview of the result (`frames.rs`), once there is one.
    pub preview_url: Option<String>,
}

/// The overlay page's own timings of its first frame, for the dev summary line.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PaintReport {
    /// From `capture://started` arriving to the response's last byte.
    pub fetch_ms: f64,
    /// Turning the bytes into an image (`ImageData` or `createImageBitmap`).
    pub decode_ms: f64,
    /// Putting it on the canvas.
    pub draw_ms: f64,
    /// From `overlay_ready` returning to the second animation frame.
    pub paint_ms: f64,
    pub bytes: u64,
}

/// Starts a capture in `mode`, after the one-shot delay when one is set. A call while a capture
/// is open or counting down does nothing (`session::start` says why).
#[tauri::command]
pub fn capture_start(app: AppHandle, mode: CaptureMode) -> CommandResult<()> {
    session::start(&app, mode, Instant::now())
}

/// The overlay page could not load or draw its frame. The core reloads the page once; a second
/// failure drops that display, or closes the capture with a failure toast when the display was
/// the one the user works on.
#[tauri::command]
pub fn overlay_failed(app: AppHandle, capture_id: CaptureId, display_id: DisplayId, message: String) {
    session::overlay_failed(&app, capture_id, display_id, &message);
}

/// Captures one window of the open capture on its own (real alpha, shadow and corners from the
/// settings), when the user clicks it in window mode or with Space. Copy and Save then deliver
/// this image while the selection still covers the window.
#[tauri::command]
pub async fn capture_pick_window(
    app: AppHandle,
    capture_id: CaptureId,
    display_id: DisplayId,
    window_id: WindowId,
) -> CommandResult<PickedWindow> {
    tauri::async_runtime::spawn_blocking(move || session::pick_window(&app, capture_id, display_id, window_id))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
}

/// Sets the one-shot delay of the next capture: 0 (off), 3, 5 or 10 seconds.
#[tauri::command]
pub fn capture_set_delay(app: AppHandle, seconds: u32) -> CommandResult<DelayState> {
    session::delay::set_pending(&app, seconds)
}

/// Sets the beautify style the images of capture `capture_id` leave with, or `None` to leave
/// them as they are (F, the panel). It belongs to this capture alone; Settings › Beautify keeps
/// its presets. A style that is not valid (`invalid_argument`) or a capture that is gone
/// (`not_found`) changes nothing.
#[tauri::command]
pub fn capture_set_beautify(
    state: State<'_, AppState>,
    capture_id: CaptureId,
    style: Option<BeautifyStyle>,
) -> CommandResult<()> {
    if let Some(style) = &style {
        style.check().map_err(CommandError::invalid)?;
    }
    let session =
        state.sessions.get(capture_id).ok_or_else(|| CommandError::not_found(format!("capture #{}", capture_id.0)))?;
    session.set_beautify(style.map(|s| s.clamped()));
    Ok(())
}

/// The delay that is set and the countdown that is running, if any.
#[tauri::command]
pub fn capture_delay(app: AppHandle) -> DelayState {
    session::delay::state(&app)
}

/// Stops a running countdown without capturing (what Esc does while it runs).
#[tauri::command]
pub fn capture_cancel_delay(app: AppHandle) -> bool {
    session::delay::cancel(&app)
}

/// Turns the overlay's selection into a scrolling capture (`scroll.rs`): the selection streams
/// into the stitcher while the user scrolls the page under it, which the overlay lets the pointer
/// through to. Fails with `unsupported` off macOS and with `busy` while one runs.
#[tauri::command]
pub async fn capture_start_scroll(app: AppHandle, selection: Selection) -> CommandResult<ScrollProgress> {
    tauri::async_runtime::spawn_blocking(move || crate::scroll::start(&app, selection))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
}

/// Starts or stops automatic scrolling (Space). Starting fails with `permission_denied` while
/// Glimpsfy may not post scroll events (Accessibility); the user's own scrolling goes on.
#[tauri::command]
pub fn capture_set_autoscroll(app: AppHandle, on: bool) -> CommandResult<ScrollProgress> {
    crate::scroll::set_auto(&app, on)
}

/// Takes back the rows of the last movement (⌘Z).
#[tauri::command]
pub fn capture_undo_scroll(app: AppHandle) -> CommandResult<ScrollProgress> {
    crate::scroll::undo(&app)
}

/// Ends the scrolling capture with its result (↩, Done): the image opens in the editor, and the
/// capture closes as `editor`. Pieces a gap still parts follow each other end to end.
#[tauri::command]
pub async fn capture_finish_scroll(app: AppHandle) -> CommandResult<super::render::EditorImage> {
    tauri::async_runtime::spawn_blocking(move || crate::scroll::finish(&app))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
}

/// Drops the scrolling capture's result and goes back to the frozen selection (Esc); answers
/// whether one ran.
#[tauri::command]
pub fn capture_cancel_scroll(app: AppHandle) -> bool {
    crate::scroll::cancel(&app)
}

/// Tells the core which of the overlay's parts (window-local logical points) lie over the
/// scrolling capture's selection, where the pointer otherwise goes through to the app below.
#[tauri::command]
pub fn capture_cover_scroll(app: AppHandle, covers: Vec<LogicalRect>) {
    crate::scroll::cover(&app, covers);
}

/// The capture this overlay window belongs to, for a page that loaded (or reloaded) after
/// `capture://started` was sent.
#[tauri::command]
pub fn capture_current(state: State<'_, AppState>, window: tauri::WebviewWindow) -> Option<OverlayFrame> {
    let display = crate::windows::overlay_display(window.label())?;
    let session = state.sessions.current()?;
    session.overlay_frame(display)
}

/// Ends the capture: without output (Esc, Close), or because the page handed the selection on
/// itself (a pin, a recording's setup). The outputs the core makes give their own reasons, so a
/// page cannot claim a copy, a save or an upload that did not happen.
#[tauri::command]
pub fn capture_close(app: AppHandle, capture_id: CaptureId, reason: Option<CloseReason>) -> CommandResult<()> {
    let reason = reason.unwrap_or(CloseReason::Cancelled);
    if !matches!(reason, CloseReason::Cancelled | CloseReason::Pinned | CloseReason::Recording) {
        return Err(CommandError::invalid(format!("a page cannot close a capture as {reason:?}")));
    }
    session::close(&app, capture_id, reason);
    Ok(())
}

/// The overlay drew its frame while still invisible; this shows its window.
#[tauri::command]
pub fn overlay_ready(app: AppHandle, capture_id: CaptureId, display_id: DisplayId) -> CommandResult<()> {
    session::ready(&app, capture_id, display_id)
}

/// The overlay's first frame after `overlay_ready` reached the screen.
#[tauri::command]
pub fn overlay_painted(app: AppHandle, capture_id: CaptureId, display_id: DisplayId, report: PaintReport) {
    session::painted(&app, capture_id, display_id, report);
}

/// Every connected display.
#[tauri::command]
pub async fn capture_displays(state: State<'_, AppState>) -> CommandResult<Vec<Display>> {
    let backend = state.backend.clone();
    tauri::async_runtime::spawn_blocking(move || backend.displays())
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
        .map_err(Into::into)
}

/// Windows on screen, frontmost first.
#[tauri::command]
pub async fn capture_windows(state: State<'_, AppState>) -> CommandResult<Vec<WindowInfo>> {
    let backend = state.backend.clone();
    tauri::async_runtime::spawn_blocking(move || backend.windows(WindowListOptions::default()))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
        .map_err(Into::into)
}
