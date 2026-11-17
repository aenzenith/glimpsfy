//! The capture flow, from the shortcut to the closed overlay.
//!
//! 1. `start` (shortcut, menu, popover, `capture_start`) waits out the one-shot delay when one is
//!    set (`session/delay.rs`), then runs the mode: the overlay modes (area, window, copy text)
//!    open the overlay; display, all displays and repeat last area capture and deliver at once.
//! 2. An overlay capture checks the permission, orders the warm overlay windows in invisibly
//!    (`windows::prewarm_overlays`) and captures every display on a worker thread. The frames
//!    become a `CaptureSession`, and each overlay window gets `capture://started` with its
//!    display's frame address (`frames.rs`), the windows to snap to and what to do after a
//!    selection.
//! 3. Each page fetches and draws its frame while still invisible, then calls `overlay_ready`,
//!    which shows its window (`ready`); its first painted frame comes back as `overlay_painted`.
//!    A page that cannot load its frame calls `overlay_failed` at once: its page is reloaded
//!    once, and a second failure drops that display or closes the capture with a toast.
//! 4. Copy, Save or Close end the session (`close`), which hides every overlay window.
//!
//! One session exists at a time. A trigger while one is open, starting or counting down does
//! nothing: restarting would throw away a selection (and drawings) without a word, and the new
//! frame would be the same, since the overlay covers the screen. The exception is a session
//! that is stuck (no overlay ready 1.5 s after it opened): the trigger closes it and starts anew.

pub mod delay;

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use glimpsfy_capture::{
    selection_to_frame, CaptureError, CaptureOptions, ColorInfo, Display, DisplayId, Frame, LogicalPoint, LogicalRect,
    PermissionState, PhysicalSize, PixelFormat, ScaleFactor, WindowCaptureOptions, WindowId, WindowInfo,
    WindowListOptions,
};
use tauri::{AppHandle, Emitter, EventTarget, Manager};

use crate::commands::capture::{
    AfterSelection, CaptureClosed, CaptureId, CaptureMode, CloseReason, OverlayFrame, OverlayWindow, PaintReport,
    PickedWindow, Selection,
};
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::frames::{self, FrameFormat};
use crate::output::{Origin, OutputAction};
use crate::settings::{self, WindowBackground};
use crate::trace::{self, Mark, Trace};
use crate::{dev, events, output, permissions, windows, AppState};

/// A capture whose overlay never reported ready is closed after this, so a dead page cannot
/// leave an invisible session that blocks the next shortcut.
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// A second trigger closes an open capture none of whose overlays is ready after this long.
const STUCK_AFTER: Duration = Duration::from_millis(1500);
/// A capture thread that has not finished after this long no longer blocks a new capture.
const START_STUCK_AFTER: Duration = Duration::from_secs(10);
/// A capture from the menu waits for the menu to fade, so it is not in the frame.
const MENU_FADE: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct Sessions {
    current: Mutex<Option<Arc<CaptureSession>>>,
    /// The capture being taken (its generation and start), before it has a session.
    starting: Mutex<Option<(u64, Instant)>>,
    generation: AtomicU64,
    next_id: AtomicU64,
    /// The last selection that was copied or saved, for "Repeat last area".
    last_area: Mutex<Option<LastArea>>,
    /// What stays of the last closed capture, for an image delivered after it closed.
    recent: Mutex<Option<Recent>>,
}

impl Sessions {
    pub fn current(&self) -> Option<Arc<CaptureSession>> {
        self.current.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn get(&self, id: CaptureId) -> Option<Arc<CaptureSession>> {
        self.current().filter(|s| s.id == id)
    }

    fn take(&self, id: CaptureId) -> Option<Arc<CaptureSession>> {
        let mut current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if current.as_ref().is_some_and(|s| s.id == id) {
            current.take()
        } else {
            None
        }
    }

    pub fn is_idle(&self) -> bool {
        self.starting.lock().unwrap_or_else(|p| p.into_inner()).is_none() && self.current().is_none()
    }

    fn next_id(&self) -> CaptureId {
        CaptureId(self.next_id.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

/// Where a session's frames came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    ScreenCaptureKit,
    /// Generated frames (`GLIMPSFY_SYNTHETIC`), for measuring without the permission.
    Synthetic,
}

/// The frontmost window at capture time, for `{app}` and `{title}` and the history.
#[derive(Clone, Debug, Default)]
pub struct FrontWindow {
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    pub title: Option<String>,
}

/// An area to capture again: the same rectangle on the same display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LastArea {
    pub display: DisplayId,
    /// The display's frame at the time (global, logical); the area is repeated only while the
    /// display has the same size, so it still means the same pixels.
    pub display_frame: LogicalRect,
    /// Display-local, logical.
    pub rect: LogicalRect,
}

/// What stays of a closed capture until the next one closes.
struct Recent {
    id: CaptureId,
    mode: CaptureMode,
    displays: Vec<Display>,
    front: FrontWindow,
    taken_at: SystemTime,
}

/// A window the user clicked in window mode (or with Space), captured on its own.
pub struct Picked {
    pub window_id: WindowId,
    pub display: DisplayId,
    /// The window's rectangle, display-local and logical; it may reach past the display.
    pub rect: LogicalRect,
    pub frame: Arc<Frame>,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub title: Option<String>,
}

pub struct CaptureSession {
    pub id: CaptureId,
    pub mode: CaptureMode,
    pub trace: Trace,
    pub source: Source,
    pub format: FrameFormat,
    pub displays: Vec<SessionDisplay>,
    /// The display under the pointer when the capture started; its overlay takes the keyboard.
    pub pointer_display: Option<DisplayId>,
    pub pointer: LogicalPoint,
    /// The frontmost app at capture time, for the `{app}` file name token.
    pub front_app: Option<String>,
    pub front: FrontWindow,
    /// When the frames were taken.
    pub taken_at: SystemTime,
    /// When the session was published.
    pub opened: Instant,
    /// What the overlay does after a selection.
    pub after: AfterSelection,
    /// Every window on screen at capture time, frontmost first.
    pub all_windows: Vec<WindowInfo>,
    picked: Mutex<Option<Arc<Picked>>>,
    /// The display whose overlay holds the selection; its overlay keeps the keyboard wherever
    /// the pointer goes.
    holder: Mutex<Option<DisplayId>>,
    /// Closed by the measurement loop once painted (`GLIMPSFY_AUTOCAPTURE`).
    pub auto: bool,
}

pub struct SessionDisplay {
    pub display: Display,
    pub frame: Arc<Frame>,
    pub token: String,
    pub windows: Vec<OverlayWindow>,
    ready: AtomicBool,
    painted: AtomicBool,
    /// The display was disconnected, or its page failed twice; the capture goes on without it.
    removed: AtomicBool,
    failures: AtomicU32,
}

impl SessionDisplay {
    fn live(&self) -> bool {
        !self.removed.load(Ordering::SeqCst)
    }
}

impl CaptureSession {
    pub fn display(&self, id: DisplayId) -> Option<&SessionDisplay> {
        self.displays.iter().find(|d| d.display.id == id)
    }

    pub fn overlay_frame(&self, id: DisplayId) -> Option<OverlayFrame> {
        let entry = self.display(id).filter(|d| d.live())?;
        let pointer = (self.pointer_display == Some(id)).then_some(LogicalPoint {
            x: self.pointer.x - entry.display.frame.x,
            y: self.pointer.y - entry.display.frame.y,
        });
        Some(OverlayFrame {
            capture_id: self.id,
            display: entry.display.clone(),
            frame_url: frames::frame_url(id, &entry.token),
            frame_size: entry.frame.size,
            format: self.format,
            color_space: entry.frame.color.space,
            windows: entry.windows.clone(),
            pointer,
            mode: self.mode,
            after_selection: self.after,
        })
    }

    /// `display` took the selection (`held`) or lost it. Only the holder's own loss clears it: a
    /// display drops its selection after another one took over, and that must not undo the take.
    pub fn hold(&self, display: DisplayId, held: bool) {
        let mut holder = self.holder.lock().unwrap_or_else(|p| p.into_inner());
        if held {
            *holder = Some(display);
        } else if *holder == Some(display) {
            *holder = None;
        }
    }

    /// The display that holds the selection, while it is still part of the capture.
    pub fn holder(&self) -> Option<DisplayId> {
        let holder = *self.holder.lock().unwrap_or_else(|p| p.into_inner());
        holder.filter(|id| self.display(*id).is_some_and(SessionDisplay::live))
    }

    /// The origin of an image taken from `display` of this capture.
    pub fn origin(&self, display: DisplayId) -> Origin {
        Origin {
            capture_id: self.id,
            mode: self.mode,
            display: self.display(display).map(|d| d.display.clone()),
            app_name: self.front.app_name.clone(),
            bundle_id: self.front.bundle_id.clone(),
            window_title: self.front.title.clone(),
            taken_at: self.taken_at,
        }
    }

    /// The picked window, when `selection` is still its rectangle (within a point, whole or
    /// clipped to its display).
    pub fn picked_for(&self, selection: &Selection) -> Option<Arc<Picked>> {
        let picked = self.picked.lock().unwrap_or_else(|p| p.into_inner()).clone()?;
        if picked.display != selection.display_id {
            return None;
        }
        let entry = self.display(picked.display)?;
        let bounds = LogicalRect::new(0.0, 0.0, entry.display.frame.width, entry.display.frame.height);
        let clipped = picked.rect.intersection(&bounds)?;
        let near = |a: &LogicalRect, b: &LogicalRect| {
            (a.x - b.x).abs() <= 1.0
                && (a.y - b.y).abs() <= 1.0
                && (a.right() - b.right()).abs() <= 1.0
                && (a.bottom() - b.bottom()).abs() <= 1.0
        };
        (near(&selection.rect, &picked.rect) || near(&selection.rect, &clipped)).then_some(picked)
    }

    /// No overlay is ready a while after the session opened: its pages are not answering.
    fn is_stuck(&self) -> bool {
        self.opened.elapsed() > STUCK_AFTER && !self.displays.iter().any(|d| d.ready.load(Ordering::SeqCst))
    }

    fn all_painted(&self) -> bool {
        self.displays.iter().filter(|d| d.live()).all(|d| d.painted.load(Ordering::SeqCst))
    }
}

/// What the overlay does after a selection in `mode`, by Settings › General › After a selection.
fn after_for(mode: CaptureMode, setting: settings::AfterSelection) -> AfterSelection {
    if mode == CaptureMode::CopyText {
        return AfterSelection::CopyText;
    }
    match setting {
        settings::AfterSelection::Annotate => AfterSelection::Annotate,
        settings::AfterSelection::CopyAndClose => AfterSelection::Copy,
        settings::AfterSelection::SaveAndClose => AfterSelection::Save,
    }
}

/// What a capture without the overlay does with its image: copy when the settings say copy,
/// save otherwise (there is no overlay to annotate in, so "Annotate" saves).
fn direct_action(setting: settings::AfterSelection) -> OutputAction {
    match setting {
        settings::AfterSelection::CopyAndClose => OutputAction::Copy,
        settings::AfterSelection::Annotate | settings::AfterSelection::SaveAndClose => OutputAction::Save,
    }
}

// ------------------------------------------------------------------------------------- start

/// Starts a capture in `mode`; `at` is when the shortcut or the click reached the app. A delay
/// that is set (or the mode "area after a delay") counts down first.
pub fn start(app: &AppHandle, mode: CaptureMode, at: Instant) -> CommandResult<()> {
    if delay::is_running() {
        trace::log(format!("[glimpsfy] {mode:?} ignored: a delayed capture is counting down"));
        return Ok(());
    }
    if busy(app) {
        trace::log(format!("[glimpsfy] {mode:?} ignored: a capture is open"));
        return Ok(());
    }
    let state = app.state::<AppState>();
    let seconds = match mode {
        CaptureMode::AreaDelayed => {
            delay::take_pending();
            state.settings.get().general.delay_seconds.clamp(1, 60)
        }
        _ => delay::take_pending(),
    };
    if seconds > 0 {
        let mode = if mode == CaptureMode::AreaDelayed { CaptureMode::Area } else { mode };
        return delay::begin(app, mode, seconds);
    }
    start_now(app, mode, at)
}

/// Starts a capture from the tray menu, once the menu has faded.
pub fn start_from_menu(app: &AppHandle, mode: CaptureMode) {
    let app = app.clone();
    thread::spawn(move || {
        thread::sleep(MENU_FADE);
        if let Err(error) = start(&app, mode, Instant::now()) {
            trace::log(format!("[glimpsfy] capture from the menu: {error}"));
        }
    });
}

/// Runs `mode` now, without a delay.
pub fn start_now(app: &AppHandle, mode: CaptureMode, at: Instant) -> CommandResult<()> {
    match mode {
        CaptureMode::Area | CaptureMode::AreaDelayed => start_overlay(app, CaptureMode::Area, at, false),
        CaptureMode::Window | CaptureMode::CopyText => start_overlay(app, mode, at, false),
        CaptureMode::Display => start_direct(app, Direct::Display, at),
        CaptureMode::AllDisplays => start_direct(app, Direct::AllDisplays, at),
        CaptureMode::RepeatLastArea => {
            let last = *app.state::<AppState>().sessions.last_area.lock().unwrap_or_else(|p| p.into_inner());
            match last {
                Some(area) => start_direct(app, Direct::Repeat(area), at),
                None => start_overlay(app, CaptureMode::Area, at, false),
            }
        }
    }
}

/// The area capture of the measurement loop and the old entry point.
pub fn start_area(app: &AppHandle, at: Instant, auto: bool) -> CommandResult<()> {
    start_overlay(app, CaptureMode::Area, at, auto)
}

/// Whether a capture is open or being taken, closing an open one that is stuck.
fn busy(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    if let Some(current) = state.sessions.current() {
        if !current.is_stuck() {
            return true;
        }
        trace::log(format!(
            "[glimpsfy] capture #{} closed: no overlay is ready, and a new capture starts",
            current.id.0
        ));
        close(app, current.id, CloseReason::Failed);
    }
    let starting = *state.sessions.starting.lock().unwrap_or_else(|p| p.into_inner());
    starting.is_some_and(|(_, since)| since.elapsed() < START_STUCK_AFTER)
}

/// Claims the right to take a capture; `None` when another one is being taken.
fn claim(app: &AppHandle) -> Option<u64> {
    if busy(app) {
        return None;
    }
    let state = app.state::<AppState>();
    let mut starting = state.sessions.starting.lock().unwrap_or_else(|p| p.into_inner());
    if starting.is_some_and(|(_, since)| since.elapsed() < START_STUCK_AFTER) {
        return None;
    }
    if starting.is_some() {
        trace::log("[glimpsfy] a capture that never finished no longer blocks new ones");
    }
    let generation = state.sessions.generation.fetch_add(1, Ordering::SeqCst) + 1;
    *starting = Some((generation, Instant::now()));
    Some(generation)
}

fn release(app: &AppHandle, generation: u64) {
    let state = app.state::<AppState>();
    let mut starting = state.sessions.starting.lock().unwrap_or_else(|p| p.into_inner());
    if starting.is_some_and(|(g, _)| g == generation) {
        *starting = None;
    }
}

fn is_claimed(app: &AppHandle, generation: u64) -> bool {
    let state = app.state::<AppState>();
    let starting = state.sessions.starting.lock().unwrap_or_else(|p| p.into_inner());
    starting.is_some_and(|(g, _)| g == generation)
}

fn permitted(app: &AppHandle, synthetic: bool) -> CommandResult<()> {
    let state = app.state::<AppState>();
    if synthetic || matches!(state.backend.permission(), PermissionState::Granted | PermissionState::NotRequired) {
        return Ok(());
    }
    permissions::screen_recording_missing(app);
    Err(CommandError::new(ErrorCode::PermissionDenied, "screen recording permission is missing"))
}

fn start_overlay(app: &AppHandle, mode: CaptureMode, at: Instant, auto: bool) -> CommandResult<()> {
    let Some(generation) = claim(app) else {
        return Ok(());
    };
    let synthetic = dev::options().synthetic;
    if let Err(error) = permitted(app, synthetic) {
        release(app, generation);
        return Err(error);
    }

    windows::prewarm_overlays(app);
    let app = app.clone();
    let spawned = thread::Builder::new().name("glimpsfy-capture".into()).spawn(move || {
        let result = capture_frozen(&app, mode, at, synthetic, auto);
        if is_claimed(&app, generation) {
            match result {
                Ok(session) => publish(&app, session),
                Err(error) => {
                    windows::hide_overlays(&app);
                    capture_failed(&app, error);
                }
            }
        }
        release(&app, generation);
    });
    if let Err(error) = spawned {
        return Err(CommandError::internal(error.to_string()));
    }
    Ok(())
}

fn capture_failed(app: &AppHandle, error: CaptureError) {
    trace::log(format!("[glimpsfy] capture failed: {error}"));
    if matches!(error, CaptureError::PermissionDenied) {
        permissions::screen_recording_missing(app);
    } else {
        windows::toast_failure(app, None, &CommandError::from(error));
    }
}

fn capture_frozen(
    app: &AppHandle,
    mode: CaptureMode,
    at: Instant,
    synthetic: bool,
    auto: bool,
) -> Result<CaptureSession, CaptureError> {
    let state = app.state::<AppState>();
    let settings = state.settings.get();
    let backend = state.backend.clone();
    let trace = Trace::new(at);

    let displays = backend.displays()?;
    let ids: Vec<DisplayId> = displays.iter().map(|d| d.id).collect();
    let options = CaptureOptions { shows_cursor: settings.general.include_pointer, exclude_own_windows: true };

    // The window list for snapping is read while the frames are taken.
    let lister = backend.clone();
    let listing = thread::spawn(move || lister.windows(WindowListOptions::default()));

    let frames: Vec<(DisplayId, Arc<Frame>)> = if synthetic {
        synthetic_frames(&displays)
    } else {
        backend.capture_displays(&ids, &options)?.into_iter().map(|(id, frame)| (id, Arc::new(frame))).collect()
    };
    let taken_at = SystemTime::now();
    trace.mark(Mark::Captured);
    let all_windows = listing.join().ok().and_then(Result::ok).unwrap_or_default();

    let pointer = pointer();
    let pointer_display = displays.iter().find(|d| d.frame.contains(pointer)).map(|d| d.id);
    let front = front_window(&all_windows);

    let entries = frames
        .into_iter()
        .filter_map(|(id, frame)| {
            let display = displays.iter().find(|d| d.id == id)?.clone();
            let windows = snap_targets(&display, &all_windows);
            Some(SessionDisplay {
                display,
                frame,
                token: token(),
                windows,
                ready: AtomicBool::new(false),
                painted: AtomicBool::new(false),
                removed: AtomicBool::new(false),
                failures: AtomicU32::new(0),
            })
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Err(CaptureError::Backend("no display could be captured".into()));
    }

    Ok(CaptureSession {
        id: state.sessions.next_id(),
        mode,
        trace,
        source: if synthetic { Source::Synthetic } else { Source::ScreenCaptureKit },
        format: FrameFormat::current(),
        displays: entries,
        pointer_display,
        pointer,
        front_app: front.app_name.clone(),
        front,
        taken_at,
        opened: Instant::now(),
        after: after_for(mode, settings.general.after_selection),
        all_windows,
        picked: Mutex::new(None),
        holder: Mutex::new(None),
        auto,
    })
}

/// The frontmost ordinary window (layer 0), whose app and title name an area capture.
fn front_window(windows: &[WindowInfo]) -> FrontWindow {
    windows
        .iter()
        .find(|w| w.layer == 0 && w.alpha > 0.0)
        .map(|w| FrontWindow {
            app_name: Some(w.app_name.clone()),
            bundle_id: w.bundle_id.clone(),
            title: w.title.clone(),
        })
        .unwrap_or_default()
}

/// The highest window level a capture can pick: `kCGModalPanelWindowLevel`. Floating windows
/// (picture in picture, palettes, always-on-top utilities) sit above ordinary ones and are what
/// the user sees there; the Dock, the menu bar and the system's own panels are higher.
const PICKABLE_LAYERS: std::ops::RangeInclusive<i32> = 0..=8;

/// Windows the user can pick (`PICKABLE_LAYERS`) that touch the display, made display-local,
/// frontmost first.
fn snap_targets(display: &Display, windows: &[WindowInfo]) -> Vec<OverlayWindow> {
    let local = LogicalRect::new(0.0, 0.0, display.frame.width, display.frame.height);
    windows
        .iter()
        .filter(|w| {
            PICKABLE_LAYERS.contains(&w.layer) && w.alpha > 0.0 && w.bounds.width >= 8.0 && w.bounds.height >= 8.0
        })
        .filter_map(|w| {
            let rect = w.bounds.relative_to(display.frame.origin());
            rect.intersection(&local)?;
            Some(OverlayWindow {
                id: w.id,
                app_name: w.app_name.clone(),
                title: w.title.clone(),
                rect,
                corner_radius: w.corner_radius,
            })
        })
        .collect()
}

fn publish(app: &AppHandle, session: CaptureSession) {
    let state = app.state::<AppState>();
    let session = Arc::new(session);
    *state.sessions.current.lock().unwrap_or_else(|p| p.into_inner()) = Some(session.clone());

    for entry in &session.displays {
        let label = windows::overlay_label(entry.display.id);
        if app.get_webview_window(&label).is_none() {
            trace::log(format!("[glimpsfy] no overlay window for display {}", entry.display.id.0));
            continue;
        }
        if let Some(payload) = session.overlay_frame(entry.display.id) {
            let _ = app.emit_to(EventTarget::webview_window(label), events::CAPTURE_STARTED, payload);
        }
    }

    windows::follow_pointer(app, session.id);

    // A page that never answers must not leave an invisible session behind. The pages report a
    // failure at once (`overlay_failed`); this catches a page that is not running at all.
    let app = app.clone();
    let id = session.id;
    thread::spawn(move || {
        thread::sleep(READY_TIMEOUT);
        let state = app.state::<AppState>();
        let Some(session) = state.sessions.get(id) else {
            return;
        };
        if session.displays.iter().any(|d| d.ready.load(Ordering::SeqCst)) {
            // Pages that never answered stay out of the way; the capture goes on without them.
            for entry in session.displays.iter().filter(|d| !d.ready.load(Ordering::SeqCst)) {
                entry.removed.store(true, Ordering::SeqCst);
            }
            return;
        }
        trace::log(format!("[glimpsfy] capture #{} closed: no overlay page answered", id.0));
        close(&app, id, CloseReason::Failed);
        windows::toast_failure(
            &app,
            session.pointer_display,
            &CommandError::internal("The capture could not be shown on screen within 5 seconds. Try again."),
        );
    });
}

// ------------------------------------------------------------------ captures without overlay

/// A capture that is delivered at once.
#[derive(Clone, Copy, Debug)]
enum Direct {
    /// The display under the pointer.
    Display,
    /// Every display, the primary first.
    AllDisplays,
    Repeat(LastArea),
}

impl Direct {
    fn mode(self) -> CaptureMode {
        match self {
            Direct::Display => CaptureMode::Display,
            Direct::AllDisplays => CaptureMode::AllDisplays,
            Direct::Repeat(_) => CaptureMode::RepeatLastArea,
        }
    }
}

fn start_direct(app: &AppHandle, kind: Direct, at: Instant) -> CommandResult<()> {
    let Some(generation) = claim(app) else {
        return Ok(());
    };
    let synthetic = dev::options().synthetic;
    if let Err(error) = permitted(app, synthetic) {
        release(app, generation);
        return Err(error);
    }
    let app = app.clone();
    let spawned = thread::Builder::new().name("glimpsfy-capture".into()).spawn(move || {
        let result = capture_direct(&app, kind, synthetic);
        release(&app, generation);
        match result {
            Ok(Some(images)) => {
                trace::log(format!(
                    "[glimpsfy] {:?} capture: {} image{} in {:.1} ms",
                    kind.mode(),
                    images.len(),
                    if images.len() == 1 { "" } else { "s" },
                    trace::ms(at.elapsed())
                ));
                let action = direct_action(app.state::<AppState>().settings.get().general.after_selection);
                // A failure is reported by `deliver_many` itself (event and toast).
                let _ = output::deliver_many(&app, images, action);
            }
            Ok(None) => {
                trace::log("[glimpsfy] the last area's display is gone or resized; an area capture starts instead");
                if let Err(error) = start_overlay(&app, CaptureMode::Area, at, false) {
                    windows::toast_failure(&app, None, &error);
                }
            }
            Err(error) => capture_failed(&app, error),
        }
    });
    if let Err(error) = spawned {
        return Err(CommandError::internal(error.to_string()));
    }
    Ok(())
}

/// The images of a capture without overlay; `None` when the last area cannot be repeated.
fn capture_direct(
    app: &AppHandle,
    kind: Direct,
    synthetic: bool,
) -> Result<Option<Vec<(Frame, Origin)>>, CaptureError> {
    let state = app.state::<AppState>();
    let settings = state.settings.get();
    let backend = state.backend.clone();
    let displays = backend.displays()?;

    let pointer = pointer();
    let mut targets: Vec<Display> = match kind {
        Direct::Display => displays
            .iter()
            .find(|d| d.frame.contains(pointer))
            .or_else(|| displays.iter().find(|d| d.is_primary))
            .or(displays.first())
            .cloned()
            .into_iter()
            .collect(),
        Direct::AllDisplays => displays.clone(),
        Direct::Repeat(area) => {
            let same = displays.iter().find(|d| {
                d.id == area.display
                    && (d.frame.width - area.display_frame.width).abs() < 0.5
                    && (d.frame.height - area.display_frame.height).abs() < 0.5
            });
            match same {
                Some(display) => vec![display.clone()],
                None => return Ok(None),
            }
        }
    };
    targets.sort_by_key(|d| !d.is_primary);
    if targets.is_empty() {
        return Err(CaptureError::Backend("no display is connected".into()));
    }

    let lister = backend.clone();
    let listing = thread::spawn(move || lister.windows(WindowListOptions::default()));
    let ids: Vec<DisplayId> = targets.iter().map(|d| d.id).collect();
    let options = CaptureOptions { shows_cursor: settings.general.include_pointer, exclude_own_windows: true };
    let mut frames: Vec<(DisplayId, Frame)> = if synthetic {
        synthetic_frames(&targets).into_iter().map(|(id, frame)| (id, (*frame).clone())).collect()
    } else {
        backend.capture_displays(&ids, &options)?
    };
    let taken_at = SystemTime::now();
    let front = front_window(&listing.join().ok().and_then(Result::ok).unwrap_or_default());
    frames.sort_by_key(|(id, _)| ids.iter().position(|i| i == id));

    let capture_id = state.sessions.next_id();
    let mut images = Vec::with_capacity(frames.len());
    for (id, frame) in frames {
        let Some(display) = targets.iter().find(|d| d.id == id) else {
            continue;
        };
        let frame = match kind {
            Direct::Repeat(area) => {
                let rect = selection_to_frame(area.rect, display.frame.size(), frame.size)
                    .ok_or_else(|| CaptureError::Backend("the last area is outside its display".into()))?;
                frame.crop(rect)?
            }
            _ => frame,
        };
        let origin = Origin {
            capture_id,
            mode: kind.mode(),
            display: Some(display.clone()),
            app_name: front.app_name.clone(),
            bundle_id: front.bundle_id.clone(),
            window_title: front.title.clone(),
            taken_at,
        };
        images.push((frame, origin));
    }
    if images.is_empty() {
        return Err(CaptureError::Backend("no display could be captured".into()));
    }
    Ok(Some(images))
}

/// Keeps a copied or saved selection for "Repeat last area".
pub fn remember_area(app: &AppHandle, session: &CaptureSession, selection: &Selection) {
    if session.mode == CaptureMode::CopyText {
        return;
    }
    let Some(entry) = session.display(selection.display_id) else {
        return;
    };
    let area = LastArea { display: selection.display_id, display_frame: entry.display.frame, rect: selection.rect };
    *app.state::<AppState>().sessions.last_area.lock().unwrap_or_else(|p| p.into_inner()) = Some(area);
}

/// The origin of an image of capture `id`, open or closed a moment ago.
pub fn origin(app: &AppHandle, id: CaptureId, display: DisplayId) -> Option<Origin> {
    let state = app.state::<AppState>();
    if let Some(session) = state.sessions.get(id) {
        return Some(session.origin(display));
    }
    let recent = state.sessions.recent.lock().unwrap_or_else(|p| p.into_inner());
    let recent = recent.as_ref().filter(|r| r.id == id)?;
    Some(Origin {
        capture_id: id,
        mode: recent.mode,
        display: recent.displays.iter().find(|d| d.id == display).cloned(),
        app_name: recent.front.app_name.clone(),
        bundle_id: recent.front.bundle_id.clone(),
        window_title: recent.front.title.clone(),
        taken_at: recent.taken_at,
    })
}

// ----------------------------------------------------------------------------- the overlay

/// An overlay drew its frame: its window is shown, with the keyboard when the pointer is on it.
pub fn ready(app: &AppHandle, id: CaptureId, display: DisplayId) -> CommandResult<()> {
    let state = app.state::<AppState>();
    let session = state.sessions.get(id).ok_or_else(|| CommandError::not_found(format!("capture #{}", id.0)))?;
    let entry = session.display(display).ok_or_else(|| CommandError::not_found(format!("display {}", display.0)))?;
    if !entry.live() || entry.ready.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let key = match session.pointer_display {
        Some(pointer) => pointer == display,
        None => entry.display.is_primary,
    };
    windows::show_overlay(app, display, key);
    if session.trace.get(Mark::Visible).is_none() {
        stall::watch(app, &session);
    }
    session.trace.mark(Mark::Visible);
    Ok(())
}

pub fn painted(app: &AppHandle, id: CaptureId, display: DisplayId, report: PaintReport) {
    let state = app.state::<AppState>();
    let Some(session) = state.sessions.get(id) else {
        return;
    };
    let Some(entry) = session.display(display) else {
        return;
    };
    if entry.painted.swap(true, Ordering::SeqCst) {
        return;
    }
    session.trace.mark(Mark::Painted);
    session.trace.note(format!(
        "page {}: fetch {:.1} decode {:.1} draw {:.1} paint {:.1} ms",
        display.0, report.fetch_ms, report.decode_ms, report.draw_ms, report.paint_ms
    ));
    if !session.all_painted() {
        return;
    }

    let source = match session.source {
        Source::ScreenCaptureKit => "screencapturekit",
        Source::Synthetic => "synthetic",
    };
    let count = session.displays.len();
    let label = format!("capture #{} · {source} · {count} display{}", id.0, if count == 1 { "" } else { "s" });
    trace::log(session.trace.summary(&label));
    stall::occlusion(app, &session, "painted");
    record_sample(&session.trace);

    if session.auto {
        let app = app.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(120));
            close(&app, id, CloseReason::Cancelled);
        });
    }
}

/// An overlay page could not load or draw its frame. The first failure reloads the page, which
/// asks for its frame again (`capture_current`); a second one drops the display when another
/// display carries on and the user is not working on this one, and closes the capture with a
/// failure toast otherwise.
pub fn overlay_failed(app: &AppHandle, id: CaptureId, display: DisplayId, message: &str) {
    let state = app.state::<AppState>();
    let Some(session) = state.sessions.get(id) else {
        return;
    };
    let Some(entry) = session.display(display) else {
        return;
    };
    let failures = entry.failures.fetch_add(1, Ordering::SeqCst) + 1;
    trace::log(format!(
        "[glimpsfy] capture #{} display {}: the overlay failed ({message}), try {failures}",
        id.0, display.0
    ));
    if failures == 1 {
        if let Some(window) = app.get_webview_window(&windows::overlay_label(display)) {
            if window.reload().is_ok() {
                return;
            }
        }
    }

    let key = match session.pointer_display {
        Some(pointer) => pointer == display,
        None => entry.display.is_primary,
    };
    let others = session.displays.iter().any(|d| d.display.id != display && d.live());
    if others && !key {
        entry.removed.store(true, Ordering::SeqCst);
        hide_overlay(app, display);
        return;
    }
    close(app, id, CloseReason::Failed);
    windows::toast_failure(
        app,
        Some(display),
        &CommandError::internal(format!("The capture could not be shown on screen ({message}). Try again.")),
    );
}

/// Captures one window of the open capture on its own and keeps it for Copy and Save.
pub fn pick_window(
    app: &AppHandle,
    id: CaptureId,
    display: DisplayId,
    window: WindowId,
) -> CommandResult<PickedWindow> {
    let state = app.state::<AppState>();
    let session = state.sessions.get(id).ok_or_else(|| CommandError::not_found(format!("capture #{}", id.0)))?;
    let entry = session.display(display).ok_or_else(|| CommandError::not_found(format!("display {}", display.0)))?;
    let info = session
        .all_windows
        .iter()
        .find(|w| w.id == window)
        .cloned()
        .ok_or_else(|| CommandError::not_found(format!("window {}", window.0)))?;
    let rect = info.bounds.relative_to(entry.display.frame.origin());

    let frame = if session.source == Source::Synthetic {
        // Without ScreenCaptureKit, the window's rectangle of the frozen frame stands in.
        let bounds = LogicalRect::new(0.0, 0.0, entry.display.frame.width, entry.display.frame.height);
        let local =
            rect.intersection(&bounds).ok_or_else(|| CommandError::invalid("the window is not on this display"))?;
        let pixels = selection_to_frame(local, entry.display.frame.size(), entry.frame.size)
            .ok_or_else(|| CommandError::invalid("the window is not on this display"))?;
        entry.frame.crop(pixels)?
    } else {
        let settings = state.settings.get();
        let options = WindowCaptureOptions {
            shows_cursor: settings.general.include_pointer,
            include_shadow: settings.output.window_shadow,
            keep_rounded_corners: settings.output.window_corners,
            background: match settings.output.window_background {
                WindowBackground::White => Some([255, 255, 255]),
                WindowBackground::Transparent => None,
            },
        };
        state.backend.capture_window(window, &options)?
    };

    let picked = PickedWindow { window_id: window, width: frame.size.width, height: frame.size.height };
    *session.picked.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(Picked {
        window_id: window,
        display,
        rect,
        frame: Arc::new(frame),
        app_name: info.app_name,
        bundle_id: info.bundle_id,
        title: info.title,
    }));
    Ok(picked)
}

/// Shows the overlays of capture `id` again after they stepped aside (a cancelled save panel).
pub fn reshow(app: &AppHandle, id: CaptureId) {
    let state = app.state::<AppState>();
    let Some(session) = state.sessions.get(id) else {
        return;
    };
    let keyed = session.holder().or(session.pointer_display);
    for entry in session.displays.iter().filter(|d| d.live() && d.ready.load(Ordering::SeqCst)) {
        let key = keyed.map_or(entry.display.is_primary, |k| k == entry.display.id);
        windows::show_overlay(app, entry.display.id, key);
    }
}

/// The displays changed while a capture may be open. When displays were only removed and every
/// other display is unchanged, the capture goes on without the removed ones; any other change
/// (a display moved, rescaled or added) closes it, since its frames no longer match the screens.
pub fn displays_changed(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(current) = state.sessions.current() else {
        return;
    };
    let now = match state.backend.displays() {
        Ok(displays) => displays,
        Err(error) => {
            trace::log(format!("[glimpsfy] displays changed, and they cannot be read: {error}"));
            close(app, current.id, CloseReason::Cancelled);
            return;
        }
    };
    let unchanged = |entry: &SessionDisplay, display: &Display| {
        display.frame == entry.display.frame && display.pixel_size == entry.display.pixel_size
    };
    let mut gone = Vec::new();
    for entry in current.displays.iter().filter(|d| d.live()) {
        match now.iter().find(|d| d.id == entry.display.id) {
            None => gone.push(entry),
            Some(display) if unchanged(entry, display) => {}
            Some(_) => {
                trace::log(format!(
                    "[glimpsfy] capture #{} closed: display {} changed",
                    current.id.0, entry.display.id.0
                ));
                close(app, current.id, CloseReason::Cancelled);
                return;
            }
        }
    }
    for entry in &gone {
        entry.removed.store(true, Ordering::SeqCst);
        trace::log(format!("[glimpsfy] capture #{}: display {} was removed", current.id.0, entry.display.id.0));
    }
    if !current.displays.iter().any(|d| d.live()) {
        close(app, current.id, CloseReason::Cancelled);
    }
}

/// Ends a capture: every overlay window goes, and the pages hear `capture://closed`.
pub fn close(app: &AppHandle, id: CaptureId, reason: CloseReason) -> Option<Arc<CaptureSession>> {
    let state = app.state::<AppState>();
    let session = state.sessions.take(id)?;
    windows::hide_overlays(app);
    let _ = app.emit(events::CAPTURE_CLOSED, CaptureClosed { capture_id: id, reason });
    *state.sessions.recent.lock().unwrap_or_else(|p| p.into_inner()) = Some(Recent {
        id,
        mode: session.mode,
        displays: session.displays.iter().map(|d| d.display.clone()).collect(),
        front: session.front.clone(),
        taken_at: session.taken_at,
    });
    Some(session)
}

fn hide_overlay(app: &AppHandle, display: DisplayId) {
    let Some(window) = app.get_webview_window(&windows::overlay_label(display)) else {
        return;
    };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::hide_overlay(&window, mtm));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = window.hide();
    }
}

// ------------------------------------------------------------------------- first-paint stalls

/// Dev-only probes for the rare first paint that takes 300–450 ms instead of ~20 ms, on every
/// display at once (`docs/ARCHITECTURE.md`, "First-paint stalls"). They log, per capture:
///
/// - each overlay window's occlusion state when the first overlay is shown and when all have
///   painted ("occluded" at show time supports the occlusion hypothesis: WebKit holds back
///   `requestAnimationFrame` of a window that AppKit reports occluded until the next occlusion
///   notification, and the prewarmed overlay sits at alpha 0);
/// - the worst delay of the main thread between the first show and the last paint, from an
///   empty closure posted every 16 ms (a long delay supports the blocked-main-thread hypothesis:
///   the UI process commits every webview's layers on the main thread, so all displays stall
///   together).
mod stall {
    use super::*;

    #[cfg(all(debug_assertions, target_os = "macos"))]
    pub fn watch(app: &AppHandle, session: &Arc<CaptureSession>) {
        occlusion(app, session, "shown");
        let (app, session) = (app.clone(), session.clone());
        thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(1);
            let mut worst = Duration::ZERO;
            while Instant::now() < until && !session.all_painted() {
                let sent = Instant::now();
                let (tx, rx) = std::sync::mpsc::channel();
                if app
                    .run_on_main_thread(move || {
                        let _ = tx.send(sent.elapsed());
                    })
                    .is_err()
                {
                    return;
                }
                worst = worst.max(rx.recv_timeout(Duration::from_secs(1)).unwrap_or(Duration::from_secs(1)));
                thread::sleep(Duration::from_millis(16));
            }
            trace::log(format!(
                "[glimpsfy] capture #{} · main thread's worst delay while painting {:.1} ms",
                session.id.0,
                trace::ms(worst)
            ));
        });
    }

    #[cfg(all(debug_assertions, target_os = "macos"))]
    pub fn occlusion(app: &AppHandle, session: &Arc<CaptureSession>, when: &'static str) {
        use objc2_app_kit::{NSWindow, NSWindowOcclusionState};
        let (handle, session) = (app.clone(), session.clone());
        crate::macos::on_main(app, move |_mtm| {
            let states: Vec<String> = session
                .displays
                .iter()
                .filter_map(|entry| {
                    let window = handle.get_webview_window(&windows::overlay_label(entry.display.id))?;
                    let pointer = window.ns_window().ok().filter(|p| !p.is_null())?;
                    // SAFETY: tao's NSWindow for this window, alive while the window is; read on
                    // the main thread.
                    let ns_window = unsafe { &*(pointer as *const NSWindow) };
                    let visible = ns_window.occlusionState().0 & NSWindowOcclusionState::Visible.0 != 0;
                    Some(format!("{} {}", entry.display.id.0, if visible { "visible" } else { "occluded" }))
                })
                .collect();
            trace::log(format!("[glimpsfy] capture #{} · occlusion when {when}: {}", session.id.0, states.join(", ")));
        });
    }

    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    pub fn watch(_app: &AppHandle, _session: &Arc<CaptureSession>) {}

    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    pub fn occlusion(_app: &AppHandle, _session: &Arc<CaptureSession>, _when: &'static str) {}
}

// ----------------------------------------------------------------------------------- helpers

fn pointer() -> LogicalPoint {
    #[cfg(target_os = "macos")]
    return crate::macos::pointer();
    #[cfg(not(target_os = "macos"))]
    LogicalPoint { x: 0.0, y: 0.0 }
}

/// A token no earlier capture used, so a page never shows a stale frame.
fn token() -> String {
    let seed = std::collections::hash_map::RandomState::new();
    format!("{:016x}", seed.hash_one(Instant::now()))
}

/// Frames of each display's real pixel size (5120 × 2880 with `GLIMPSFY_SIMULATE_5K`), made
/// once and reused, so a synthetic capture costs what a finished ScreenCaptureKit capture hands
/// over and nothing more. The pattern is a colour ramp under a 100 px grid, so a wrong channel
/// order, scale or crop shows at a glance.
fn synthetic_frames(displays: &[Display]) -> Vec<(DisplayId, Arc<Frame>)> {
    static CACHE: OnceLock<Mutex<HashMap<DisplayId, Arc<Frame>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    displays
        .iter()
        .map(|display| {
            let size = if dev::options().simulate_5k { PhysicalSize::new(5120, 2880) } else { display.pixel_size };
            let scale = ScaleFactor(size.width as f64 / display.frame.width.max(1.0));
            let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
            let fits = |f: &Arc<Frame>| f.size == size && f.scale == scale;
            let frame = match cache.get(&display.id) {
                Some(frame) if fits(frame) => frame.clone(),
                _ => {
                    let frame = Arc::new(Frame { scale, ..ramp(size) });
                    cache.insert(display.id, frame.clone());
                    frame
                }
            };
            (display.id, frame)
        })
        .collect()
}

fn ramp(size: PhysicalSize) -> Frame {
    let (w, h) = (size.width as usize, size.height as usize);
    let mut data = vec![0u8; w * h * 4];
    for (y, row) in data.chunks_exact_mut(w * 4).enumerate() {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let grid = x % 100 == 0 || y % 100 == 0;
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            let b = 160u8;
            let (r, g, b) = if grid { (255, 255, 255) } else { (r, g, b) };
            px.copy_from_slice(&[b, g, r, 255]);
        }
    }
    Frame {
        size,
        bytes_per_row: w * 4,
        format: PixelFormat::Bgra8,
        color: ColorInfo::srgb(),
        scale: ScaleFactor::ONE,
        data,
    }
}

// ------------------------------------------------------------------------------ measurement

#[derive(Clone, Copy, Debug)]
struct Sample {
    captured: f64,
    served: f64,
    visible: f64,
    painted: f64,
}

fn samples() -> &'static Mutex<Vec<Sample>> {
    static SAMPLES: OnceLock<Mutex<Vec<Sample>>> = OnceLock::new();
    SAMPLES.get_or_init(Default::default)
}

fn record_sample(trace: &Trace) {
    let at = |mark| trace.get(mark).map_or(f64::NAN, trace::ms);
    samples().lock().unwrap_or_else(|p| p.into_inner()).push(Sample {
        captured: at(Mark::Captured),
        served: at(Mark::Served),
        visible: at(Mark::Visible),
        painted: at(Mark::Painted),
    });
}

/// `GLIMPSFY_AUTOCAPTURE=N`: N area captures through the shortcut's own entry, each closed once
/// painted, then the statistics and a quit.
pub fn autocapture(app: AppHandle, runs: u32) {
    thread::spawn(move || {
        // Let every overlay page load and start listening first.
        thread::sleep(Duration::from_secs(4));
        let gap = Duration::from_millis(dev::options().autocapture_gap_ms);
        for _ in 0..runs {
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                let _ = start_area(&handle, Instant::now(), true);
            });
            let deadline = Instant::now() + Duration::from_secs(8);
            thread::sleep(Duration::from_millis(50));
            while !app.state::<AppState>().sessions.is_idle() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(gap);
        }
        print_statistics(runs);
        app.exit(0);
    });
}

fn print_statistics(runs: u32) {
    let samples = samples().lock().unwrap_or_else(|p| p.into_inner()).clone();
    let stat = |pick: fn(&Sample) -> f64| {
        let mut values: Vec<f64> = samples.iter().map(pick).filter(|v| v.is_finite()).collect();
        values.sort_by(f64::total_cmp);
        if values.is_empty() {
            return "n/a".to_string();
        }
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        format!(
            "min {:.1} · median {:.1} · p90 {:.1} · max {:.1}",
            values[0],
            at(0.5),
            at(0.9),
            values[values.len() - 1]
        )
    };
    eprintln!("[glimpsfy] autocapture: {} of {runs} captures painted (ms since the trigger)", samples.len());
    eprintln!("[glimpsfy]   captured  {}", stat(|s| s.captured));
    eprintln!("[glimpsfy]   served    {}", stat(|s| s.served));
    eprintln!("[glimpsfy]   visible   {}", stat(|s| s.visible));
    eprintln!("[glimpsfy]   painted   {}", stat(|s| s.painted));
}
