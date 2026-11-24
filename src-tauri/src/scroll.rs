//! The scrolling capture (D27): what runs between ⌘↓ (or ⌥⇧⌘S) and Done. The overlay's selection
//! streams live while the user, or Glimpsfy, scrolls the page under it, and the stitcher
//! (`glimpsfy_capture::stitch`) grows one tall image from the frames.
//!
//! - **Start.** `capture_start_scroll` opens a stream of the selection alone at [`FPS`] frames a
//!   second, without the cursor and without Glimpsfy's windows, hides the overlays of the other
//!   displays and opens a hole in the selection's overlay (`macos::scroll::scroll_hole`), so the
//!   wheel and the trackpad reach the app under it while the keyboard stays with the overlay.
//! - **Stitching.** The stream's sink keeps the newest [`QUEUE`] frames and drops older ones, so a
//!   slow stitch never backs the stream up. The `glimpsfy-scroll` thread copies each frame into
//!   memory and hands it to the stitcher; a pause of [`PAUSE`] starts a new movement, which ⌘Z
//!   takes back alone. Changes reach the overlay as `capture://scroll`, at most every
//!   [`EMIT_EVERY`], each with a new address of the strip's preview (`frames.rs`).
//! - **Auto scroll.** With Accessibility, Space makes the thread post scroll events of
//!   [`STEP_SHARE`] of the band to the app of the window under the selection
//!   (`macos::scroll::post_scroll`). A step follows once the previous one's frames settled, and
//!   [`STILL_STEPS`] steps in a row that moved nothing stop it at the page's end.
//! - **End.** Done stitches the result and opens it in the editor (`render::open_frame`); the
//!   result reaching `stitch::MAX_HEIGHT` does the same by itself. Esc drops the result and the
//!   overlay goes back to its frozen selection. The result lives in memory alone until the
//!   editor's first output, and closing the capture any other way drops it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use glimpsfy_capture::stitch::{self, GapSide, Step, Stitcher};
use glimpsfy_capture::{
    AudioBuffer, CaptureError, CaptureStream, ColorInfo, DisplayId, Frame, LogicalPoint, LogicalRect, PermissionState,
    ScaleFactor, StreamConfig, StreamPixelFormat, StreamSink, StreamTarget, VideoFrame, WindowInfo,
};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::capture::{events, AfterSelection, CaptureId, ScrollGap, ScrollProgress, Selection};
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::commands::render::EditorImage;
use crate::{frames, session, trace, AppState};

/// The stream's rate: enough for a fast flick to leave overlapping frames.
const FPS: u32 = 30;
/// Frames waiting for the stitcher; an older one is dropped for a newer one.
const QUEUE: usize = 2;
/// A pause in the page's movement this long ends a movement, so ⌘Z takes back one scroll.
const PAUSE: Duration = Duration::from_millis(350);
/// The overlay hears of changes at most this often; the last one always arrives.
const EMIT_EVERY: Duration = Duration::from_millis(80);
/// The width of the strip's preview in pixels: the strip's 96 points at 2x.
const PREVIEW_WIDTH: u32 = 192;
/// An automatic step scrolls this share of the selection's moving band.
const STEP_SHARE: f64 = 0.45;
/// After a step, the page counts as settled once no frame came for this long.
const SETTLE: Duration = Duration::from_millis(160);
/// A step that brought no frame within this long moved nothing.
const STEP_WAIT: Duration = Duration::from_millis(700);
/// Steps in a row that moved nothing before automatic scrolling stops: the page's end.
const STILL_STEPS: u32 = 3;

/// The scrolling capture that runs; there is at most one.
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

fn active() -> MutexGuard<'static, Option<Active>> {
    ACTIVE.lock().unwrap_or_else(|p| p.into_inner())
}

struct Active {
    capture_id: CaptureId,
    display: DisplayId,
    /// The preview's token, which its address carries.
    token: String,
    shared: Arc<Shared>,
    stream: Box<dyn CaptureStream>,
    worker: Option<JoinHandle<()>>,
}

/// What the stream's sink, the worker and the commands share.
struct Shared {
    capture_id: CaptureId,
    display: DisplayId,
    token: String,
    color: ColorInfo,
    scale: ScaleFactor,
    /// The window and the point automatic scrolling posts to.
    target: Option<Target>,
    frames: Mutex<VecDeque<VideoFrame>>,
    arrived: Condvar,
    stop: AtomicBool,
    state: Mutex<State>,
}

/// Where automatic scrolling sends its events: the window under the selection's centre. Only
/// macOS posts scroll events, so elsewhere the fields go unread.
#[derive(Clone, Copy)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Target {
    pid: i32,
    window: u32,
    /// The selection's centre in global points.
    at: LogicalPoint,
}

struct State {
    /// Made from the first frame, whose size the stream chose.
    stitcher: Option<Stitcher>,
    auto: bool,
    ended: bool,
    /// The preview's BMP and its version, which a new preview raises.
    preview: Option<Arc<Vec<u8>>>,
    version: u64,
    /// A change the overlay has not heard of yet.
    dirty: bool,
    last_emit: Option<Instant>,
    /// When the page last moved, for the pause that ends a movement.
    last_change: Option<Instant>,
    step: Option<AutoStep>,
    still_steps: u32,
}

/// The automatic step that was posted last.
struct AutoStep {
    at: Instant,
    moved: bool,
    last_frame: Instant,
}

/// The stream's sink: keeps the newest frames for the worker.
struct Sink(Arc<Shared>);

impl StreamSink for Sink {
    fn video(&self, frame: VideoFrame) {
        let mut frames = self.0.frames.lock().unwrap_or_else(|p| p.into_inner());
        if frames.len() >= QUEUE {
            frames.pop_front();
        }
        frames.push_back(frame);
        self.0.arrived.notify_one();
    }

    fn audio(&self, _buffer: AudioBuffer) {}

    fn stopped(&self, error: Option<CaptureError>) {
        if let Some(error) = error {
            trace::log(format!("[glimpsfy] scrolling capture's stream ended: {error}"));
        }
    }
}

// ------------------------------------------------------------------------------- commands

/// Starts the scrolling capture of `selection`.
pub fn start(app: &AppHandle, selection: Selection) -> CommandResult<ScrollProgress> {
    if !cfg!(target_os = "macos") {
        return Err(CommandError::new(ErrorCode::NotImplemented, "scrolling capture works on macOS only"));
    }
    if active().is_some() {
        return Err(CommandError::new(ErrorCode::Busy, "a scrolling capture runs"));
    }
    let state = app.state::<AppState>();
    let session = state
        .sessions
        .get(selection.capture_id)
        .ok_or_else(|| CommandError::not_found(format!("capture #{}", selection.capture_id.0)))?;
    let entry = session
        .display(selection.display_id)
        .ok_or_else(|| CommandError::not_found(format!("display {}", selection.display_id.0)))?;
    let bounds = LogicalRect::new(0.0, 0.0, entry.display.frame.width, entry.display.frame.height);
    let scale = entry.display.scale.0.max(1.0);
    let rect = selection
        .rect
        .intersection(&bounds)
        .map(|r| on_pixels(r, scale))
        .filter(|r| r.width >= 8.0 && r.height >= 8.0)
        .ok_or_else(|| CommandError::invalid("the selection is too small to scroll"))?;
    let at = LogicalPoint {
        x: entry.display.frame.x + rect.x + rect.width / 2.0,
        y: entry.display.frame.y + rect.y + rect.height / 2.0,
    };
    let shared = Arc::new(Shared {
        capture_id: session.id,
        display: entry.display.id,
        token: session::token(),
        color: entry.frame.color.clone(),
        scale: entry.display.scale,
        target: target(&session.all_windows, at),
        frames: Mutex::new(VecDeque::new()),
        arrived: Condvar::new(),
        stop: AtomicBool::new(false),
        state: Mutex::new(State {
            stitcher: None,
            auto: false,
            ended: false,
            preview: None,
            version: 0,
            dirty: false,
            last_emit: None,
            last_change: None,
            step: None,
            still_steps: 0,
        }),
    });
    let config = StreamConfig {
        target: StreamTarget::Area { display: entry.display.id, rect },
        fps: FPS,
        output_size: None,
        size_divisor: 1,
        shows_cursor: false,
        capture_system_audio: false,
        microphone: None,
        // The backend leaves out every window of Glimpsfy, the overlay among them.
        excluded_windows: Vec::new(),
        shown_windows: Vec::new(),
        color_space: entry.frame.color.space,
        pixel_format: StreamPixelFormat::Bgra,
        queue_depth: 4,
        // As the still the selection was made on, so a banner never lands in the stitched image.
        clean_desktop: state.settings.get().output.clean_desktop,
    };
    let mut stream = state
        .backend
        .open_stream(config, Box::new(Sink(shared.clone())))
        .map_err(|e| CommandError::new(ErrorCode::Capture, e.to_string()))?;
    stream.start().map_err(|e| CommandError::new(ErrorCode::Capture, e.to_string()))?;
    let worker = {
        let app = app.clone();
        let shared = shared.clone();
        thread::Builder::new()
            .name("glimpsfy-scroll".into())
            .spawn(move || work(&app, &shared))
            .map_err(|e| CommandError::internal(e.to_string()))?
    };
    let progress = progress_of(&shared, &lock(&shared.state));
    *active() = Some(Active {
        capture_id: session.id,
        display: entry.display.id,
        token: shared.token.clone(),
        shared,
        stream,
        worker: Some(worker),
    });
    for other in session.displays.iter().filter(|d| d.display.id != entry.display.id) {
        session::hide_overlay(app, other.display.id);
    }
    hole(app, entry.display.id, Some(rect));
    trace::log(format!(
        "[glimpsfy] capture #{}: scrolling capture of {:.0}×{:.0} pt started",
        session.id.0, rect.width, rect.height
    ));
    Ok(progress)
}

/// Turns automatic scrolling on or off.
pub fn set_auto(app: &AppHandle, on: bool) -> CommandResult<ScrollProgress> {
    let shared = running()?;
    if on && !auto_allowed() {
        return Err(CommandError::new(ErrorCode::PermissionDenied, "automatic scrolling needs accessibility"));
    }
    if on && shared.target.is_none() {
        return Err(CommandError::not_found("a window under the selection"));
    }
    let mut state = lock(&shared.state);
    state.auto = on;
    state.ended = false;
    state.step = None;
    state.still_steps = 0;
    let progress = progress_of(&shared, &state);
    drop(state);
    shared.arrived.notify_one();
    let _ = app.emit(events::SCROLL, &progress);
    Ok(progress)
}

/// The overlay's own parts that lie over the selection take the pointer back from the hole, so a
/// pop or a bar that found no room outside the selection stays clickable. A page that is late
/// for an ended capture changes nothing.
pub fn cover(app: &AppHandle, covers: Vec<LogicalRect>) {
    let Ok(shared) = running() else { return };
    #[cfg(target_os = "macos")]
    {
        let Some(window) = app.get_webview_window(&crate::windows::overlay_label(shared.display)) else {
            return;
        };
        crate::macos::on_main(app, move |mtm| crate::macos::scroll::scroll_cover(&window, &covers, mtm));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, shared, covers);
}

/// Takes back the last movement.
pub fn undo(app: &AppHandle) -> CommandResult<ScrollProgress> {
    let shared = running()?;
    let mut state = lock(&shared.state);
    let undone = state.stitcher.as_mut().is_some_and(Stitcher::undo);
    if undone {
        // A step back stops the page's own scrolling, which would only bring the rows back.
        state.auto = false;
        state.step = None;
        refresh_preview(&mut state);
    }
    let progress = progress_of(&shared, &state);
    drop(state);
    let _ = app.emit(events::SCROLL, &progress);
    Ok(progress)
}

/// Ends the scrolling capture with its result, in the editor.
pub fn finish(app: &AppHandle) -> CommandResult<EditorImage> {
    let Some(active) = take() else {
        return Err(CommandError::not_found("a scrolling capture"));
    };
    let (capture_id, display) = (active.capture_id, active.display);
    let shared = stop(app, active);
    let result = lock(&shared.state).stitcher.as_ref().map(Stitcher::finish);
    let frame = match result {
        Some(Ok(frame)) => Arc::new(frame),
        Some(Err(error)) => return Err(CommandError::new(ErrorCode::Capture, error.to_string())),
        None => return Err(CommandError::invalid("nothing was captured yet")),
    };
    let session = app
        .state::<AppState>()
        .sessions
        .get(capture_id)
        .ok_or_else(|| CommandError::not_found(format!("capture #{}", capture_id.0)))?;
    // The drawings on the frozen selection belong to it, not to the scrolled result.
    let mut document = glimpsfy_annotate::Document::new(
        glimpsfy_annotate::ImageSize { width: frame.size.width, height: frame.size.height },
        frame.scale.0,
    );
    document.beautify = session.beautify();
    let document = document.to_json().map_err(|e| CommandError::internal(e.to_string()))?;
    let language = app.state::<AppState>().settings.get().general.language;
    let name = crate::i18n::text(language, "scroll.name", &[]);
    trace::log(format!(
        "[glimpsfy] capture #{}: scrolling capture finished at {}×{} px",
        capture_id.0, frame.size.width, frame.size.height
    ));
    Ok(crate::commands::render::open_frame(app, &session, display, frame, document, Some(name)))
}

/// Drops the result and goes back to the frozen selection.
pub fn cancel(app: &AppHandle) -> bool {
    let Some(active) = take() else {
        return false;
    };
    let capture_id = active.capture_id;
    stop(app, active);
    let session = app.state::<AppState>().sessions.get(capture_id);
    // A capture opened for the scrolling capture alone has no selection to go back to.
    match session {
        Some(session) if session.after == AfterSelection::Scroll => {
            session::close(app, capture_id, crate::commands::capture::CloseReason::Cancelled);
        }
        Some(_) => session::reshow(app, capture_id),
        None => {}
    }
    true
}

/// Capture `id` closed: its scrolling capture, if one runs, stops without a result.
pub fn closed(app: &AppHandle, id: CaptureId) {
    let taken = {
        let mut active = active();
        match active.as_ref() {
            Some(a) if a.capture_id == id => active.take(),
            _ => None,
        }
    };
    if let Some(active) = taken {
        stop(app, active);
    }
}

/// The preview of the scrolling capture on `display`, for `frames.rs`.
pub fn preview_bmp(display: DisplayId, token: &str) -> Option<Vec<u8>> {
    let shared = {
        let active = active();
        let active = active.as_ref().filter(|a| a.display == display && a.token == token)?;
        active.shared.clone()
    };
    let preview = lock(&shared.state).preview.clone()?;
    Some(preview.as_ref().clone())
}

// ------------------------------------------------------------------------------- internals

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn running() -> CommandResult<Arc<Shared>> {
    active().as_ref().map(|a| a.shared.clone()).ok_or_else(|| CommandError::not_found("a scrolling capture"))
}

fn take() -> Option<Active> {
    active().take()
}

/// Stops the stream and the worker, and closes the overlay's hole.
fn stop(app: &AppHandle, mut active: Active) -> Arc<Shared> {
    active.shared.stop.store(true, Ordering::SeqCst);
    active.shared.arrived.notify_all();
    if let Err(error) = active.stream.stop() {
        trace::log(format!("[glimpsfy] scrolling capture's stream did not stop: {error}"));
    }
    if let Some(worker) = active.worker.take() {
        // The worker finishes the capture itself at the limit, from a thread of its own, so
        // this is never the worker waiting for itself.
        if worker.thread().id() != thread::current().id() {
            let _ = worker.join();
        }
    }
    lock(&active.shared.frames).clear();
    hole(app, active.display, None);
    active.shared.clone()
}

/// `rect` with its edges on the display's pixels. The stitcher matches rows exactly, and a stream
/// of a rect off the pixel grid hands over rows scaled between pixels, which differ from the same
/// rows one scroll later.
fn on_pixels(rect: LogicalRect, scale: f64) -> LogicalRect {
    let snap = |v: f64| (v * scale).round() / scale;
    let (x, y) = (snap(rect.x), snap(rect.y));
    LogicalRect::new(x, y, snap(rect.x + rect.width) - x, snap(rect.y + rect.height) - y)
}

fn auto_allowed() -> bool {
    crate::permissions::accessibility() == PermissionState::Granted
}

/// The frontmost ordinary window of another app under `at`, global points.
fn target(windows: &[WindowInfo], at: LogicalPoint) -> Option<Target> {
    let own = std::process::id() as i32;
    windows
        .iter()
        .filter(|w| w.pid != own && w.layer == 0 && w.alpha > 0.0)
        .find(|w| w.bounds.contains(at))
        .map(|w| Target { pid: w.pid, window: w.id.0, at })
}

fn hole(app: &AppHandle, display: DisplayId, rect: Option<LogicalRect>) {
    #[cfg(target_os = "macos")]
    {
        let Some(window) = app.get_webview_window(&crate::windows::overlay_label(display)) else {
            return;
        };
        crate::macos::on_main(app, move |mtm| crate::macos::scroll::scroll_hole(&window, rect, mtm));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, display, rect);
}

fn progress_of(shared: &Shared, state: &State) -> ScrollProgress {
    let p = state.stitcher.as_ref().map(Stitcher::progress).unwrap_or_default();
    ScrollProgress {
        capture_id: shared.capture_id,
        display_id: shared.display,
        width: p.width,
        height: p.height,
        frames: p.frames,
        header: p.header,
        header_found: p.header_found,
        footer: p.footer,
        footer_found: p.footer_found,
        gaps: p.gaps,
        view: p.view.map(|(first, height)| [first, height]),
        gap: p.gap.map(|side| match side {
            GapSide::Above => ScrollGap::Above,
            GapSide::Below => ScrollGap::Below,
        }),
        bytes: p.bytes,
        limit: stitch::MAX_HEIGHT,
        auto: state.auto,
        auto_allowed: auto_allowed() && shared.target.is_some(),
        ended: state.ended,
        preview_url: state.preview.is_some().then(|| frames::scroll_url(shared.display, &shared.token, state.version)),
    }
}

fn refresh_preview(state: &mut State) {
    let Some(stitcher) = state.stitcher.as_ref() else {
        return;
    };
    state.preview = Some(Arc::new(stitcher.preview().to_bmp()));
    state.version += 1;
    state.dirty = true;
}

/// The `glimpsfy-scroll` thread: stitches the frames and runs automatic scrolling.
fn work(app: &AppHandle, shared: &Arc<Shared>) {
    loop {
        let next = {
            let mut frames = lock(&shared.frames);
            if frames.is_empty() && !shared.stop.load(Ordering::SeqCst) {
                frames = shared
                    .arrived
                    .wait_timeout(frames, Duration::from_millis(40))
                    .map(|(f, _)| f)
                    .unwrap_or_else(|p| p.into_inner().0);
            }
            frames.pop_front()
        };
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        if let Some(frame) = next {
            if stitch_one(shared, &frame) {
                // The limit: the capture finishes by itself, from a thread that may wait for this
                // one to end.
                let app = app.clone();
                thread::spawn(move || {
                    if let Err(error) = finish(&app) {
                        trace::log(format!("[glimpsfy] scrolling capture did not finish at its limit: {error}"));
                    }
                });
                return;
            }
        }
        step_auto(shared);
        let mut state = lock(&shared.state);
        let due = state.dirty && state.last_emit.is_none_or(|t| t.elapsed() >= EMIT_EVERY);
        if due {
            state.dirty = false;
            state.last_emit = Some(Instant::now());
            let progress = progress_of(shared, &state);
            drop(state);
            let _ = app.emit(events::SCROLL, &progress);
        }
    }
}

/// Hands one frame to the stitcher; `true` once the result reached its limit.
fn stitch_one(shared: &Shared, video: &VideoFrame) -> bool {
    let mut frame: Frame = match glimpsfy_capture::cpu_frame(video) {
        Ok(frame) => frame,
        Err(error) => {
            trace::log(format!("[glimpsfy] a frame of the scrolling capture could not be read: {error}"));
            return false;
        }
    };
    frame.scale = shared.scale;
    let now = Instant::now();
    let mut state = lock(&shared.state);
    let state = &mut *state;
    let stitcher = state.stitcher.get_or_insert_with(|| {
        Stitcher::new(frame.size, PREVIEW_WIDTH.min(frame.size.width), shared.color.clone(), shared.scale)
    });
    if state.last_change.is_none_or(|t| now.duration_since(t) >= PAUSE) {
        stitcher.begin_movement();
    }
    let step = stitcher.push(&frame);
    match step {
        Step::First | Step::Moved | Step::Opened | Step::Joined => {
            state.last_change = Some(now);
            if let Some(auto) = state.step.as_mut() {
                auto.moved = true;
                auto.last_frame = now;
            }
            refresh_preview(state);
            false
        }
        Step::Full => true,
        Step::Still | Step::Ignored => false,
    }
}

/// Posts the next automatic step when the last one settled, and stops at the page's end.
fn step_auto(shared: &Shared) {
    let Some(target) = shared.target else {
        return;
    };
    let mut guard = lock(&shared.state);
    let state = &mut *guard;
    if !state.auto {
        return;
    }
    let Some(stitcher) = state.stitcher.as_mut() else {
        return;
    };
    let band = stitcher.progress().view.map_or(stitcher.size().height, |(_, h)| h);
    let now = Instant::now();
    let due = match &state.step {
        None => true,
        Some(step) if step.moved => now.duration_since(step.last_frame) >= SETTLE,
        Some(step) => now.duration_since(step.at) >= STEP_WAIT,
    };
    if !due {
        return;
    }
    if state.step.as_ref().is_some_and(|s| !s.moved) {
        state.still_steps += 1;
        if state.still_steps >= STILL_STEPS {
            state.auto = false;
            state.ended = true;
            state.step = None;
            state.dirty = true;
            return;
        }
    } else if state.step.is_some() {
        state.still_steps = 0;
    }
    stitcher.begin_movement();
    let points = (band as f64 / shared.scale.0.max(1.0) * STEP_SHARE).round().max(1.0) as i32;
    #[cfg(target_os = "macos")]
    let posted = crate::macos::scroll::post_scroll(target.pid, Some(target.window), target.at, -points);
    #[cfg(not(target_os = "macos"))]
    let posted = {
        let _ = (target, points);
        false
    };
    if !posted {
        // The permission went while it ran.
        state.auto = false;
        state.step = None;
        state.dirty = true;
        return;
    }
    state.step = Some(AutoStep { at: now, moved: false, last_frame: now });
}
