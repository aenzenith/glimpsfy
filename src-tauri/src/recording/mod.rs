//! Screen recording in the app: the setup surface and its windows, one take from the countdown
//! to the saved file (the state events, the ticks, the tray, the click and key overlays and the
//! webcam bubble), the hand-off to the history and the trim window. The recorder itself, its
//! states and the files are `glimpsfy_record`'s; this module puts them on screen.
//!
//! Windows (all made here, all non-activating panels except the trim window):
//!
//! - `rec-target` covers the setup's display: the setup, the countdown and the red frame. While a
//!   take runs it lets every click through. Never recorded.
//! - `rec-bar` holds the setup bar and the control bar; the page measures it and calls
//!   `record_bar_fit`. Never recorded.
//! - `rec-canvas` covers the recorded rectangle, click-through, and draws the click rings and the
//!   key caps. Recorded.
//! - the webcam bubble, a native panel with the camera's preview (`macos::recording`). Recorded.
//! - `rec-bubble-ctl` holds the bubble's hover controls. Never recorded.
//! - `recorder` is the trim window, an ordinary window.
//!
//! Threads: the commands' blocking calls (opening the stream, finishing the file, exports) run
//! on worker threads; one countdown thread per take ticks the seconds; one tick thread sends
//! `record://tick` four times a second while a take runs; one watch thread follows the pointer
//! for the bubble's drag and hover and the keystroke overlay while a setup or a take is open; one
//! thread at start removes the posters of earlier runs.

pub mod media;
pub mod setup;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use glimpsfy_capture::{LogicalPoint, LogicalRect, StreamTarget};
use glimpsfy_record::devices::Meter;
use glimpsfy_record::session::{Env, OwnWindows, Recorder, SystemClock, SystemWriters};
use glimpsfy_record::{
    Corner, RecordingConfig, RecordingId, RecordingOutcome, RecordingState, StopReason, WebcamShape,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, EventTarget, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::commands::record::events;
use crate::shortcuts::Action;
use crate::{trace, windows, AppState};

use self::media::Library;
use self::setup::{BarGlass, BubbleSize, RecordSetup, SetupDisplay, SetupRequest, SetupUpdate, Size, TargetMode};

pub const TARGET: &str = "rec-target";
pub const BAR: &str = "rec-bar";
pub const CANVAS: &str = "rec-canvas";
pub const BUBBLE_CONTROLS: &str = "rec-bubble-ctl";
/// Every panel of the recorder, which a capture's overlay never takes the keyboard from.
pub const PANELS: [&str; 4] = [TARGET, BAR, CANVAS, BUBBLE_CONTROLS];
/// The bubble's hover controls, laid over its lower edge.
const CONTROLS_SIZE: (f64, f64) = (96.0, 44.0);

/// The kit's menu bar glyph; while a take records, a red copy of it shows instead.
const TRAY_GLYPH: &[u8] = include_bytes!("../../icons/tray.png");

/// The recorded part of a take: where it is and what the overlays draw over it.
#[derive(Clone, Debug)]
struct Take {
    /// The recorder's take this is, so a start that failed late clears only its own.
    id: RecordingId,
    mode: TargetMode,
    display: SetupDisplay,
    /// The recorded rectangle, global.
    rect: LogicalRect,
    config: RecordingConfig,
    has_microphone: bool,
    has_system: bool,
    /// Whether the take came from the setup, which returns when the countdown is cancelled.
    from_setup: bool,
    /// Return was pressed while the stream still opened, when the recorder cannot start yet: the
    /// take starts the moment the stream is open instead of counting down.
    skip: bool,
}

/// The webcam bubble (`record://bubble`), in global points.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BubbleState {
    pub visible: bool,
    pub size: BubbleSize,
    pub corner: Corner,
    pub rect: LogicalRect,
    pub dragging: bool,
    pub near: Option<Corner>,
    pub slots: Vec<Slot>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Slot {
    pub corner: Corner,
    pub rect: LogicalRect,
}

/// What the canvas window draws (`record://canvas`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasConfig {
    pub mode: TargetMode,
    pub rect: LogicalRect,
    pub clicks: glimpsfy_record::ClickHighlight,
    pub keystrokes: glimpsfy_record::KeystrokeOverlay,
    pub lift_bottom: f64,
    pub drop_top: f64,
}

/// The bubble's hover controls ask for another size or the camera off.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BubbleChange {
    pub size: Option<BubbleSize>,
    #[serde(default)]
    pub off: bool,
}

#[derive(Default)]
struct Inner {
    setup: Option<RecordSetup>,
    take: Option<Take>,
    bubble: Option<BubbleState>,
    shape: WebcamShape,
    camera_device: Option<String>,
    canvas: Option<CanvasConfig>,
    /// The bar's last measured sizes and glass, to place it again when the target moves.
    bar: Option<(Size, Option<Size>, BarGlass)>,
    bar_shown: bool,
    /// The glass the bar window shows, in global points, empty while it is hidden: the window
    /// takes the mouse only over it and lets every other click through to what lies below.
    bar_hit: Vec<LogicalRect>,
    /// Whether the bar window takes the mouse now.
    bar_mouse: bool,
    meter: Option<Meter>,
}

/// The recording area's state, managed by Tauri.
pub struct RecordingApp {
    pub recorder: Arc<Recorder>,
    pub library: Library,
    inner: Mutex<Inner>,
    /// Bumped whenever the meters change, so an old meter thread stops.
    meter_generation: AtomicU64,
    /// Whether the watch thread runs.
    watching: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn state(app: &AppHandle) -> &RecordingApp {
    app.state::<RecordingApp>().inner()
}

/// Makes the recording state. Once, at start.
pub fn setup(app: &AppHandle) {
    let handle = app.clone();
    let on_fault = Arc::new(move |id: &RecordingId| {
        let app = handle.clone();
        let id = id.clone();
        // The fault arrives on a capture thread; finishing the file blocks, so it gets its own.
        std::thread::spawn(move || {
            trace::log(format!("[glimpsfy] recording {} stopped on its own", id.0));
            if let Some(outcome) = state(&app).recorder.finish_fault() {
                finished(&app, outcome);
            }
        });
    });
    let backend = app.state::<AppState>().backend.clone();
    let env = Env {
        streams: Arc::new(backend),
        writers: Arc::new(SystemWriters),
        clock: Arc::new(SystemClock),
        trash: Arc::new(glimpsfy_record::files::SystemTrash),
        on_fault,
    };
    app.manage(RecordingApp {
        recorder: Arc::new(Recorder::new(env)),
        library: Library::default(),
        inner: Mutex::new(Inner::default()),
        meter_generation: AtomicU64::new(0),
        watching: AtomicBool::new(false),
    });
    // A poster is read only in the run that wrote it, so an earlier run's posters serve no one.
    if let Some(dir) = poster_dir(app) {
        let started = std::time::SystemTime::now();
        std::thread::spawn(move || prune_posters(&dir, started));
    }
}

/// Runs `f` on the main thread and waits for its answer (up to two seconds).
fn on_main_wait<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(objc2::MainThreadMarker) -> T + Send + 'static,
) -> Option<T> {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::macos::on_main(app, move |mtm| {
            let _ = tx.send(f(mtm));
        });
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, f);
        None
    }
}

// ----------------------------------------------------------------------------- the windows

/// A `rec-*` panel, made the first time and kept hidden between uses.
fn panel(app: &AppHandle, label: &str) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(label) {
        return Some(window);
    }
    let build = || {
        WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
            .title("Glimpsfy")
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .resizable(false)
            .skip_taskbar(true)
            .visible(false)
            .focused(false)
            .always_on_top(true)
            .visible_on_all_workspaces(true)
            .accept_first_mouse(true)
            .disable_drag_drop_handler()
            .inner_size(10.0, 10.0)
            .build()
    };
    #[cfg(target_os = "macos")]
    let built = crate::macos::without_activating(build);
    #[cfg(not(target_os = "macos"))]
    let built = build();
    match built {
        Ok(window) => {
            #[cfg(target_os = "macos")]
            {
                use crate::macos::recording::Panel;
                let kind = match label {
                    TARGET => Panel::Target,
                    BAR => Panel::Bar,
                    CANVAS => Panel::Canvas,
                    _ => Panel::BubbleControls,
                };
                let w = window.clone();
                crate::macos::on_main(app, move |mtm| crate::macos::recording::setup_panel(&w, kind, mtm));
            }
            Some(window)
        }
        Err(error) => {
            trace::log(format!("[glimpsfy] window {label}: {error}"));
            None
        }
    }
}

fn show(app: &AppHandle, label: &str, frame: LogicalRect, key: bool, through: bool) {
    let Some(window) = panel(app, label) else { return };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::recording::show(&window, frame, key, through, mtm));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, key, through);
        let _ = window.set_position(tauri::LogicalPosition::new(frame.x, frame.y));
        let _ = window.set_size(tauri::LogicalSize::new(frame.width, frame.height));
        let _ = window.show();
    }
}

/// Shows the bar window at `frame` and, under it, the native glass for `pieces` in the same
/// turn of the main thread, so the blur and the shadow move with the bar; `through` lets the
/// clicks pass while the pointer is off the glass.
fn show_bar(app: &AppHandle, frame: LogicalRect, key: bool, through: bool, pieces: Vec<setup::GlassPiece>) {
    #[cfg(target_os = "macos")]
    {
        let Some(window) = panel(app, BAR) else { return };
        crate::macos::on_main(app, move |mtm| {
            crate::macos::recording::show(&window, frame, key, through, mtm);
            crate::macos::recording::backdrop_show(&window, frame, &pieces, mtm);
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pieces, through);
        show(app, BAR, frame, key, false);
    }
}

/// Lets the bar window take the mouse, or pass it to what lies below.
fn bar_take_mouse(app: &AppHandle, takes: bool) {
    let Some(window) = app.get_webview_window(BAR) else { return };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::recording::take_mouse(&window, takes, mtm));
    #[cfg(not(target_os = "macos"))]
    let _ = (window, takes);
}

/// Whether the pointer is over one of `rects` (global points). Off macOS there is no pointer to
/// read, and the bar always takes the mouse.
fn pointer_over(rects: &[LogicalRect]) -> bool {
    #[cfg(target_os = "macos")]
    {
        let pointer = crate::macos::pointer();
        rects.iter().any(|r| r.contains(pointer))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = rects;
        true
    }
}

/// Hides a `rec-*` window; its caller holds no lock on the recording's state.
fn hide(app: &AppHandle, label: &str) {
    if label == BAR {
        lock(&state(app).inner).bar_hit.clear();
    }
    let Some(window) = app.get_webview_window(label) else { return };
    #[cfg(target_os = "macos")]
    {
        let bar = label == BAR;
        crate::macos::on_main(app, move |mtm| {
            crate::macos::recording::hide(&window, mtm);
            if bar {
                crate::macos::recording::backdrop_hide(mtm);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = window.hide();
    }
}

/// The window numbers of the app's own windows: those the file must not show, and those it
/// shows (the canvas and the bubble).
fn own_windows(app: &AppHandle) -> OwnWindows {
    let labels: Vec<(String, WebviewWindow)> = app.webview_windows().into_iter().collect();
    on_main_wait(app, move |_mtm| {
        #[cfg(target_os = "macos")]
        {
            use glimpsfy_capture::WindowId;
            let mut own = OwnWindows::default();
            for (label, window) in &labels {
                if let Some(n) = crate::macos::recording::window_number(window, _mtm) {
                    if label == CANVAS {
                        own.shown.push(WindowId(n));
                    } else {
                        own.hidden.push(WindowId(n));
                    }
                }
            }
            if let Some(n) = crate::macos::recording::bubble_window_number(_mtm) {
                own.shown.push(WindowId(n));
            }
            if let Some(n) = crate::macos::recording::backdrop_window_number(_mtm) {
                own.hidden.push(WindowId(n));
            }
            own
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = labels;
            OwnWindows::default()
        }
    })
    .unwrap_or_default()
}

fn visible_frame(app: &AppHandle, display: glimpsfy_capture::DisplayId) -> Option<LogicalRect> {
    #[cfg(target_os = "macos")]
    return on_main_wait(app, move |mtm| crate::macos::recording::visible_frame(display, mtm)).flatten();
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, display);
        None
    }
}

// --------------------------------------------------------------------------------- events

fn emit_state(app: &AppHandle) {
    let now = state(app).recorder.state();
    let _ = app.emit(events::STATE, &now);
    tray(app, &now, None);
}

fn emit_setup(app: &AppHandle) {
    let setup = lock(&state(app).inner).setup.clone();
    let _ = app.emit(events::SETUP, setup);
}

fn emit_bubble(app: &AppHandle) {
    let bubble = lock(&state(app).inner).bubble.clone();
    let _ = app.emit(events::BUBBLE, bubble);
}

fn emit_canvas(app: &AppHandle) {
    let canvas = lock(&state(app).inner).canvas.clone();
    let _ = app.emit_to(EventTarget::webview_window(CANVAS), events::CANVAS, canvas);
}

// ------------------------------------------------------------------------------- the setup

fn camera_from_settings(app: &AppHandle) -> setup::CameraSetup {
    let r = app.state::<AppState>().settings.get().recording;
    setup::CameraSetup {
        on: false,
        device_id: r.webcam_device.clone(),
        size: setup::bubble_size(r.webcam_size),
        corner: setup::corner(r.webcam_corner),
    }
}

/// Opens the setup surface on a display, or retargets the open one.
pub fn open_setup(app: &AppHandle, request: SetupRequest) -> CommandResult<RecordSetup> {
    let rec = state(app);
    if !rec.recorder.state().eq(&RecordingState::Idle) {
        return Err(CommandError::new(ErrorCode::Busy, "a recording is already running"));
    }
    let backend = app.state::<AppState>().backend.clone();
    let displays = backend.displays()?;
    let display = match request.display_id {
        Some(id) => displays.iter().find(|d| d.id == id),
        None => {
            #[cfg(target_os = "macos")]
            let pointer = crate::macos::pointer();
            #[cfg(not(target_os = "macos"))]
            let pointer = LogicalPoint { x: 0.0, y: 0.0 };
            setup::display_at(&displays, pointer)
        }
    }
    .ok_or_else(|| CommandError::not_found("the display is not connected any more"))?;
    let described = setup::describe(display, visible_frame(app, display.id));
    let mode = request.mode.unwrap_or(TargetMode::Area);
    let windows = if mode == TargetMode::Window || request.window_id.is_some() {
        setup::windows_on(&backend.windows(setup::window_options()).unwrap_or_default(), &described)
    } else {
        Vec::new()
    };
    // Retargeting keeps this take's camera and microphone choices.
    let kept = lock(&rec.inner).setup.as_ref().map(|s| (s.camera.clone(), s.microphone));
    let (camera, microphone) = kept
        .unwrap_or_else(|| (camera_from_settings(app), app.state::<AppState>().settings.get().recording.microphone));
    let resolution = setup::resolution(&app.state::<AppState>().settings.get().recording);
    let mut next = RecordSetup::new(mode, described, windows, request.rect, request.window_id, camera, microphone);
    next.settle(resolution);
    lock(&rec.inner).setup = Some(next.clone());
    show_setup(app);
    emit_setup(app);
    sync_bubble(app);
    watch(app);
    Ok(next)
}

/// Shows the target over the setup's display with the keyboard, and the bar once the page has
/// measured it.
fn show_setup(app: &AppHandle) {
    let (frame, bar) = {
        let inner = lock(&state(app).inner);
        let Some(setup) = &inner.setup else { return };
        (setup.display.frame, inner.bar)
    };
    show(app, TARGET, frame, true, false);
    // The bar page shows nothing until it has a setup; it calls `record_bar_fit` then.
    let _ = panel(app, BAR);
    if let Some((bar, pop, glass)) = bar {
        let _ = fit_bar(app, bar, pop, glass);
    }
}

pub fn setup_state(app: &AppHandle) -> Option<RecordSetup> {
    lock(&state(app).inner).setup.clone()
}

pub fn update_setup(app: &AppHandle, update: SetupUpdate) -> CommandResult<RecordSetup> {
    let rec = state(app);
    if let Some(display) = update.display_id {
        let current = lock(&rec.inner).setup.as_ref().map(|s| (s.mode, s.display.id));
        if let Some((mode, id)) = current {
            if id != display {
                open_setup(app, SetupRequest { mode: Some(mode), display_id: Some(display), ..Default::default() })?;
            }
        }
    }
    // The windows are read again on every entry into the window mode and every pick, since the
    // user may have moved, opened or closed some since the last read.
    let needs_windows = {
        let inner = lock(&rec.inner);
        if inner.setup.is_none() {
            return Err(CommandError::not_found("no recording setup is open"));
        }
        update.mode == Some(TargetMode::Window) || update.window_id.is_some()
    };
    let windows = if needs_windows {
        let backend = app.state::<AppState>().backend.clone();
        let display = lock(&rec.inner).setup.as_ref().map(|s| s.display.clone());
        display.map(|d| setup::windows_on(&backend.windows(setup::window_options()).unwrap_or_default(), &d))
    } else {
        None
    };
    let resolution = setup::resolution(&app.state::<AppState>().settings.get().recording);
    let (next, camera_changed) = {
        let mut inner = lock(&rec.inner);
        let setup = inner.setup.as_mut().ok_or_else(|| CommandError::not_found("no recording setup is open"))?;
        if let Some(windows) = windows {
            // The chosen window follows its new place, or goes when it closed.
            let chosen = setup.window.as_ref().map(|w| w.id);
            setup.window = chosen.and_then(|id| windows.iter().find(|w| w.id == id).cloned());
            setup.windows = windows;
        }
        if let Some(mode) = update.mode {
            if mode != setup.mode {
                setup.chip = mode == TargetMode::Display;
            }
            setup.mode = mode;
        }
        // The window mode starts with a window chosen, the front one (where the user works), so
        // the dim shows the target at once and Record is ready; a click picks another.
        if setup.mode == TargetMode::Window && setup.window.is_none() && update.window_id.is_none() {
            setup.window = setup.windows.first().cloned();
        }
        if let Some(rect) = update.rect {
            setup.set_area(rect);
        }
        if let Some(id) = update.window_id {
            setup.window = setup.windows.iter().find(|w| w.id == id).cloned();
            setup.chip = false;
        }
        if update.chip_seen == Some(true) {
            setup.chip = false;
        }
        if let Some(on) = update.microphone {
            setup.microphone = on;
        }
        let mut camera_changed = false;
        if let Some(on) = update.camera {
            camera_changed |= setup.camera.on != on;
            setup.camera.on = on;
        }
        if let Some(device) = update.camera_device {
            camera_changed |= setup.camera.device_id != device;
            setup.camera.device_id = device;
        }
        setup.settle(resolution);
        (setup.clone(), camera_changed)
    };
    emit_setup(app);
    let bar = lock(&rec.inner).bar;
    if let Some((bar, pop, glass)) = bar {
        let _ = fit_bar(app, bar, pop, glass);
    }
    let _ = camera_changed;
    sync_bubble(app);
    Ok(next)
}

/// Close (Esc) on the setup: every recorder window goes and the camera turns off.
pub fn close_setup(app: &AppHandle) {
    {
        let mut inner = lock(&state(app).inner);
        if inner.take.is_some() {
            return;
        }
        inner.setup = None;
        inner.bubble = None;
        inner.bar_shown = false;
        inner.meter = None;
    }
    state(app).meter_generation.fetch_add(1, Ordering::SeqCst);
    for label in [TARGET, BAR, BUBBLE_CONTROLS, CANVAS] {
        hide(app, label);
    }
    hide_bubble(app);
    emit_setup(app);
    emit_bubble(app);
}

/// Places the bar window for the bar and popover the page measured, with the glass under them.
pub fn fit_bar(app: &AppHandle, bar: Size, pop: Option<Size>, glass: BarGlass) -> CommandResult<setup::BarLayout> {
    let rec = state(app);
    let now = rec.recorder.state();
    let mut inner = lock(&rec.inner);
    inner.bar = Some((bar, pop, glass));
    let place = match (&inner.take, &inner.setup) {
        (Some(take), _) if !matches!(now, RecordingState::Idle | RecordingState::Countdown { .. }) => {
            Some((take.mode, take.rect, take.display.clone(), false))
        }
        (None, Some(setup)) if now == RecordingState::Idle => {
            setup.global_rect().map(|rect| (setup.mode, rect, setup.display.clone(), !inner.bar_shown))
        }
        _ => None,
    };
    let Some((mode, rect, display, key)) = place else {
        // Nothing shows a bar now (the countdown, or no setup).
        drop(inner);
        hide(app, BAR);
        return Ok(setup::BarLayout { popover_side: setup::PopoverSide::Below, width: 0.0, height: 0.0 });
    };
    let (frame, side) = setup::place_bar(mode, rect, &display, bar, pop);
    let layout = setup::BarLayout { popover_side: side, width: frame.width, height: frame.height };
    if side != glass.side {
        // The page draws the bar at the stage's other edge: shown now, the bar would sit a
        // popover's height away from its place until the page moved it. The window stays as it
        // is until the page has laid the bar out for this side and fits it again.
        drop(inner);
        let _ = app.emit_to(EventTarget::webview_window(BAR), events::BAR_LAYOUT, layout);
        return Ok(layout);
    }
    inner.bar_shown = true;
    if bar.width <= 0.0 || bar.height <= 0.0 {
        drop(inner);
        hide(app, BAR);
        return Ok(layout);
    }
    let pieces = setup::glass_pieces(frame, bar, pop, side, glass);
    inner.bar_hit =
        pieces.iter().map(|p| LogicalRect { x: frame.x + p.rect.x, y: frame.y + p.rect.y, ..p.rect }).collect();
    inner.bar_mouse = pointer_over(&inner.bar_hit);
    // Sent while the state is locked, so a change the watch thread makes next reaches the
    // window after this one.
    show_bar(app, frame, key, !inner.bar_mouse, pieces);
    Ok(layout)
}

// ------------------------------------------------------------------------------- the bubble

fn bubble_area(inner: &Inner) -> Option<(TargetMode, LogicalRect, SetupDisplay)> {
    if let Some(take) = &inner.take {
        return Some((take.mode, take.rect, take.display.clone()));
    }
    let setup = inner.setup.as_ref()?;
    Some((setup.mode, setup.global_rect()?, setup.display.clone()))
}

fn slots_of(area: LogicalRect, size: f64) -> Vec<Slot> {
    setup::bubble_slots(area, size).iter().map(|(corner, rect)| Slot { corner: *corner, rect: *rect }).collect()
}

/// Shows, moves or hides the bubble to match the setup or the take.
fn sync_bubble(app: &AppHandle) {
    let settings = app.state::<AppState>().settings.get().recording;
    let wanted = {
        let mut inner = lock(&state(app).inner);
        let camera = match (&inner.take, &inner.setup) {
            (Some(take), _) => take.config.webcam.enabled.then(|| {
                let size = match take.config.webcam.size {
                    glimpsfy_record::OverlaySize::Large => BubbleSize::Large,
                    _ => BubbleSize::Small,
                };
                (take.config.webcam.device_id.clone(), size, take.config.webcam.corner)
            }),
            (None, Some(setup)) => {
                setup.camera.on.then(|| (setup.camera.device_id.clone(), setup.camera.size, setup.camera.corner))
            }
            _ => None,
        };
        let area = bubble_area(&inner);
        match (camera, area) {
            (Some((device, size, fallback)), Some((mode, rect, display))) => {
                let usable = setup::usable(mode, rect, &display);
                let points = size.points();
                let corner = inner.bubble.as_ref().filter(|b| b.size == size).map(|b| b.corner).unwrap_or(fallback);
                let slots = slots_of(usable, points);
                let rect = slots.iter().find(|s| s.corner == corner).map(|s| s.rect).unwrap_or(slots[0].rect);
                inner.shape = match settings.webcam_shape {
                    crate::settings::WebcamShape::Circle => WebcamShape::Circle,
                    crate::settings::WebcamShape::RoundedSquare => WebcamShape::RoundedSquare,
                };
                inner.camera_device = device.clone().or(settings.webcam_device.clone());
                inner.bubble =
                    Some(BubbleState { visible: true, size, corner, rect, dragging: false, near: None, slots });
                Some((inner.camera_device.clone(), rect, inner.shape == WebcamShape::Circle))
            }
            _ => {
                inner.bubble = None;
                None
            }
        }
    };
    match wanted {
        Some((device, rect, round)) => {
            #[cfg(target_os = "macos")]
            {
                let handle = app.clone();
                crate::macos::on_main(app, move |mtm| {
                    if let Err(error) = crate::macos::recording::bubble_show(device.as_deref(), rect, round, mtm) {
                        trace::log(format!("[glimpsfy] webcam bubble: {error}"));
                        let _ = handle.emit(events::BUBBLE, None::<BubbleState>);
                    }
                });
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (device, rect, round);
        }
        None => hide_bubble(app),
    }
    emit_bubble(app);
}

fn hide_bubble(app: &AppHandle) {
    hide(app, BUBBLE_CONTROLS);
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, crate::macos::recording::bubble_hide);
}

pub fn bubble_state(app: &AppHandle) -> Option<BubbleState> {
    lock(&state(app).inner).bubble.clone()
}

/// The bubble's hover controls: the other size, or the camera off for this setup or take.
pub fn set_bubble(app: &AppHandle, change: BubbleChange) {
    {
        let mut inner = lock(&state(app).inner);
        if change.off {
            if let Some(take) = inner.take.as_mut() {
                take.config.webcam.enabled = false;
            }
            if let Some(setup) = inner.setup.as_mut() {
                setup.camera.on = false;
            }
        } else if let Some(size) = change.size {
            let corner = inner.bubble.as_ref().map(|b| b.corner);
            if let Some(take) = inner.take.as_mut() {
                take.config.webcam.size = match size {
                    BubbleSize::Small => glimpsfy_record::OverlaySize::Small,
                    BubbleSize::Large => glimpsfy_record::OverlaySize::Large,
                };
                if let Some(corner) = corner {
                    take.config.webcam.corner = corner;
                }
            }
            if let Some(setup) = inner.setup.as_mut() {
                setup.camera.size = size;
                if let Some(corner) = corner {
                    setup.camera.corner = corner;
                }
            }
        }
    }
    if change.off {
        hide(app, BUBBLE_CONTROLS);
    }
    sync_bubble(app);
    emit_setup(app);
}

/// The drag ended: the bubble snaps to the nearest corner, which the next setup starts in.
fn snap_bubble(app: &AppHandle, dropped: LogicalRect) {
    let snapped = {
        let mut inner = lock(&state(app).inner);
        let Some(bubble) = inner.bubble.as_mut() else { return };
        let slots: [(Corner, LogicalRect); 4] = match bubble.slots.as_slice() {
            [a, b, c, d] => [(a.corner, a.rect), (b.corner, b.rect), (c.corner, c.rect), (d.corner, d.rect)],
            _ => return,
        };
        let corner = setup::nearest_corner(&slots, dropped);
        bubble.corner = corner;
        bubble.rect = slots.iter().find(|(c, _)| *c == corner).map(|(_, r)| *r).unwrap_or(dropped);
        bubble.dragging = false;
        bubble.near = None;
        let rect = bubble.rect;
        if let Some(take) = inner.take.as_mut() {
            take.config.webcam.corner = corner;
        }
        if let Some(setup) = inner.setup.as_mut() {
            setup.camera.corner = corner;
        }
        (rect, corner, inner.shape == WebcamShape::Circle)
    };
    let (rect, corner, round) = snapped;
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::recording::bubble_move(rect, round, true, mtm));
    #[cfg(not(target_os = "macos"))]
    let _ = (rect, round);
    emit_bubble(app);
    let store = &app.state::<AppState>().settings;
    let mut settings = store.get();
    if settings.recording.webcam_corner != setup::settings_corner(corner) {
        settings.recording.webcam_corner = setup::settings_corner(corner);
        if let Err(error) = store.save_from_page(settings) {
            trace::log(format!("[glimpsfy] webcam corner: {error}"));
        }
    }
}

// ----------------------------------------------------------------------- the watch thread

/// Follows the pointer while a setup or a take is open: the bubble's drag (it snaps to a corner
/// on release) and hover (its controls show), and the pointer for the keystroke overlay's move
/// rule. Polling the pointer every 30 ms costs less than an event tap and needs no permission.
fn watch(app: &AppHandle) {
    if state(app).watching.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let mut drag: Option<(LogicalPoint, LogicalRect, bool)> = None;
        let mut was_down = false;
        let mut controls = false;
        let mut last_pointer: Option<LogicalPoint> = None;
        // What the target was last told about the pointer while a window is picked.
        let mut last_pick: Option<Option<LogicalPoint>> = None;
        let mut pace = Duration::from_millis(30);
        loop {
            std::thread::sleep(pace);
            let (open, bubble, canvas, keys, picking) = {
                let inner = lock(&state(&app).inner);
                let keys = inner.take.as_ref().is_some_and(|t| t.config.keystrokes.enabled);
                let picking = match (&inner.setup, &inner.take) {
                    (Some(setup), None) if setup.mode == TargetMode::Window => Some(setup.display.frame),
                    _ => None,
                };
                (
                    inner.setup.is_some() || inner.take.is_some(),
                    inner.bubble.clone(),
                    inner.canvas.clone(),
                    keys,
                    picking,
                )
            };
            // The outline follows the pointer at the display's pace while a window is picked.
            pace = Duration::from_millis(if picking.is_some() { 16 } else { 30 });
            if !open {
                state(&app).watching.store(false, Ordering::SeqCst);
                if controls {
                    hide(&app, BUBBLE_CONTROLS);
                }
                return;
            }
            #[cfg(target_os = "macos")]
            let (pointer, down) = (crate::macos::pointer(), crate::macos::recording::left_button_down());
            #[cfg(not(target_os = "macos"))]
            let (pointer, down) = (LogicalPoint { x: 0.0, y: 0.0 }, false);

            // While a window is picked, the target outlines the window under the pointer. Its web
            // view gets no mouse moves while the bar holds the keyboard, so the pointer comes
            // from here; over the bar's glass or off the display there is none.
            match picking {
                Some(display) => {
                    let over_bar = lock(&state(&app).inner).bar_hit.iter().any(|r| r.contains(pointer));
                    let at = (!over_bar && display.contains(pointer)).then_some(pointer);
                    let moved = match (at, last_pick.flatten()) {
                        (Some(a), Some(b)) => (a.x - b.x).abs() + (a.y - b.y).abs() >= 1.0,
                        _ => last_pick != Some(at),
                    };
                    if moved {
                        last_pick = Some(at);
                        let _ = app.emit_to(EventTarget::webview_window(TARGET), events::POINTER, at);
                    }
                }
                None => last_pick = None,
            }

            // The bar's window takes the mouse only over its glass; with a button down nothing
            // changes, so a press keeps its window until it is let go.
            #[cfg(target_os = "macos")]
            {
                let mut inner = lock(&state(&app).inner);
                if !inner.bar_hit.is_empty() && !down {
                    let over = inner.bar_hit.iter().any(|r| r.contains(pointer));
                    if over != inner.bar_mouse {
                        inner.bar_mouse = over;
                        bar_take_mouse(&app, over);
                    }
                }
            }

            if let Some(bubble) = bubble.filter(|b| b.visible) {
                let over = bubble.rect.contains(pointer);
                if down && !was_down && over {
                    drag = Some((pointer, bubble.rect, false));
                }
                if let Some((start, origin, moved)) = drag.as_mut() {
                    let (dx, dy) = (pointer.x - start.x, pointer.y - start.y);
                    if !*moved && dx.abs() + dy.abs() > 3.0 {
                        *moved = true;
                    }
                    let rect = LogicalRect { x: origin.x + dx, y: origin.y + dy, ..*origin };
                    if *moved && down {
                        let slots: Option<[(Corner, LogicalRect); 4]> = match bubble.slots.as_slice() {
                            [a, b, c, d] => {
                                Some([(a.corner, a.rect), (b.corner, b.rect), (c.corner, c.rect), (d.corner, d.rect)])
                            }
                            _ => None,
                        };
                        let near = slots.map(|s| setup::nearest_corner(&s, rect));
                        let changed = {
                            let mut inner = lock(&state(&app).inner);
                            match inner.bubble.as_mut() {
                                Some(b) if !b.dragging || b.near != near || b.rect != rect => {
                                    b.dragging = true;
                                    b.near = near;
                                    b.rect = rect;
                                    true
                                }
                                _ => false,
                            }
                        };
                        if changed {
                            emit_bubble(&app);
                        }
                    }
                    if !down {
                        let moved = *moved;
                        drag = None;
                        if moved {
                            snap_bubble(&app, rect);
                        }
                    }
                }
                // A press on a hover control is not a drag until it moves, so the controls stay.
                let hover = over && !drag.as_ref().is_some_and(|(_, _, moved)| *moved);
                if hover != controls {
                    controls = hover;
                    if hover {
                        let (w, h) = CONTROLS_SIZE;
                        let r = bubble.rect;
                        let frame = LogicalRect {
                            x: r.x + (r.width - w) / 2.0,
                            y: r.y + r.height - h - 6.0,
                            width: w,
                            height: h,
                        };
                        show(&app, BUBBLE_CONTROLS, frame, false, false);
                    } else {
                        hide(&app, BUBBLE_CONTROLS);
                    }
                }
            } else if controls {
                controls = false;
                hide(&app, BUBBLE_CONTROLS);
            }
            was_down = down;

            if let (true, Some(canvas)) = (keys, canvas) {
                let inside = canvas.rect.contains(pointer).then_some(pointer);
                let moved = match (inside, last_pointer) {
                    (Some(a), Some(b)) => (a.x - b.x).abs() + (a.y - b.y).abs() >= 2.0,
                    (None, None) => false,
                    _ => true,
                };
                if moved {
                    last_pointer = inside;
                    let _ = app.emit_to(EventTarget::webview_window(CANVAS), events::POINTER, inside);
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------------- meters

/// Meters these microphones with `record://levels` about 15 times a second; `None` stops.
pub fn meter(app: &AppHandle, microphones: Option<Vec<String>>, system: bool) {
    let rec = state(app);
    let generation = rec.meter_generation.fetch_add(1, Ordering::SeqCst) + 1;
    lock(&rec.inner).meter = None;
    let Some(ids) = microphones else {
        return;
    };
    if ids.is_empty() && !system {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // Opening a microphone blocks for a moment; the meter thread pays it.
        let meter = Meter::open(&ids);
        {
            let rec = state(&app);
            if rec.meter_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            lock(&rec.inner).meter = Some(meter);
        }
        loop {
            std::thread::sleep(Duration::from_millis(66));
            let rec = state(&app);
            if rec.meter_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let levels = match lock(&rec.inner).meter.as_ref() {
                Some(meter) => meter.levels(),
                None => return,
            };
            // The system audio has no meter before a take: it would need a capture stream, and with
            // it the screen recording indicator, only to draw a bar.
            let payload = serde_json::json!({ "microphones": levels, "system": if system { Some(0.0) } else { None } });
            let _ = app.emit(events::LEVELS, payload);
        }
    });
}

// --------------------------------------------------------------------------------- the take

fn recording_name() -> String {
    format!("Glimpsfy Recording {}", chrono::Local::now().format("%Y-%m-%d at %H.%M.%S"))
}

/// The geometry of a configuration given without a setup: its display and recorded rectangle.
fn geometry_of(app: &AppHandle, target: &StreamTarget) -> CommandResult<(TargetMode, SetupDisplay, LogicalRect)> {
    let backend = app.state::<AppState>().backend.clone();
    let displays = backend.displays()?;
    let (mode, display, rect) = match target {
        StreamTarget::Display { display } => {
            let d = displays
                .iter()
                .find(|d| d.id == *display)
                .ok_or_else(|| CommandError::not_found("the display is not connected any more"))?;
            (TargetMode::Display, d, d.frame)
        }
        StreamTarget::Area { display, rect } => {
            let d = displays
                .iter()
                .find(|d| d.id == *display)
                .ok_or_else(|| CommandError::not_found("the display is not connected any more"))?;
            (TargetMode::Area, d, LogicalRect { x: rect.x + d.frame.x, y: rect.y + d.frame.y, ..*rect })
        }
        StreamTarget::Window { window } => {
            let all = backend.windows(setup::window_options())?;
            let w =
                all.iter().find(|w| w.id == *window).ok_or_else(|| CommandError::not_found("the window is closed"))?;
            let centre = LogicalPoint { x: w.bounds.x + w.bounds.width / 2.0, y: w.bounds.y + w.bounds.height / 2.0 };
            let d = setup::display_at(&displays, centre)
                .ok_or_else(|| CommandError::not_found("no display shows the window"))?;
            (TargetMode::Window, d, w.bounds)
        }
    };
    Ok((mode, setup::describe(display, visible_frame(app, display.id)), rect))
}

/// Starts the countdown, and then the take. Without a configuration it records the open setup
/// with Settings › Recording.
pub async fn start(app: AppHandle, config: Option<RecordingConfig>) -> CommandResult<RecordingId> {
    let rec = state(&app);
    if rec.recorder.state() != RecordingState::Idle {
        return Err(CommandError::new(ErrorCode::Busy, "a recording is already running"));
    }
    let settings = app.state::<AppState>().settings.get();
    let (config, mode, display, rect, from_setup) = match config {
        Some(config) => {
            let (mode, display, rect) = geometry_of(&app, &config.target)?;
            (config, mode, display, rect, false)
        }
        None => {
            let setup =
                lock(&rec.inner).setup.clone().ok_or_else(|| CommandError::invalid("no recording setup is open"))?;
            let target =
                setup.target.clone().ok_or_else(|| CommandError::invalid("choose a window to record first"))?;
            let rect = setup.global_rect().ok_or_else(|| CommandError::invalid("choose what to record first"))?;
            let permissions = crate::permissions::status(&app);
            let devices = glimpsfy_record::devices::list();
            let config = setup::config(&settings.recording, &setup, target, &permissions, devices.system_audio);
            (config, setup.mode, setup.display.clone(), rect, true)
        }
    };
    config.validate()?;
    let id = RecordingId::new();
    let folder = config.output_folder.clone();
    std::fs::create_dir_all(&folder).map_err(|e| {
        CommandError::new(ErrorCode::Io, format!("the recordings folder {} could not be made: {e}", folder.display()))
    })?;
    let path = glimpsfy_record::files::free_path(&folder, &recording_name(), "mp4");
    let take = Take {
        id: id.clone(),
        mode,
        display: display.clone(),
        rect,
        has_microphone: config.audio.microphone.is_some(),
        has_system: config.audio.system,
        config: config.clone(),
        from_setup,
        skip: false,
    };

    {
        let mut inner = lock(&rec.inner);
        // The take is claimed here, in one step under the lock: two starts at once (the shortcut
        // and the button) both passed the check above, and the later one must leave the first
        // one's windows alone. `finished` gives the claim back.
        if inner.take.is_some() || rec.recorder.state() != RecordingState::Idle {
            return Err(CommandError::new(ErrorCode::Busy, "a recording is already running"));
        }
        // The meters let go of the microphones; the take opens its own.
        rec.meter_generation.fetch_add(1, Ordering::SeqCst);
        inner.meter = None;
        let (lift, drop) = setup::keys_offsets(mode, &display);
        inner.canvas = Some(CanvasConfig {
            mode,
            rect,
            clicks: config.clicks,
            keystrokes: config.keystrokes,
            lift_bottom: lift,
            drop_top: drop,
        });
        inner.take = Some(take);
    }
    // The countdown: the target shows it with the keyboard (Esc cancels, Return starts), the
    // bar goes, the canvas waits over the recorded rectangle.
    hide(&app, BAR);
    show(&app, TARGET, display.frame, true, false);
    show(&app, CANVAS, rect, false, true);
    emit_canvas(&app);
    sync_bubble(&app);
    watch(&app);
    let own = own_windows(&app);

    let recorder = rec.recorder.clone();
    let (begin_id, begin_config) = (id.clone(), config.clone());
    let begun = tauri::async_runtime::spawn_blocking(move || recorder.begin(begin_id, begin_config, path, &own))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))
        .and_then(|begun| begun.map_err(CommandError::from));
    if let Err(error) = begun {
        // Nothing was written: back to the setup as it was. A countdown cancelled while the
        // stream opened has already ended through `finished`, and the claim may belong to a
        // newer take by now, so only this take's own claim and windows go.
        let ours = {
            let mut inner = lock(&rec.inner);
            let ours = inner.take.as_ref().is_some_and(|t| t.id == id);
            if ours {
                inner.take = None;
            }
            ours
        };
        if ours {
            end_take_windows(&app, from_setup);
        }
        emit_state(&app);
        return Err(error);
    }
    emit_state(&app);
    let skipped = lock(&rec.inner).take.as_ref().is_some_and(|t| t.id == id && t.skip);
    if config.countdown_seconds == 0 || skipped {
        go_live(&app, &id);
    } else {
        countdown(&app, id.clone());
    }
    Ok(id)
}

fn countdown(app: &AppHandle, id: RecordingId) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let recorder = state(&app).recorder.clone();
        if recorder.current().as_ref() != Some(&id) {
            return;
        }
        match recorder.countdown_tick() {
            Ok(0) => {
                go_live(&app, &id);
                return;
            }
            Ok(_) => emit_state(&app),
            // Skipped, cancelled or failed meanwhile.
            Err(_) => return,
        }
    });
}

/// Return during the countdown: the take starts now.
pub fn skip_countdown(app: &AppHandle) -> CommandResult<()> {
    let id = state(app).recorder.current().ok_or_else(|| CommandError::not_found("no recording is running"))?;
    if !matches!(state(app).recorder.state(), RecordingState::Countdown { .. }) {
        return Err(CommandError::invalid("the countdown is over"));
    }
    // While the stream opens the recorder refuses to start, and Return used to be lost: the
    // countdown then ran its full length. `start` reads this once the stream is open.
    if let Some(take) = lock(&state(app).inner).take.as_mut().filter(|t| t.id == id) {
        take.skip = true;
    }
    go_live(app, &id);
    Ok(())
}

/// The countdown ended: buffers belong to the take from now on.
fn go_live(app: &AppHandle, id: &RecordingId) {
    let rec = state(app);
    if rec.recorder.current().as_ref() != Some(id) {
        return;
    }
    if let Err(error) = rec.recorder.go() {
        trace::log(format!("[glimpsfy] recording {}: {error}", id.0));
        return;
    }
    emit_state(app);
    // The target keeps the red frame and lets every click through to the app being recorded,
    // which gets the keyboard back.
    #[cfg(target_os = "macos")]
    if let Some(target) = app.get_webview_window(TARGET) {
        crate::macos::on_main(app, move |mtm| crate::macos::recording::pass_through(&target, mtm));
    }
    let watch_input = lock(&rec.inner).take.as_ref().map(|t| (t.config.clicks.enabled, t.config.keystrokes));
    if let Some((clicks, keys)) = watch_input {
        #[cfg(target_os = "macos")]
        {
            let handle = app.clone();
            crate::macos::on_main(app, move |mtm| {
                crate::macos::recording::watch_input(
                    &handle,
                    crate::macos::recording::InputWatch {
                        clicks,
                        keys: keys.enabled,
                        only_shortcuts: keys.only_shortcuts,
                        on_click,
                        on_keys,
                    },
                    mtm,
                )
            });
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (clicks, keys);
    }
    // The bar page fits the control bar when its size differs from the last one it reported; when
    // it is the same the page stays silent, so the last sizes place the window here.
    let bar = lock(&rec.inner).bar;
    if let Some((bar, pop, glass)) = bar {
        let _ = fit_bar(app, bar, pop, glass);
    }
    ticks(app, id.clone());
}

fn on_click(app: &AppHandle, point: LogicalPoint) {
    let wanted =
        lock(&state(app).inner).take.as_ref().is_some_and(|t| t.config.clicks.enabled && t.rect.contains(point));
    if wanted && matches!(state(app).recorder.state(), RecordingState::Recording { .. }) {
        let _ = app.emit_to(EventTarget::webview_window(CANVAS), events::CLICK, point);
    }
}

fn on_keys(app: &AppHandle, caps: Vec<String>) {
    let wanted = lock(&state(app).inner).take.as_ref().is_some_and(|t| t.config.keystrokes.enabled);
    if wanted && matches!(state(app).recorder.state(), RecordingState::Recording { .. }) {
        let _ = app.emit_to(EventTarget::webview_window(CANVAS), events::KEYS, serde_json::json!({ "caps": caps }));
    }
}

/// Sends `record://tick` four times a second while the take records or pauses, and keeps the
/// tray's time current.
fn ticks(app: &AppHandle, id: RecordingId) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        let rec = state(&app);
        let Some(progress) = rec.recorder.progress() else { return };
        if progress.id != id {
            return;
        }
        let (mic, system, muted) = {
            let inner = lock(&rec.inner);
            let take = inner.take.as_ref();
            (take.is_some_and(|t| t.has_microphone), take.is_some_and(|t| t.has_system), rec.recorder.state())
        };
        let muted = matches!(
            muted,
            RecordingState::Recording { microphone_muted: true, .. }
                | RecordingState::Paused { microphone_muted: true, .. }
        );
        let tick = events::RecordTick {
            id: progress.id.clone(),
            elapsed_ms: progress.elapsed_ms,
            microphone_level: (mic && !muted).then_some(progress.microphone_level),
            system_level: system.then_some(progress.system_level),
            file_bytes: progress.file_bytes,
        };
        let _ = app.emit(events::TICK, &tick);
        tray(&app, &rec.recorder.state(), Some(progress.elapsed_ms));
    });
}

pub fn pause(app: &AppHandle) -> CommandResult<()> {
    state(app).recorder.pause()?;
    emit_state(app);
    Ok(())
}

pub fn resume(app: &AppHandle) -> CommandResult<()> {
    state(app).recorder.resume()?;
    emit_state(app);
    Ok(())
}

pub fn keep(app: &AppHandle) -> CommandResult<()> {
    state(app).recorder.keep()?;
    emit_state(app);
    Ok(())
}

pub fn set_muted(app: &AppHandle, muted: bool) -> CommandResult<()> {
    state(app).recorder.set_microphone_muted(muted)?;
    emit_state(app);
    Ok(())
}

/// Esc during the countdown: back to the setup with every choice intact.
pub async fn cancel_countdown(app: AppHandle) -> CommandResult<()> {
    let recorder = state(&app).recorder.clone();
    if !matches!(recorder.state(), RecordingState::Countdown { .. }) {
        return Err(CommandError::invalid("no countdown is running"));
    }
    let outcome = tauri::async_runtime::spawn_blocking(move || recorder.cancel())
        .await
        .map_err(|e| CommandError::internal(e.to_string()))??;
    finished(&app, outcome);
    Ok(())
}

/// Stops the take and resolves when the file is complete.
pub async fn stop(app: AppHandle) -> CommandResult<glimpsfy_record::RecordingResult> {
    let recorder = state(&app).recorder.clone();
    let id = match recorder.state() {
        RecordingState::Recording { id, .. } | RecordingState::Paused { id, .. } => id,
        RecordingState::Countdown { .. } => {
            cancel_countdown(app).await?;
            return Err(CommandError::new(
                ErrorCode::Cancelled,
                "the countdown was cancelled before anything was recorded",
            ));
        }
        _ => return Err(CommandError::not_found("no recording is running")),
    };
    // Finishing a long take takes a moment; the bar shows it.
    let _ = app.emit(events::STATE, RecordingState::Finishing { id });
    let outcome = tauri::async_runtime::spawn_blocking(move || recorder.stop())
        .await
        .map_err(|e| CommandError::internal(e.to_string()))??;
    finished(&app, outcome.clone());
    match outcome {
        RecordingOutcome::Saved { result } => Ok(result),
        RecordingOutcome::Failed { code, message, .. } => Err(CommandError::new(error_code(&code), message)),
        _ => Err(CommandError::new(ErrorCode::Cancelled, "the take ended without a file")),
    }
}

/// Discard: without `confirmed` the take pauses and the bar asks; with it, after the question,
/// the file goes to the Trash. During the countdown it cancels.
pub async fn discard(app: AppHandle, confirmed: bool) -> CommandResult<&'static str> {
    let recorder = state(&app).recorder.clone();
    match recorder.state() {
        RecordingState::Countdown { .. } => {
            cancel_countdown(app).await?;
            return Ok("discarded");
        }
        RecordingState::Recording { .. } | RecordingState::Paused { .. } => {}
        _ => return Err(CommandError::not_found("no recording is running")),
    }
    if !confirmed {
        recorder.ask_discard()?;
        emit_state(&app);
        return Ok("asked");
    }
    let id = recorder.current();
    if let Some(id) = id {
        let _ = app.emit(events::STATE, RecordingState::Finishing { id });
    }
    let outcome = tauri::async_runtime::spawn_blocking(move || recorder.discard())
        .await
        .map_err(|e| CommandError::internal(e.to_string()))??;
    finished(&app, outcome);
    Ok("discarded")
}

/// The UI's error code for an outcome's code, which the recorder writes in the UI's words.
fn error_code(code: &str) -> ErrorCode {
    match code {
        "permission_denied" => ErrorCode::PermissionDenied,
        "not_found" => ErrorCode::NotFound,
        "capture" => ErrorCode::Capture,
        "io" => ErrorCode::Io,
        "cancelled" => ErrorCode::Cancelled,
        "busy" => ErrorCode::Busy,
        "invalid_argument" => ErrorCode::InvalidArgument,
        "not_implemented" => ErrorCode::NotImplemented,
        "provider" => ErrorCode::Provider,
        _ => ErrorCode::Internal,
    }
}

/// Hides the take's windows; back to the setup when the take came from it and ended before
/// anything was recorded, closed otherwise.
fn end_take_windows(app: &AppHandle, back_to_setup: bool) {
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, crate::macos::recording::unwatch_input);
    {
        let mut inner = lock(&state(app).inner);
        inner.canvas = None;
        inner.bar_shown = false;
    }
    hide(app, CANVAS);
    emit_canvas(app);
    if back_to_setup && lock(&state(app).inner).setup.is_some() {
        show_setup(app);
        sync_bubble(app);
        emit_setup(app);
        return;
    }
    {
        let mut inner = lock(&state(app).inner);
        inner.setup = None;
        inner.bubble = None;
    }
    for label in [TARGET, BAR, BUBBLE_CONTROLS] {
        hide(app, label);
    }
    hide_bubble(app);
    emit_setup(app);
    emit_bubble(app);
}

/// A take ended: the windows go, the state returns to idle, `record://finished` follows once,
/// and a saved file reaches the history, the toast and the trim window.
pub fn finished(app: &AppHandle, outcome: RecordingOutcome) {
    let take = lock(&state(app).inner).take.take();
    let cancelled = matches!(outcome, RecordingOutcome::Cancelled { .. });
    end_take_windows(app, cancelled && take.as_ref().is_some_and(|t| t.from_setup));
    emit_state(app);
    let _ = app.emit(events::FINISHED, &outcome);
    match outcome {
        RecordingOutcome::Saved { result } => {
            let app = app.clone();
            std::thread::spawn(move || keep_take(&app, take.as_ref(), result, None));
        }
        RecordingOutcome::Failed { code, message, partial, .. } => {
            let error = CommandError::new(error_code(&code), message);
            match partial {
                // What was written plays to its last fragment: it is kept, in the history too.
                Some(result) => {
                    let app = app.clone();
                    std::thread::spawn(move || keep_take(&app, take.as_ref(), result, Some(error)));
                }
                None => windows::toast_failure(app, take.as_ref().map(|t| t.display.id), &error),
            }
        }
        RecordingOutcome::Discarded { .. } | RecordingOutcome::Cancelled { .. } => {}
    }
}

/// A small PNG of a recording's first frame as a data URL, for a toast.
pub fn thumbnail(path: &Path) -> Option<String> {
    let frame = glimpsfy_record::platform::frame_at(path, 0, 180).ok()?;
    let png = frame.encode(glimpsfy_capture::ImageEncoding::Png(glimpsfy_capture::PngEffort::Fast)).ok()?;
    Some(format!("data:image/png;base64,{}", base64(&png)))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
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

/// How long a poster stays in the cache. The history reads it once, on a thread it starts the
/// moment the take is kept, so an hour leaves a wide margin.
const POSTER_LIFE: Duration = Duration::from_secs(60 * 60);

/// Where the posters go: one PNG per kept take, which only the history's thumbnail reads.
fn poster_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    crate::paths::cache_dir(app).ok().map(|dir| dir.join("recordings"))
}

/// Removes the posters written before `cutoff`. The history keeps a thumbnail of its own and
/// never reads a poster again, so without this the folder would grow by one image per take.
fn prune_posters(dir: &Path, cutoff: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        // Only plain files the recorder wrote: a link or anything else here is left alone.
        let is_poster = entry.file_type().is_ok_and(|t| t.is_file())
            && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|at| at < cutoff);
        if is_poster && old {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Hands a finished (or partial) take to the history with a poster, shows the toast and opens
/// the trim window. Off the main thread: the poster decodes a frame.
fn keep_take(
    app: &AppHandle,
    take: Option<&Take>,
    result: glimpsfy_record::RecordingResult,
    failure: Option<CommandError>,
) {
    use crate::commands::history::{record_capture, CaptureHandoff};
    use crate::windows::{toast, toast_failure, Toast, ToastKind};

    state(app).library.remember(result.clone(), None);
    let poster = poster_dir(app).and_then(|dir| {
        if let Some(cutoff) = std::time::SystemTime::now().checked_sub(POSTER_LIFE) {
            prune_posters(&dir, cutoff);
        }
        let frame = glimpsfy_record::platform::frame_at(&result.path, 0, 720).ok()?;
        let png = frame.encode(glimpsfy_capture::ImageEncoding::Png(glimpsfy_capture::PngEffort::Fast)).ok()?;
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{}.png", result.id.0));
        std::fs::write(&path, png).ok()?;
        Some(path)
    });
    let unread = poster.clone();
    let handoff = CaptureHandoff {
        kind: Some(glimpsfy_history::EntryKind::Recording),
        name: result.path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        saved_path: Some(result.path.clone()),
        poster_path: poster,
        width: Some(result.size.width),
        height: Some(result.size.height),
        scale: take.map(|t| t.display.scale),
        duration_ms: Some(result.duration_ms),
        bytes: Some(result.file_bytes),
        display_id: take.map(|t| t.display.id.0),
        display_name: take.map(|t| t.display.name.clone()),
        created_at_ms: Some(result.created_at_ms),
        ..CaptureHandoff::default()
    };
    let recorded = record_capture(app, handoff);
    match &recorded {
        Ok(Some(entry)) => state(app).library.set_entry(&result.id, entry.id.0.clone()),
        Ok(None) => {}
        Err(error) => trace::log(format!("[glimpsfy] history of recording {}: {error}", result.id.0)),
    }
    // With the history off, or without an entry, no thumbnail will read the poster.
    if !matches!(recorded, Ok(Some(_))) {
        if let Some(path) = unread {
            let _ = std::fs::remove_file(path);
        }
    }

    let display = take.map(|t| t.display.id);
    match failure {
        Some(error) => {
            let error = CommandError::new(
                error.code,
                format!("{}; what was recorded is kept at {}", error.message, result.path.display()),
            );
            toast_failure(app, display, &error);
        }
        None => {
            // A take the full disk stopped is saved all the same: one toast says both.
            let disk_full = result.stopped_because == Some(StopReason::DiskFull);
            let mut saved = Toast::new(if disk_full { ToastKind::DiskFull } else { ToastKind::Saved });
            saved.path = Some(result.path.display().to_string());
            saved.folder = result.path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
            saved.width = Some(result.size.width);
            saved.height = Some(result.size.height);
            saved.bytes = Some(result.file_bytes);
            saved.format = Some("mp4");
            saved.thumbnail = thumbnail(&result.path);
            toast(app, display, saved);
            // The trim window follows every take; the history already holds the full one.
            let source = media::TrimSource::Recording { recording_id: result.id.clone() };
            if let Err(error) = media::open_trim(app, source, media::StartFormat::Mp4) {
                trace::log(format!("[glimpsfy] trim window: {error}"));
            }
        }
    }
}

// ----------------------------------------------------------------------------------- the tray

/// The tray while a take runs: the glyph turns red and the elapsed time follows it; paused shows
/// the plain glyph, a pause mark and the frozen time; the countdown shows its seconds.
fn tray(app: &AppHandle, now: &RecordingState, elapsed: Option<u64>) {
    let Some(icon) = crate::tray::icon(app) else { return };
    let clock = |ms: u64| {
        let s = ms / 1000;
        if s >= 3600 {
            format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        } else {
            format!("{}:{:02}", s / 60, s % 60)
        }
    };
    let glyph = tauri::image::Image::from_bytes(TRAY_GLYPH).ok();
    match now {
        RecordingState::Recording { elapsed_ms, .. } => {
            if let Some(red) = glyph.as_ref().map(red_glyph) {
                let _ = icon.set_icon(Some(red));
                let _ = icon.set_icon_as_template(false);
            }
            let _ = icon.set_title(Some(clock(elapsed.unwrap_or(*elapsed_ms))));
        }
        RecordingState::Paused { elapsed_ms, .. } => {
            let _ = icon.set_icon(glyph);
            let _ = icon.set_icon_as_template(true);
            let _ = icon.set_title(Some(format!("❚❚ {}", clock(elapsed.unwrap_or(*elapsed_ms)))));
        }
        RecordingState::Countdown { remaining_seconds, .. } => {
            let _ = icon.set_title(Some(remaining_seconds.to_string()));
        }
        RecordingState::Finishing { .. } => {}
        RecordingState::Idle => {
            let _ = icon.set_icon(glyph);
            let _ = icon.set_icon_as_template(true);
            let _ = icon.set_title(None::<String>);
        }
    }
}

/// The glyph in the kit's recording red, keeping its shape.
fn red_glyph(glyph: &tauri::image::Image<'_>) -> tauri::image::Image<'static> {
    let mut rgba = glyph.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = 0xFF;
        pixel[1] = 0x3B;
        pixel[2] = 0x30;
    }
    tauri::image::Image::new_owned(rgba, glyph.width(), glyph.height())
}

// ------------------------------------------------------------------------------- shortcuts

/// The recording shortcuts, from `shortcuts.rs`'s dispatcher. Record Screen opens the setup and
/// stops a running take (the same key ends what it began); Record Display opens the setup on
/// the display under the pointer; Pause and Mute toggle; Discard asks in the bar.
pub fn shortcut(app: &AppHandle, action: Action) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let now = state(&app).recorder.state();
        let result: CommandResult<()> = match (action, &now) {
            (
                Action::RecordScreen | Action::RecordDisplay,
                RecordingState::Recording { .. } | RecordingState::Paused { .. },
            ) => stop(app.clone()).await.map(|_| ()),
            (Action::RecordScreen | Action::RecordDisplay, RecordingState::Countdown { .. }) => skip_countdown(&app),
            (Action::RecordScreen, RecordingState::Idle) => {
                if lock(&state(&app).inner).setup.is_some() {
                    start(app.clone(), None).await.map(|_| ())
                } else {
                    open_setup(&app, SetupRequest { mode: Some(TargetMode::Area), ..Default::default() }).map(|_| ())
                }
            }
            (Action::RecordDisplay, RecordingState::Idle) => {
                open_setup(&app, SetupRequest { mode: Some(TargetMode::Display), ..Default::default() }).map(|_| ())
            }
            (Action::PauseRecording, RecordingState::Recording { .. }) => pause(&app),
            (Action::PauseRecording, RecordingState::Paused { .. }) => resume(&app),
            (
                Action::MuteMicrophone,
                RecordingState::Recording { microphone_muted, .. } | RecordingState::Paused { microphone_muted, .. },
            ) => set_muted(&app, !microphone_muted),
            (Action::DiscardRecording, RecordingState::Recording { .. } | RecordingState::Paused { .. }) => {
                discard(app.clone(), false).await.map(|_| ())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            trace::log(format!("[glimpsfy] {action:?}: {error}"));
        }
    });
}

/// ⌥-click on the menu bar icon: stops a take that is recording or paused, or cancels its
/// countdown, and says whether there was one, so the click does nothing else.
pub fn stop_from_tray(app: &AppHandle) -> bool {
    let running = matches!(
        recorder_state(app),
        RecordingState::Countdown { .. } | RecordingState::Recording { .. } | RecordingState::Paused { .. }
    );
    if running {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = stop(app).await {
                if error.code != ErrorCode::Cancelled {
                    trace::log(format!("[glimpsfy] stopping the recording from the menu bar icon: {error}"));
                }
            }
        });
    }
    running
}

/// Whether the recording actions have their keys: always, now that recording works.
pub fn handles(action: Action) -> bool {
    matches!(
        action,
        Action::RecordScreen
            | Action::RecordDisplay
            | Action::PauseRecording
            | Action::MuteMicrophone
            | Action::DiscardRecording
    )
}

/// The recorder's state, for a bar or target page that opens or reloads during a take.
pub fn recorder_state(app: &AppHandle) -> RecordingState {
    state(app).recorder.state()
}

/// The canvas's configuration, for the canvas page's first read.
pub fn canvas_state(app: &AppHandle) -> Option<CanvasConfig> {
    lock(&state(app).inner).canvas.clone()
}
