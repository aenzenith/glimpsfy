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
//! - `rec-canvas` covers the recorded rectangle, click-through, and draws the click rings, the
//!   key caps, the drawings and the spotlight. It takes the mouse only in drawing mode
//!   (`present`). Recorded.
//! - the webcam bubble, a native panel with the camera's preview (`macos::recording`). Recorded.
//! - `rec-bubble-ctl` holds the bubble's hover controls. Never recorded.
//! - `rec-rail` holds drawing mode's tools beside the recorded area; it never takes the keyboard
//!   (`present`). Never recorded.
//! - `recorder` is the trim window, an ordinary window.
//!
//! Threads: the commands' blocking calls (opening the stream, finishing the file, exports) run
//! on worker threads; one countdown thread per take ticks the seconds; one tick thread sends
//! `record://tick` four times a second while a take runs; one watch thread follows the pointer
//! for the bubble's drag and hover, the keystroke overlay, the rail's and the canvas's hold on the
//! mouse while a setup or a take is open; one thread at start removes the posters of earlier
//! runs. A quit during a take holds the exit ([`hold_exit`]); one quit thread stops the take as
//! Stop does, waits until its file is finished and in the history, and exits.

pub mod media;
pub mod present;
pub mod setup;
pub mod transcript;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use glimpsfy_capture::{CaptureOptions, DisplayId, LogicalPoint, LogicalRect, StreamTarget};
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
pub const RAIL: &str = "rec-rail";
/// Every panel of the recorder, which a capture's overlay never takes the keyboard from.
pub const PANELS: [&str; 5] = [TARGET, BAR, CANVAS, BUBBLE_CONTROLS, RAIL];
/// The bubble's hover controls, laid over its lower edge.
const CONTROLS_SIZE: (f64, f64) = (96.0, 44.0);

/// The kit's menu bar glyph; while a take records, a red copy of it shows instead.
#[cfg(target_os = "macos")]
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
    /// Recording › Time limit as the take started with it: the take stops and saves itself once
    /// this much has been recorded (pauses do not count).
    limit_ms: Option<u64>,
    /// Return was pressed while the stream still opened, when the recorder cannot start yet: the
    /// take starts the moment the stream is open instead of counting down.
    skip: bool,
    /// The countdown ended and the take is being armed (see `go_live`).
    #[cfg(not(target_os = "macos"))]
    going: bool,
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
    /// Own windows that step aside for the length of a take, where the system cannot keep them
    /// out of a recording; dropping it brings them back.
    #[cfg(not(target_os = "macos"))]
    hidden: Option<crate::desktop::CaptureGuard>,
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
    /// Takes being finished or handed to the history ([`Ending`]); a quit waits for them.
    ending: AtomicUsize,
    /// Until when a quit waits for the take, once one asked.
    quitting: Mutex<Option<Instant>>,
}

/// Counts a take as being finished or handed to the history while it lives. The recorder is
/// idle again before the hand-off starts, so its state alone would let a quit exit in between.
struct Ending(AppHandle);

impl Ending {
    fn new(app: &AppHandle) -> Ending {
        state(app).ending.fetch_add(1, Ordering::SeqCst);
        Ending(app.clone())
    }
}

impl Drop for Ending {
    fn drop(&mut self) {
        state(&self.0).ending.fetch_sub(1, Ordering::SeqCst);
    }
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
            let _ending = Ending::new(&app);
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
        ending: AtomicUsize::new(0),
        quitting: Mutex::new(None),
    });
    transcript::setup(app);
    // A poster is read only in the run that wrote it, so an earlier run's posters serve no one.
    if let Some(dir) = poster_dir(app) {
        let started = std::time::SystemTime::now();
        std::thread::spawn(move || prune_posters(&dir, started));
    }
}

/// Runs `f` on the main thread and waits for its answer (up to two seconds).
#[cfg(target_os = "macos")]
fn on_main_wait<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(objc2::MainThreadMarker) -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::macos::on_main(app, move |mtm| {
        let _ = tx.send(f(mtm));
    });
    rx.recv_timeout(Duration::from_secs(2)).ok()
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
            #[cfg(not(target_os = "macos"))]
            use crate::desktop::recording::Panel;
            #[cfg(target_os = "macos")]
            use crate::macos::recording::Panel;
            let kind = match label {
                TARGET => Panel::Target,
                BAR => Panel::Bar,
                CANVAS => Panel::Canvas,
                // The rail is a keyless panel at the bars' level, as the bubble's controls are.
                _ => Panel::BubbleControls,
            };
            #[cfg(target_os = "macos")]
            {
                let w = window.clone();
                crate::macos::on_main(app, move |mtm| crate::macos::recording::setup_panel(&w, kind, mtm));
            }
            #[cfg(not(target_os = "macos"))]
            crate::desktop::recording::setup_panel(&window, kind);
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
        let _ = app;
        crate::desktop::recording::show(&window, frame, key, through);
    }
}

/// Shows the bar window at `frame` and, under it, the native glass for `pieces` in the same
/// turn of the main thread, so the blur and the shadow move with the bar; `through` lets the
/// clicks pass while the pointer is off the glass.
fn show_bar(app: &AppHandle, frame: LogicalRect, key: bool, through: bool, pieces: Vec<setup::GlassPiece>) {
    let Some(window) = panel(app, BAR) else { return };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| {
        crate::macos::recording::show(&window, frame, key, through, mtm);
        crate::macos::recording::backdrop_show(&window, frame, &pieces, mtm);
    });
    #[cfg(not(target_os = "macos"))]
    {
        crate::desktop::recording::show(&window, frame, key, through);
        crate::desktop::recording::backdrop_show(&window, frame, &pieces);
    }
}

/// Lets the bar window take the mouse, or pass it to what lies below.
fn bar_take_mouse(app: &AppHandle, takes: bool) {
    let Some(window) = app.get_webview_window(BAR) else { return };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::recording::take_mouse(&window, takes, mtm));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        crate::desktop::recording::take_mouse(&window, takes);
    }
}

/// Whether the pointer is over one of `rects` (global points). Where the system does not tell
/// where the pointer is (Wayland), the bar always takes the mouse.
fn pointer_over(rects: &[LogicalRect]) -> bool {
    #[cfg(target_os = "macos")]
    {
        let pointer = crate::macos::pointer();
        rects.iter().any(|r| r.contains(pointer))
    }
    #[cfg(not(target_os = "macos"))]
    crate::desktop::recording::pointer_over(rects)
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
        crate::desktop::recording::hide(&window);
        if label == BAR {
            crate::desktop::recording::backdrop_hide();
        }
    }
}

/// The window numbers of the app's own windows: those the file must not show, and those it
/// shows (the canvas and the bubble).
fn own_windows(app: &AppHandle) -> OwnWindows {
    #[cfg(target_os = "macos")]
    {
        use glimpsfy_capture::WindowId;
        let labels: Vec<(String, WebviewWindow)> = app.webview_windows().into_iter().collect();
        on_main_wait(app, move |mtm| {
            let mut own = OwnWindows::default();
            for (label, window) in &labels {
                if let Some(n) = crate::macos::recording::window_number(window, mtm) {
                    if label == CANVAS {
                        own.shown.push(WindowId(n));
                    } else {
                        own.hidden.push(WindowId(n));
                    }
                }
            }
            if let Some(n) = crate::macos::recording::bubble_window_number(mtm) {
                own.shown.push(WindowId(n));
            }
            if let Some(n) = crate::macos::recording::backdrop_window_number(mtm) {
                own.hidden.push(WindowId(n));
            }
            own
        })
        .unwrap_or_default()
    }
    #[cfg(not(target_os = "macos"))]
    crate::desktop::recording::own_windows(app)
}

fn visible_frame(app: &AppHandle, display: glimpsfy_capture::DisplayId) -> Option<LogicalRect> {
    #[cfg(target_os = "macos")]
    return on_main_wait(app, move |mtm| crate::macos::recording::visible_frame(display, mtm)).flatten();
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        crate::desktop::recording::visible_frame(display)
    }
}

// --------------------------------------------------------------------------------- events

fn emit_state(app: &AppHandle) {
    let now = state(app).recorder.state();
    let _ = app.emit(events::STATE, &now);
    tray(app, &now, None);
    // Linux keeps the bar off the recorded rectangle while the take records, and brings it back
    // while it pauses (`bar_off_the_take`).
    #[cfg(target_os = "linux")]
    if matches!(now, RecordingState::Recording { .. } | RecordingState::Paused { .. }) {
        let bar = lock(&state(app).inner).bar;
        if let Some((bar, pop, glass)) = bar {
            let _ = fit_bar(app, bar, pop, glass);
        }
    }
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
            let pointer = crate::desktop::pointer();
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
    let recording = app.state::<AppState>().settings.get().recording;
    let resolution = setup::resolution(&recording);
    let mut next = RecordSetup::new(mode, described, windows, request.rect, request.window_id, camera, microphone);
    next.settle(resolution);
    next.backdrop = take_backdrop(app, display.id);
    next.free_bytes = glimpsfy_record::files::free_bytes(&recording.recordings_folder).ok();
    lock(&rec.inner).setup = Some(next.clone());
    show_setup(app);
    emit_setup(app);
    sync_bubble(app);
    watch(app);
    Ok(next)
}

/// Where windows cannot be transparent (an X11 desktop without a compositor) the target is an
/// opaque slab over its display and the user would pick blind, so it draws a still of the display
/// under the setup instead, as the capture overlay draws its frozen frame. The still is taken with
/// the app's own windows out of the way and served through `glimpsfy-frame://`.
fn take_backdrop(app: &AppHandle, display: DisplayId) -> Option<String> {
    if crate::commands::windows::app_platform().capabilities.transparent_windows {
        return None;
    }
    let backend = app.state::<AppState>().backend.clone();
    #[cfg(not(target_os = "macos"))]
    let hidden = crate::desktop::hide_for_capture(app, &[]);
    #[cfg(not(target_os = "macos"))]
    hidden.settle();
    // The setup's backdrop stands for the live screen, so it shows the desktop as it is.
    let taken = backend.capture_display(display, &CaptureOptions::default());
    #[cfg(not(target_os = "macos"))]
    drop(hidden);
    match taken {
        Ok(frame) => Some(crate::frames::set_backdrop(display, Arc::new(frame))),
        Err(error) => {
            // The setup still opens, over the opaque page, rather than not at all.
            trace::log(format!("[glimpsfy] the recording target's still failed: {error}"));
            None
        }
    }
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
    crate::frames::clear_backdrop();
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
    #[cfg(target_os = "linux")]
    let (frame, side) = {
        let recording = matches!(now, RecordingState::Recording { .. });
        match inner.take.as_ref().filter(|_| recording) {
            Some(take) => match bar_off_the_take(app, take.rect, (frame, side), bar, pop, glass) {
                Some(place) => place,
                None => {
                    drop(inner);
                    hide(app, BAR);
                    return Ok(setup::BarLayout { popover_side: side, width: frame.width, height: frame.height });
                }
            },
            None => (frame, side),
        }
    };
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

/// Where the bar goes while a take records on Linux, which cannot keep any window out of the
/// file (an X11 grab and the ScreenCast portal show whatever is on screen): where `placed` puts
/// it when its glass stays off the recorded rectangle, else at the foot of another display that
/// the take does not record, else nowhere (`None`), and the take is paused, stopped and
/// discarded from the tray menu, the shortcuts and `glimpsfy --action`, while the tray shows its
/// time. A paused take records nothing, so the bar comes back while it pauses.
#[cfg(target_os = "linux")]
fn bar_off_the_take(
    app: &AppHandle,
    recorded: LogicalRect,
    placed: (LogicalRect, setup::PopoverSide),
    bar: Size,
    pop: Option<Size>,
    glass: BarGlass,
) -> Option<(LogicalRect, setup::PopoverSide)> {
    let covers = |(frame, side): (LogicalRect, setup::PopoverSide)| {
        setup::glass_pieces(frame, bar, pop, side, glass).iter().any(|piece| {
            let global = LogicalRect { x: frame.x + piece.rect.x, y: frame.y + piece.rect.y, ..piece.rect };
            global.intersection(&recorded).is_some()
        })
    };
    // Without a tray host (stock GNOME) the bar is the only control on screen, so it stays where
    // it is, and the file shows it, rather than leave a take nobody can see how to stop.
    if !covers(placed) || crate::desktop::tray_host_absent() {
        return Some(placed);
    }
    let displays = app.state::<AppState>().backend.displays().ok()?;
    displays.iter().filter(|display| display.frame.intersection(&recorded).is_none()).find_map(|display| {
        let described = setup::describe(display, visible_frame(app, display.id));
        let place = setup::place_bar(TargetMode::Display, display.frame, &described, bar, pop);
        (!covers(place)).then_some(place)
    })
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
            if let Err(error) = crate::desktop::recording::bubble_show(device.as_deref(), rect, round) {
                trace::log(format!("[glimpsfy] webcam bubble: {error}"));
                let _ = app.emit(events::BUBBLE, None::<BubbleState>);
            }
        }
        None => hide_bubble(app),
    }
    emit_bubble(app);
}

fn hide_bubble(app: &AppHandle) {
    hide(app, BUBBLE_CONTROLS);
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, crate::macos::recording::bubble_hide);
    #[cfg(not(target_os = "macos"))]
    crate::desktop::recording::bubble_hide();
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
    crate::desktop::recording::bubble_move(rect, round, true);
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
            // The outline follows the pointer at the display's pace while a window is picked, and
            // so do the spotlight and the brush ring where they hear the pointer from here.
            let fast = picking.is_some() || present::wants_fast_watch();
            pace = Duration::from_millis(if fast { 16 } else { 30 });
            if !open {
                state(&app).watching.store(false, Ordering::SeqCst);
                if controls {
                    hide(&app, BUBBLE_CONTROLS);
                }
                return;
            }
            #[cfg(target_os = "macos")]
            let (pointer, down) = (crate::macos::pointer(), crate::macos::recording::left_button_down());
            // Where the system does not tell where the pointer is, there is nothing to follow.
            #[cfg(not(target_os = "macos"))]
            let Some((pointer, down)) = crate::desktop::recording::pointer_and_button() else {
                continue;
            };

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
            // The same for the rail, and the canvas's own hold on the mouse while presenting.
            present::watch_tick(&app, pointer, down);

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

            // While drawing or the spotlight follows every move (`present`), this slower feed of
            // the same pointer would only send old points between them.
            let keys = keys && !present::feeds_pointer();
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
        limit_ms: settings.recording.time_limit_minutes.map(|m| m.clamp(1, 240) as u64 * 60_000),
        skip: false,
        #[cfg(not(target_os = "macos"))]
        going: false,
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
#[cfg(target_os = "macos")]
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
    if let Some(target) = app.get_webview_window(TARGET) {
        crate::macos::on_main(app, move |mtm| crate::macos::recording::pass_through(&target, mtm));
    }
    went_live(app, id);
}

/// The countdown ended: buffers belong to the take from now on. Where the system cannot keep the
/// countdown, the red frame, the pins, the toast and the popover out of the file, they leave the
/// screen first and the take is armed once the screen has lost them; armed at once, its first
/// frames showed the countdown's last digit and the frame. The bar stays, since it is how the
/// take is ended. The wait runs on a thread of its own: the main thread has to take the windows
/// off the screen meanwhile.
#[cfg(not(target_os = "macos"))]
fn go_live(app: &AppHandle, id: &RecordingId) {
    let rec = state(app);
    if rec.recorder.current().as_ref() != Some(id) {
        return;
    }
    // Return and the countdown's last tick can both end the countdown; one of them arms.
    match lock(&rec.inner).take.as_mut().filter(|t| t.id == *id) {
        Some(take) if !take.going => take.going = true,
        _ => return,
    }
    let concealed = app.get_webview_window(TARGET).is_some_and(|t| crate::desktop::recording::pass_through(&t));
    let hidden = crate::desktop::hide_for_capture(app, &[BAR, TARGET]);
    let (app, id) = (app.clone(), id.clone());
    std::thread::spawn(move || {
        if concealed || hidden.hid_any() {
            std::thread::sleep(crate::desktop::HIDE_SETTLE);
        }
        let rec = state(&app);
        let armed = if rec.recorder.current().as_ref() != Some(&id) {
            Err("the take ended meanwhile".to_string())
        } else {
            rec.recorder.go().map_err(|e| e.to_string())
        };
        match armed {
            Ok(()) => {
                lock(&rec.inner).hidden = Some(hidden);
                emit_state(&app);
                went_live(&app, &id);
            }
            Err(error) => {
                trace::log(format!("[glimpsfy] recording {}: {error}", id.0));
                // The stream may still be opening (Return during its start); `start` arms the
                // take again once it is open. The hidden windows come back with `hidden`.
                if let Some(take) = lock(&rec.inner).take.as_mut().filter(|t| t.id == id) {
                    take.going = false;
                }
            }
        }
    });
}

/// What follows the arming on every platform: the click and key watch, the bar, the ticks.
fn went_live(app: &AppHandle, id: &RecordingId) {
    let rec = state(app);
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
        crate::desktop::recording::watch_input(
            app,
            crate::desktop::recording::InputWatch {
                clicks,
                keys: keys.enabled,
                only_shortcuts: keys.only_shortcuts,
                on_click,
                on_keys,
            },
        );
    }
    // The bar page fits the control bar when its size differs from the last one it reported; when
    // it is the same the page stays silent, so the last sizes place the window here.
    let bar = lock(&rec.inner).bar;
    if let Some((bar, pop, glass)) = bar {
        let _ = fit_bar(app, bar, pop, glass);
    }
    ticks(app, id.clone());
    // Drawing and the spotlight, with their two keys, from now until the take ends.
    present::begin(app);
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

/// Sends `record://tick` four times a second while the take records or pauses, keeps the
/// tray's time current, and stops the take once when it reaches its time limit.
fn ticks(app: &AppHandle, id: RecordingId) {
    let app = app.clone();
    let mut limit_reached = false;
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        let rec = state(&app);
        let Some(progress) = rec.recorder.progress() else { return };
        if progress.id != id {
            return;
        }
        let (mic, system, limit_ms) = {
            let inner = lock(&rec.inner);
            let take = inner.take.as_ref();
            (take.is_some_and(|t| t.has_microphone), take.is_some_and(|t| t.has_system), take.and_then(|t| t.limit_ms))
        };
        let muted = matches!(
            rec.recorder.state(),
            RecordingState::Recording { microphone_muted: true, .. }
                | RecordingState::Paused { microphone_muted: true, .. }
        );
        let tick = events::RecordTick {
            id: progress.id.clone(),
            elapsed_ms: progress.elapsed_ms,
            microphone_level: (mic && !muted).then_some(progress.microphone_level),
            system_level: system.then_some(progress.system_level),
            file_bytes: progress.file_bytes,
            limit_ms,
        };
        let _ = app.emit(events::TICK, &tick);
        tray(&app, &rec.recorder.state(), Some(progress.elapsed_ms));
        // The elapsed time leaves pauses out, so a paused take never reaches its limit.
        if !limit_reached && limit_ms.is_some_and(|limit| progress.elapsed_ms >= limit) {
            limit_reached = true;
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                // Stop may have won the race; then there is nothing left to stop.
                if let Err(error) = stop_because(app, Some(StopReason::TimeLimit)).await {
                    trace::log(format!("[glimpsfy] the time limit did not stop the take: {}", error.message));
                }
            });
        }
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
    stop_because(app, None).await
}

/// [`stop`], saying why the app stopped the take on its own (the time limit), which the toast
/// then reports.
async fn stop_because(app: AppHandle, reason: Option<StopReason>) -> CommandResult<glimpsfy_record::RecordingResult> {
    let recorder = state(&app).recorder.clone();
    let _ending = Ending::new(&app);
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
    let outcome = tauri::async_runtime::spawn_blocking(move || match reason {
        Some(reason) => recorder.stop_because(reason),
        None => recorder.stop(),
    })
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
    let _ending = Ending::new(&app);
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
    // Drawing mode, the spotlight, the rail and their keys end with the take.
    present::end(app);
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, crate::macos::recording::unwatch_input);
    #[cfg(not(target_os = "macos"))]
    {
        crate::desktop::recording::unwatch_input();
        // Taken out under the lock and dropped after it: the drop shows windows again.
        let hidden = lock(&state(app).inner).hidden.take();
        drop(hidden);
    }
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
    crate::frames::clear_backdrop();
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
            let (app, ending) = (app.clone(), Ending::new(app));
            std::thread::spawn(move || {
                keep_take(&app, take.as_ref(), result, None);
                drop(ending);
            });
        }
        RecordingOutcome::Failed { code, message, partial, .. } => {
            let error = CommandError::new(error_code(&code), message);
            match partial {
                // What was written plays to its last fragment: it is kept, in the history too.
                Some(result) => {
                    let (app, ending) = (app.clone(), Ending::new(app));
                    std::thread::spawn(move || {
                        keep_take(&app, take.as_ref(), result, Some(error));
                        drop(ending);
                    });
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
    // Quitting: the file is complete and in the history, and a toast or the trim window would
    // only flash before the app goes.
    if lock(&state(app).quitting).is_some() {
        return;
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
            // A take the full disk stopped is saved all the same: one toast says both. A take
            // that reached its time limit says so, with its length.
            let kind = match result.stopped_because {
                Some(StopReason::DiskFull) => ToastKind::DiskFull,
                Some(StopReason::TimeLimit) => ToastKind::TimeUp,
                None => ToastKind::Saved,
            };
            let mut saved = Toast::new(kind);
            if kind == ToastKind::TimeUp {
                saved.duration_ms = Some(result.duration_ms);
                saved.recording_id = Some(result.id.0.clone());
            }
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
    let clock = |ms: u64| {
        let s = ms / 1000;
        if s >= 3600 {
            format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        } else {
            format!("{}:{:02}", s / 60, s % 60)
        }
    };
    #[cfg(target_os = "macos")]
    {
        let Some(icon) = crate::tray::icon(app) else { return };
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
    // The same states with the platform's own glyph, and the text where its tray can show one.
    #[cfg(not(target_os = "macos"))]
    {
        // Linux's menu starts, pauses and stops the take, so its items follow the state.
        #[cfg(target_os = "linux")]
        crate::tray::follow_recording(app, now);
        let (red, text) = match now {
            RecordingState::Recording { elapsed_ms, .. } => (true, Some(clock(elapsed.unwrap_or(*elapsed_ms)))),
            RecordingState::Paused { elapsed_ms, .. } => {
                (false, Some(format!("❚❚ {}", clock(elapsed.unwrap_or(*elapsed_ms)))))
            }
            RecordingState::Countdown { remaining_seconds, .. } => (false, Some(remaining_seconds.to_string())),
            RecordingState::Finishing { .. } => return,
            RecordingState::Idle => (false, None),
        };
        // The ticks come four times a second; both calls leave alone what is already right.
        crate::desktop::tray_look(app, red);
        crate::desktop::tray_text(app, text);
    }
}

/// The glyph in the kit's recording red, keeping its shape.
#[cfg(target_os = "macos")]
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
            (Action::DrawOnScreen | Action::Spotlight, _) => {
                present::toggle(&app, action);
                Ok(())
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

/// How long a quit waits for the take before the app exits anyway; finishing a file takes a
/// second or two, and what a later exit cuts off still plays to its last fragment.
const QUIT_WAIT: Duration = Duration::from_secs(30);

/// Asked when the app is about to exit (`RunEvent::ExitRequested`). A take that is counting down,
/// recording, paused or being finished ends first, as Stop ends it, so its file is complete and
/// reaches the history; the exit comes again once it has, or after [`QUIT_WAIT`]. Returns whether
/// this exit has to wait.
pub fn hold_exit(app: &AppHandle) -> bool {
    let Some(recording) = app.try_state::<RecordingApp>() else { return false };
    let busy = take_running(&recording);
    let mut quitting = lock(&recording.quitting);
    match *quitting {
        // The exit the quit thread asks for at the end, or another quit meanwhile, which waits
        // for the same take.
        Some(deadline) => busy && Instant::now() < deadline,
        None if !busy => false,
        None => {
            let deadline = Instant::now() + QUIT_WAIT;
            *quitting = Some(deadline);
            let app = app.clone();
            std::thread::spawn(move || end_for_quit(&app, deadline));
            true
        }
    }
}

fn take_running(recording: &RecordingApp) -> bool {
    recording.recorder.state() != RecordingState::Idle || recording.ending.load(Ordering::SeqCst) > 0
}

/// The quit thread: stops the take and exits once it has ended. A countdown that went live while
/// it was being cancelled is stopped by the next round.
fn end_for_quit(app: &AppHandle, deadline: Instant) {
    let mut step: Option<std::thread::JoinHandle<()>> = None;
    while take_running(state(app)) && Instant::now() < deadline {
        let now = state(app).recorder.state();
        let running = matches!(
            now,
            RecordingState::Countdown { .. } | RecordingState::Recording { .. } | RecordingState::Paused { .. }
        );
        // Finishing (a Stop, a Discard or a failure under way) ends on its own.
        if running && step.as_ref().is_none_or(|s| s.is_finished()) {
            let app = app.clone();
            step = Some(std::thread::spawn(move || {
                if let Err(error) = tauri::async_runtime::block_on(stop(app)) {
                    if error.code != ErrorCode::Cancelled {
                        trace::log(format!("[glimpsfy] stopping the recording to quit: {error}"));
                    }
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if take_running(state(app)) {
        trace::log("[glimpsfy] quit: the recording did not finish in time");
    }
    // This exit goes through whatever started meanwhile.
    *lock(&state(app).quitting) = Some(Instant::now());
    app.exit(0);
}

/// Whether the recording actions have their keys: always, now that recording works, and drawing
/// and the spotlight wherever the pointer can be followed.
pub fn handles(action: Action) -> bool {
    match action {
        Action::RecordScreen
        | Action::RecordDisplay
        | Action::PauseRecording
        | Action::MuteMicrophone
        | Action::DiscardRecording => true,
        Action::DrawOnScreen | Action::Spotlight => present::available(),
        _ => false,
    }
}

/// The recorder's state, for a bar or target page that opens or reloads during a take.
pub fn recorder_state(app: &AppHandle) -> RecordingState {
    state(app).recorder.state()
}

/// The canvas's configuration, for the canvas page's first read.
pub fn canvas_state(app: &AppHandle) -> Option<CanvasConfig> {
    lock(&state(app).inner).canvas.clone()
}
