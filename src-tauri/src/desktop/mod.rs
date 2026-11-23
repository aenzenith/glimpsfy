//! The app shell on Windows and Linux: what `macos.rs` gives the app on macOS (overlays, the
//! toast, the popover, pins, the recorder's panels, the clipboard, the tray icon and the places of
//! all of them), written against Tauri's cross-platform API.
//!
//! What needs the system itself lives in `windows.rs` and `linux.rs`. Both offer the same
//! functions, which this file reaches as `platform::…`; each of them says in its comment what the
//! native implementation owes. A function here is named after the `macos.rs` function it stands
//! for and takes no `MainThreadMarker`: Tauri's window calls are safe from any thread, and what
//! must run on the main thread goes through [`on_main`].
//!
//! Differences from macOS that are decided, not missing:
//!
//! - **The overlay takes the keyboard focus.** Windows and Linux have no panel that takes keys
//!   without becoming the active window, so a shown overlay is the focused window, and the window
//!   that had the focus before gets it back when the overlays hide.
//! - **Own windows and captures.** macOS filters Glimpsfy's windows in the capture backend. Here
//!   each such window is marked once when it is made ([`exclude_from_capture`]); where the system
//!   cannot exclude a window, [`hide_for_capture`] hides them around a capture instead.
//! - **Places.** The capture crate's `Display.frame` says where a display is. [`to_physical`] and
//!   [`to_logical`] are the only place that turns those points into the pixels tao places windows
//!   in, and every window is placed through [`set_frame`].

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;

#[cfg(not(target_os = "windows"))]
#[path = "linux.rs"]
mod platform;

#[cfg(target_os = "linux")]
pub mod loopback;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use glimpsfy_capture::{ColorSpace, Display, DisplayId, LogicalPoint, LogicalRect, PhysicalSize, ScaleFactor};
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::commands::windows::{Platform, SystemPane};
use crate::permissions::Permission;
use crate::windows::{ToastFrame, ToastGlass};
use crate::{trace, AppState};

// ------------------------------------------------------------------------------- main thread

static APP: OnceLock<AppHandle> = OnceLock::new();

/// How long a read that has to pass through the main thread is waited for. The main thread can
/// itself be waiting for a lock the caller holds, so no read waits for good.
const MAIN_WAIT: Duration = Duration::from_millis(250);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keeps the app's handle. Once, at start, before any window is made.
pub fn setup(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

fn app() -> Option<&'static AppHandle> {
    APP.get()
}

/// Runs `f` on the main thread: at once when called there, otherwise queued on the event loop.
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    if let Some(app) = app() {
        let _ = app.run_on_main_thread(f);
    }
}

/// Runs `f` on the main thread and waits for its answer, `MAIN_WAIT` at most.
fn main_wait<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    on_main(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(MAIN_WAIT).ok()
}

// ---------------------------------------------------------------------------- the platform

/// What `app_platform` answers on Windows and Linux: the system, the Linux session and what this
/// platform can do today. A capability whose native part is not written yet is `false`.
pub fn describe() -> Platform {
    let session = platform::session();
    #[cfg(target_os = "linux")]
    let video_base = loopback::base().map(str::to_string);
    #[cfg(not(target_os = "linux"))]
    let video_base = None;
    Platform { os: platform::OS, session, capabilities: platform::capabilities(), video_base }
}

// -------------------------------------------------------------------------------- geometry
//
// Global logical points are the capture crate's: `Display.frame` is where a display is. On Linux
// they are X pixels divided by GTK's integer scale, one coherent space. On Windows no coherent
// space in points exists, so `Display.frame.x/y` is the monitor's top-left in virtual-screen
// pixels and its size is the pixel size divided by the scale; a point belongs to the display
// whose frame holds it. Both read as `physical = origin + (logical − frame origin) × scale`,
// where the display's physical origin is the platform's to say (`platform::physical_origin`).

/// A display with what placing a window on it needs.
#[derive(Clone, Debug)]
pub struct Screen {
    pub display: Display,
    /// The display's top-left corner in the pixels tao places windows in.
    origin: (f64, f64),
    /// The part not covered by the taskbar or a panel (the work area), in global points.
    pub visible: LogicalRect,
}

impl Screen {
    fn scale(&self) -> f64 {
        if self.display.scale.0 > 0.0 {
            self.display.scale.0
        } else {
            1.0
        }
    }

    /// How many of tao's pixels make one point on this screen. On Linux that is GTK's one window
    /// scale (`platform::window_scale`): a Wayland session measures a display's pixels in the
    /// compositor's, which are not the X pixels windows are placed in.
    fn window_scale(&self) -> f64 {
        #[cfg(target_os = "linux")]
        return platform::window_scale();
        #[cfg(not(target_os = "linux"))]
        self.scale()
    }

    fn physical_size(&self) -> (f64, f64) {
        #[cfg(target_os = "linux")]
        return (self.display.frame.width * self.window_scale(), self.display.frame.height * self.window_scale());
        #[cfg(not(target_os = "linux"))]
        (f64::from(self.display.pixel_size.width), f64::from(self.display.pixel_size.height))
    }
}

/// The screens as last read (`refresh_screens`). A pointer poll reads them thirty times a second,
/// so they are kept instead of being asked from the backend each time.
static SCREENS: Mutex<Vec<Screen>> = Mutex::new(Vec::new());

/// One of tao's monitors, in physical pixels.
#[derive(Clone, Debug, PartialEq)]
struct MonitorFacts {
    name: Option<String>,
    position: (f64, f64),
    size: (u32, u32),
    work: (f64, f64, f64, f64),
    scale: f64,
    primary: bool,
}

fn monitors() -> Vec<MonitorFacts> {
    let Some(app) = app() else {
        return Vec::new();
    };
    main_wait(move || {
        let primary = app.primary_monitor().ok().flatten().map(|m| *m.position());
        app.available_monitors()
            .unwrap_or_default()
            .iter()
            .map(|m| {
                let work = m.work_area();
                MonitorFacts {
                    name: m.name().cloned(),
                    position: (f64::from(m.position().x), f64::from(m.position().y)),
                    size: (m.size().width, m.size().height),
                    work: (
                        f64::from(work.position.x),
                        f64::from(work.position.y),
                        f64::from(work.size.width),
                        f64::from(work.size.height),
                    ),
                    scale: if m.scale_factor() > 0.0 { m.scale_factor() } else { 1.0 },
                    primary: primary.is_some_and(|p| p == *m.position()),
                }
            })
            .collect()
    })
    .unwrap_or_default()
}

/// A display made from one of tao's monitors, for the time the capture backend lists none (it
/// failed, or it is not written yet): windows still land on the right screens then. Its id is
/// the monitor's place in tao's list and means nothing to the backend.
fn display_of(index: usize, monitor: &MonitorFacts) -> Display {
    let (x, y) = platform::logical_origin(monitor.position, monitor.scale);
    Display {
        id: DisplayId(index as u32 + 1),
        name: monitor.name.clone().unwrap_or_else(|| format!("Display {}", index + 1)),
        frame: LogicalRect {
            x,
            y,
            width: f64::from(monitor.size.0) / monitor.scale,
            height: f64::from(monitor.size.1) / monitor.scale,
        },
        scale: ScaleFactor(monitor.scale),
        pixel_size: PhysicalSize::new(monitor.size.0, monitor.size.1),
        is_primary: monitor.primary,
        is_builtin: false,
        color_space: ColorSpace::Srgb,
        refresh_rate: None,
    }
}

/// Reads the displays again: at start and whenever the layout changed.
pub fn refresh_screens() {
    let Some(app) = app() else {
        return;
    };
    let monitors = monitors();
    let listed = app.try_state::<AppState>().and_then(|state| state.backend.displays().ok());
    let displays = match listed {
        Some(displays) if !displays.is_empty() => displays,
        _ => monitors.iter().enumerate().map(|(index, monitor)| display_of(index, monitor)).collect(),
    };
    let screens: Vec<Screen> = displays
        .into_iter()
        .map(|display| {
            let origin = platform::physical_origin(&display);
            #[cfg(target_os = "linux")]
            let scale = platform::window_scale();
            #[cfg(not(target_os = "linux"))]
            let scale = if display.scale.0 > 0.0 { display.scale.0 } else { 1.0 };
            let monitor = monitors
                .iter()
                .find(|m| (m.position.0 - origin.0).abs() < 1.5 && (m.position.1 - origin.1).abs() < 1.5);
            let visible = match monitor {
                Some(m) => LogicalRect {
                    x: display.frame.x + (m.work.0 - origin.0) / scale,
                    y: display.frame.y + (m.work.1 - origin.1) / scale,
                    width: m.work.2 / scale,
                    height: m.work.3 / scale,
                },
                None => display.frame,
            };
            Screen { display, origin, visible }
        })
        .collect();
    *lock(&SCREENS) = screens;
}

/// The screens, read now when none were read yet.
pub fn screens() -> Vec<Screen> {
    let known = lock(&SCREENS).clone();
    if !known.is_empty() {
        return known;
    }
    refresh_screens();
    lock(&SCREENS).clone()
}

fn primary(screens: &[Screen]) -> Option<&Screen> {
    screens.iter().find(|s| s.display.is_primary).or(screens.first())
}

/// How far a point is from a rectangle given by its origin and size; zero inside it.
fn distance(point: (f64, f64), origin: (f64, f64), size: (f64, f64)) -> f64 {
    let dx = (origin.0 - point.0).max(point.0 - (origin.0 + size.0)).max(0.0);
    let dy = (origin.1 - point.1).max(point.1 - (origin.1 + size.1)).max(0.0);
    dx.hypot(dy)
}

/// The screen a global logical point belongs to: the one whose frame holds it, or the nearest.
fn screen_at(screens: &[Screen], point: LogicalPoint) -> Option<&Screen> {
    let far = |s: &Screen| {
        let f = s.display.frame;
        distance((point.x, point.y), (f.x, f.y), (f.width, f.height))
    };
    screens.iter().min_by(|a, b| far(a).total_cmp(&far(b)))
}

/// The screen a physical point lies on, or the nearest.
fn screen_at_physical(screens: &[Screen], point: (f64, f64)) -> Option<&Screen> {
    let far = |s: &Screen| distance(point, s.origin, s.physical_size());
    screens.iter().min_by(|a, b| far(a).total_cmp(&far(b)))
}

/// A rectangle in global logical points as the position and size tao places a window with. The
/// rectangle belongs to the display that holds its middle, so a window that reaches over an edge
/// keeps one scale. This and [`to_logical`] are the whole mapping between points and pixels; the
/// capture crate will export the same mapping, and they are what to swap for it then.
pub fn to_physical(rect: LogicalRect) -> (tauri::PhysicalPosition<i32>, tauri::PhysicalSize<u32>) {
    // Windows maps through the capture crate, so a window and a capture of the same rectangle
    // land on the same pixels; the display that holds most of the rectangle is the one used.
    #[cfg(target_os = "windows")]
    if let Some(pixels) = glimpsfy_capture::desktop::logical_to_physical_rect(&displays(), rect) {
        return (
            tauri::PhysicalPosition::new(pixels.x, pixels.y),
            tauri::PhysicalSize::new(pixels.width.max(1), pixels.height.max(1)),
        );
    }
    let screens = screens();
    let middle = LogicalPoint { x: rect.x + rect.width / 2.0, y: rect.y + rect.height / 2.0 };
    let (origin, frame, scale) = match screen_at(&screens, middle) {
        Some(screen) => (screen.origin, screen.display.frame, screen.window_scale()),
        None => ((0.0, 0.0), LogicalRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 1.0),
    };
    let x = origin.0 + (rect.x - frame.x) * scale;
    let y = origin.1 + (rect.y - frame.y) * scale;
    (
        tauri::PhysicalPosition::new(x.round() as i32, y.round() as i32),
        tauri::PhysicalSize::new(
            (rect.width * scale).round().max(1.0) as u32,
            (rect.height * scale).round().max(1.0) as u32,
        ),
    )
}

/// A physical point (tao's window positions, the pointer) in global logical points: the inverse
/// of [`to_physical`].
pub fn to_logical(x: f64, y: f64) -> LogicalPoint {
    #[cfg(target_os = "windows")]
    if let Some(point) = glimpsfy_capture::desktop::physical_to_logical_point(
        &displays(),
        glimpsfy_capture::PhysicalPoint { x: x.round() as i32, y: y.round() as i32 },
    ) {
        return point;
    }
    let screens = screens();
    match screen_at_physical(&screens, (x, y)) {
        Some(screen) => LogicalPoint {
            x: screen.display.frame.x + (x - screen.origin.0) / screen.window_scale(),
            y: screen.display.frame.y + (y - screen.origin.1) / screen.window_scale(),
        },
        None => LogicalPoint { x, y },
    }
}

/// Moves and sizes a window to `frame`, in global top-left points. The position goes first: on
/// Windows a window that lands on a display of another scale is resized by the system to keep
/// its size in points, and the size that follows is the one that stays.
pub fn set_frame(window: &WebviewWindow, frame: LogicalRect) {
    let (position, size) = to_physical(frame);
    // Windows places the client area natively and holds it through the scale change the move
    // brings (`platform::place`): tao's answer to that change would keep the old size in points.
    #[cfg(target_os = "windows")]
    platform::place(window, position, Some(size));
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window.set_position(position);
        let _ = window.set_size(size);
    }
}

/// The displays as the capture crate's mapping takes them.
#[cfg(target_os = "windows")]
fn displays() -> Vec<Display> {
    screens().into_iter().map(|screen| screen.display).collect()
}

/// A window's frame in global top-left points, or `None` when the main thread did not answer in
/// time. Meant for the undecorated panels, whose outer and inner sizes are the same.
pub fn frame_of(window: &WebviewWindow) -> Option<LogicalRect> {
    let window = window.clone();
    // On Windows the client area is read, which is what `set_frame` places: the popover's
    // shadow keeps invisible borders around it in the outer frame.
    #[cfg(target_os = "windows")]
    let (position, size) = main_wait(move || Some((window.inner_position().ok()?, window.inner_size().ok()?)))??;
    #[cfg(target_os = "windows")]
    if let Some(frame) = glimpsfy_capture::desktop::physical_to_logical_rect(
        &displays(),
        glimpsfy_capture::PhysicalRect::new(position.x, position.y, size.width, size.height),
    ) {
        return Some(frame);
    }
    #[cfg(not(target_os = "windows"))]
    let (position, size) = main_wait(move || Some((window.outer_position().ok()?, window.outer_size().ok()?)))??;
    let (x, y) = (f64::from(position.x), f64::from(position.y));
    let (width, height) = (f64::from(size.width), f64::from(size.height));
    let screens = screens();
    let screen = screen_at_physical(&screens, (x + width / 2.0, y + height / 2.0))?;
    let scale = screen.window_scale();
    Some(LogicalRect {
        x: screen.display.frame.x + (x - screen.origin.0) / scale,
        y: screen.display.frame.y + (y - screen.origin.1) / scale,
        width: width / scale,
        height: height / scale,
    })
}

/// A point on no display: what [`pointer`] answers where the system does not say where the
/// pointer is (Wayland). No display's frame holds it, so "the display under the pointer" falls
/// back to the primary one everywhere.
const NOWHERE: LogicalPoint = LogicalPoint { x: -1.0e9, y: -1.0e9 };

/// The pointer in the global desktop space, or `None` where the system does not tell (Wayland).
/// Callable from any thread.
pub fn pointer_if_known() -> Option<LogicalPoint> {
    let (x, y) = platform::pointer_physical(app()?)?;
    Some(to_logical(x, y))
}

/// The pointer in the global desktop space; a point on no display where the system does not
/// tell. Callable from any thread.
pub fn pointer() -> LogicalPoint {
    pointer_if_known().unwrap_or(NOWHERE)
}

/// The work area (without the taskbar or the panels) of the screen that holds `point`, or of the
/// screen under the pointer, in global top-left points, and that screen's scale.
pub fn screen_area(point: Option<(f64, f64)>) -> Option<(LogicalRect, f64)> {
    let screens = screens();
    let at = point.map(|(x, y)| LogicalPoint { x, y }).or_else(pointer_if_known);
    let screen = match at {
        Some(at) => screen_at(&screens, at),
        None => primary(&screens),
    }?;
    Some((screen.visible, screen.scale()))
}

/// Puts an ordinary window in the middle of the work area of the screen that holds `at` (a
/// global top-left point), or of the screen under the pointer without one. With
/// `only_if_elsewhere`, a window whose middle already lies on that screen stays where the user
/// left it. A window wider or taller than the area shrinks to it. An ordinary window has a title
/// bar, so its outer size places it and only its position is set unless it has to shrink.
pub fn center_on_screen(window: &WebviewWindow, at: Option<LogicalPoint>, only_if_elsewhere: bool) {
    let screens = screens();
    let screen = match at.or_else(pointer_if_known) {
        Some(at) => screen_at(&screens, at),
        None => primary(&screens),
    };
    let Some(screen) = screen.cloned() else {
        return;
    };
    let read = window.clone();
    let facts = main_wait(move || {
        Some((read.outer_position().ok()?, read.outer_size().ok()?, read.inner_size().ok()?, read.scale_factor().ok()?))
    })
    .flatten();
    let Some((position, outer, inner, scale)) = facts else {
        return;
    };
    let scale = if scale > 0.0 { scale } else { 1.0 };
    if only_if_elsewhere {
        let middle = (
            f64::from(position.x) + f64::from(outer.width) / 2.0,
            f64::from(position.y) + f64::from(outer.height) / 2.0,
        );
        if distance(middle, screen.origin, screen.physical_size()) == 0.0 {
            return;
        }
    }
    // The window's size in points, which it keeps on a screen of another scale.
    let (outer_w, outer_h) = (f64::from(outer.width) / scale, f64::from(outer.height) / scale);
    let area = screen.visible;
    let (width, height) = (outer_w.min(area.width), outer_h.min(area.height));
    if width < outer_w || height < outer_h {
        let (inner_w, inner_h) = (f64::from(inner.width) / scale, f64::from(inner.height) / scale);
        let shrunk =
            tauri::LogicalSize::new((inner_w - (outer_w - width)).max(1.0), (inner_h - (outer_h - height)).max(1.0));
        let _ = window.set_size(shrunk);
    }
    let x = (area.x + (area.width - width) / 2.0).round();
    let y = (area.y + (area.height - height) / 2.0).round();
    let position = (
        screen.origin.0 + (x - screen.display.frame.x) * screen.window_scale(),
        screen.origin.1 + (y - screen.display.frame.y) * screen.window_scale(),
    );
    let position = tauri::PhysicalPosition::new(position.0.round() as i32, position.1.round() as i32);
    // On a screen of another scale Windows rescales the window as it lands, and tao's answer to
    // that would also move it on Windows 10; the corner is held natively there.
    #[cfg(target_os = "windows")]
    platform::place(window, position, None);
    #[cfg(not(target_os = "windows"))]
    let _ = window.set_position(position);
}

/// How often the monitors are compared while the platform has no notification of its own.
const DISPLAY_POLL: Duration = Duration::from_secs(2);

/// Calls `windows::displays_changed` after a display is connected, removed, moved or rescaled.
/// The platform's own notification is used when it has one; until then the monitors are compared
/// every two seconds, which costs the main thread one short read each time.
pub fn watch_displays(app: &AppHandle) {
    if platform::watch_displays(app, crate::windows::displays_changed) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let mut last = monitors();
        loop {
            std::thread::sleep(DISPLAY_POLL);
            let now = monitors();
            // An empty list is a read the main thread did not answer in time, not a change.
            if !now.is_empty() && now != last {
                last = now;
                crate::windows::displays_changed(&app);
            }
        }
    });
}

// ------------------------------------------------------------------------- showing windows

/// What the shell remembers about one of its panels.
#[derive(Default)]
struct PanelState {
    /// The window was shown at least once. GTK makes a window's surface when it is first shown,
    /// and tao's click-through call panics on a window without one, so it waits until then.
    shown: bool,
    /// The click-through asked for before the first show.
    through: Option<bool>,
    /// The shell wants the window on screen now, whatever [`hide_for_capture`] did meanwhile.
    wanted: bool,
}

static PANELS: Mutex<BTreeMap<String, PanelState>> = Mutex::new(BTreeMap::new());

/// Shows a window; `active` gives it the keyboard focus, and without it the window that has the
/// focus keeps it.
fn reveal(window: &WebviewWindow, active: bool) {
    lock(&PANELS).entry(window.label().to_string()).or_default().wanted = true;
    let window = window.clone();
    on_main(move || {
        if active {
            // Linux's override-redirect panels get the keyboard from the shell, not from a window
            // manager (`platform::show_active`).
            #[cfg(target_os = "linux")]
            platform::show_active(&window);
            #[cfg(not(target_os = "linux"))]
            {
                let _ = window.show();
                let _ = window.set_focus();
            }
        } else {
            platform::show_inactive(&window);
        }
        let mut panels = lock(&PANELS);
        let state = panels.entry(window.label().to_string()).or_default();
        state.shown = true;
        if let Some(through) = state.through.take() {
            let _ = window.set_ignore_cursor_events(through);
        }
    });
}

fn conceal(window: &WebviewWindow) {
    if let Some(state) = lock(&PANELS).get_mut(window.label()) {
        state.wanted = false;
    }
    let _ = window.hide();
}

/// Lets every click through a window to what lies below, or takes the clicks again. Asked for a
/// window that was never shown, it takes effect when the window first shows.
pub fn set_click_through(window: &WebviewWindow, through: bool) {
    let mut panels = lock(&PANELS);
    let state = panels.entry(window.label().to_string()).or_default();
    if state.shown {
        let _ = window.set_ignore_cursor_events(through);
    } else {
        state.through = Some(through);
    }
}

/// Destroys a panel and forgets what was remembered about it.
pub fn destroy_panel(window: WebviewWindow) {
    lock(&PANELS).remove(window.label());
    lock(&EXCLUDED).remove(window.label());
    let _ = window.destroy();
}

// ------------------------------------------------------------------ own windows and captures

/// The windows the system keeps out of captures and recordings for us.
static EXCLUDED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Marks one of Glimpsfy's own windows as never to appear in a capture or a recording. Called
/// once, when the window is made: the toast, the popover, the pins, the overlays and the
/// recorder's panels except the canvas, which a recording shows on purpose. Where the system
/// cannot do it (Linux, Windows before 10 2004), the window is one [`hide_for_capture`] hides.
pub fn exclude_from_capture(window: &WebviewWindow) {
    if platform::exclude_from_capture(window) {
        lock(&EXCLUDED).insert(window.label().to_string());
    }
}

/// Whether `label` is a window a capture must not show.
fn hidden_from_captures(label: &str) -> bool {
    label == crate::windows::TOAST
        || label == crate::popover::LABEL
        || label.starts_with(crate::pins::PREFIX)
        || [crate::recording::TARGET, crate::recording::BAR, crate::recording::BUBBLE_CONTROLS, crate::recording::RAIL]
            .contains(&label)
}

/// How long the compositor is given to take the hidden windows off the screen before the capture
/// reads it.
pub const HIDE_SETTLE: Duration = Duration::from_millis(120);

/// The windows [`hide_for_capture`] hid; dropping it shows them again.
#[derive(Default)]
pub struct CaptureGuard {
    hidden: Vec<WebviewWindow>,
}

impl CaptureGuard {
    /// Waits until the hidden windows have left the screen, when there are any. It sleeps, so it
    /// is called on the capture's own thread, never on the main thread.
    pub fn settle(&self) {
        if self.hid_any() {
            std::thread::sleep(HIDE_SETTLE);
        }
    }

    pub fn hid_any(&self) -> bool {
        !self.hidden.is_empty()
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        for window in self.hidden.drain(..) {
            // What the shell closed meanwhile stays closed, and the popover, which closes when it
            // loses the keyboard, is not brought back without it.
            let wanted = lock(&PANELS).get(window.label()).is_some_and(|state| state.wanted);
            if wanted && window.label() != crate::popover::LABEL {
                reveal(&window, false);
            }
        }
    }
}

/// Hides Glimpsfy's own windows that the system cannot keep out of a capture (the toast, the
/// pins, the popover, the recorder's bar, its countdown and its outline), except those in `keep`,
/// and gives the screen a moment to lose them. Dropping the guard shows them again without the
/// keyboard. Where every such window is excluded by the system this does nothing and costs
/// nothing. A still capture calls [`CaptureGuard::settle`] before it reads the screen.
pub fn hide_for_capture(app: &AppHandle, keep: &[&str]) -> CaptureGuard {
    let candidates: Vec<WebviewWindow> = {
        let excluded = lock(&EXCLUDED);
        app.webview_windows()
            .into_iter()
            .filter(|(label, _)| {
                hidden_from_captures(label) && !keep.contains(&label.as_str()) && !excluded.contains(label)
            })
            .map(|(_, window)| window)
            .collect()
    };
    if candidates.is_empty() {
        return CaptureGuard::default();
    }
    let hidden = main_wait(move || {
        candidates
            .into_iter()
            .filter(|window| window.is_visible().unwrap_or(false))
            .inspect(|window| {
                let _ = window.hide();
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    CaptureGuard { hidden }
}

// ---------------------------------------------------------------------------------- overlays

/// The window that had the keyboard before the first overlay of a capture took it.
static BEFORE_OVERLAY: Mutex<Option<platform::Foreground>> = Mutex::new(None);

/// One overlay window per display, each covering its display exactly: missing ones are made
/// hidden, moved displays get their window moved, and windows of removed displays go.
pub fn sync_overlays(app: &AppHandle) {
    refresh_screens();
    let app = app.clone();
    on_main(move || {
        let screens = screens();
        for screen in &screens {
            let label = crate::windows::overlay_label(screen.display.id);
            match app.get_webview_window(&label) {
                Some(window) => set_frame(&window, screen.display.frame),
                None => match crate::windows::build_panel(&app, &label, "Glimpsfy overlay") {
                    Ok(window) => setup_overlay(&window, screen),
                    Err(error) => {
                        trace::log(format!("[glimpsfy] overlay for display {}: {error}", screen.display.id.0))
                    }
                },
            }
        }
        for (display, window) in crate::windows::overlays(&app) {
            if !screens.iter().any(|s| s.display.id == display) {
                destroy_panel(window);
            }
        }
    });
}

/// What makes a window an overlay here: undecorated, above every other window, off the taskbar
/// (the builder's flags), covering its display exactly and hidden until a capture shows it.
fn setup_overlay(window: &WebviewWindow, screen: &Screen) {
    platform::prepare_panel(window);
    exclude_from_capture(window);
    set_frame(window, screen.display.frame);
    platform::cover_display(window);
}

/// Before a capture: the overlays exist for the displays as they are now. They stay hidden
/// while the frames are taken, because nothing here keeps a shown window out of them; a hidden
/// page still loads and draws its frame, and `show_overlay` shows it once it has.
pub fn prewarm_overlays(app: &AppHandle) {
    let known: BTreeSet<DisplayId> = screens().iter().map(|s| s.display.id).collect();
    let made: BTreeSet<DisplayId> = crate::windows::overlays(app).into_iter().map(|(display, _)| display).collect();
    if known != made {
        sync_overlays(app);
    }
}

/// Shows an overlay; `key` gives it the keyboard focus. The window that has the focus when the
/// first overlay of a capture shows is remembered, and gets it back in [`hide_overlays`].
pub fn show_overlay(window: &WebviewWindow, key: bool) {
    {
        let mut before = lock(&BEFORE_OVERLAY);
        if before.is_none() {
            *before = platform::foreground();
        }
    }
    reveal(window, key);
}

/// How often an overlay asks for the keyboard again when it did not get it: a window manager
/// that refuses must not be asked thirty times a second.
const KEY_RETRY: Duration = Duration::from_millis(400);

/// Gives a shown overlay the keyboard focus when anything else has it: another overlay (the
/// pointer moved onto its display) or another app. One that is hidden is left alone, and so is a
/// window in `keep`, which took the keyboard on purpose (the recorder's setup).
pub fn key_overlay(window: &WebviewWindow, keep: &[WebviewWindow]) {
    static LAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);
    {
        let last = lock(&LAST);
        if last.as_ref().is_some_and(|(label, at)| label == window.label() && at.elapsed() < KEY_RETRY) {
            return;
        }
    }
    let (window, keep) = (window.clone(), keep.to_vec());
    on_main(move || {
        if !window.is_visible().unwrap_or(false) || has_focus(&window).unwrap_or(true) {
            return;
        }
        if keep.iter().any(|w| has_focus(w).unwrap_or(false)) {
            return;
        }
        *lock(&LAST) = Some((window.label().to_string(), Instant::now()));
        #[cfg(target_os = "linux")]
        platform::focus(&window);
        #[cfg(not(target_os = "linux"))]
        let _ = window.set_focus();
    });
}

/// Hides one overlay (its page failed) and leaves the capture's focus as it is.
pub fn hide_overlay(window: &WebviewWindow) {
    conceal(window);
}

/// Hides every overlay. When one of them still has the keyboard, the window that had it before
/// the capture gets it back; when something else took it meanwhile (the recorder's setup, a save
/// panel, another app), it keeps it.
pub fn hide_overlays(app: &AppHandle) {
    let app = app.clone();
    on_main(move || {
        let windows = crate::windows::overlays(&app);
        let ours = windows.iter().any(|(_, window)| has_focus(window).unwrap_or(false));
        // Windows lets only the app that holds the foreground hand it on, and Glimpsfy stops
        // holding it once its overlay hides, so there the focus goes back first.
        #[cfg(target_os = "windows")]
        give_back_overlay_focus(ours);
        for (_, window) in &windows {
            conceal(window);
        }
        #[cfg(not(target_os = "windows"))]
        give_back_overlay_focus(ours);
    });
}

/// Gives the keyboard back to the window that had it before the capture, when an overlay still
/// has it (`ours`).
fn give_back_overlay_focus(ours: bool) {
    let before = lock(&BEFORE_OVERLAY).take();
    if let (true, Some(before)) = (ours, before) {
        platform::restore_foreground(before);
    }
}

/// Whether `window` has the keyboard focus, on the main thread. On Windows tao answers `false`
/// while the page inside has the keyboard, which is nearly always, so the system is asked there.
fn has_focus(window: &WebviewWindow) -> Option<bool> {
    #[cfg(target_os = "windows")]
    return Some(platform::is_foreground(window));
    #[cfg(not(target_os = "windows"))]
    window.is_focused().ok()
}

// ------------------------------------------------------------------------------------- toast

/// Where the toast window is and what it shows, for the click-through and the page's pointer.
#[derive(Default)]
struct ToastState {
    /// Where the window belongs, and where it was last put.
    rect: Option<LogicalRect>,
    placed: Option<LogicalRect>,
    /// Whether the page shows toasts, and whether the window is on screen.
    shown: bool,
    on_screen: bool,
    glass: Vec<ToastGlass>,
    through: Option<bool>,
}

static TOAST: Mutex<Option<ToastState>> = Mutex::new(None);

/// The toast: a window that never takes the keyboard focus and never shows in a capture.
pub fn setup_toast(window: &WebviewWindow) {
    platform::prepare_panel(window);
    let _ = window.set_focusable(false);
    exclude_from_capture(window);
}

/// Puts the toast window at the bottom-right corner of the work area of `display` (the primary
/// one without it), reaching `frame.past_right` and `frame.past_below` past it, as tall as the
/// work area, without the keyboard. With no toast to show the window hides, or waits on screen
/// click-through where the platform says so (`platform::TOAST_WAITS_ON_SCREEN`). The page calls
/// this on every frame of a move, so what is already right is left alone.
pub fn place_toast(window: &WebviewWindow, display: Option<DisplayId>, frame: ToastFrame) {
    let screens = screens();
    let screen = display.and_then(|id| screens.iter().find(|s| s.display.id == id)).or_else(|| primary(&screens));
    let Some(screen) = screen else {
        return;
    };
    let visible = screen.visible;
    let rect = LogicalRect {
        x: visible.x + visible.width + frame.past_right - frame.width,
        y: visible.y,
        width: frame.width,
        height: visible.height + frame.past_below,
    };
    let on_screen = frame.shown || platform::TOAST_WAITS_ON_SCREEN;
    let (moves, appears, leaves) = {
        let mut toast = lock(&TOAST);
        let state = toast.get_or_insert_with(ToastState::default);
        let was_on_screen = state.on_screen;
        state.rect = Some(rect);
        state.shown = frame.shown;
        state.on_screen = on_screen;
        // A hidden window is not moved; it gets its place when it next shows.
        let moves = on_screen && state.placed != Some(rect);
        if moves {
            state.placed = Some(rect);
        }
        (moves, on_screen && !was_on_screen, !on_screen && was_on_screen)
    };
    if moves {
        set_frame(window, rect);
    }
    if appears {
        reveal(window, false);
    }
    if leaves {
        conceal(window);
    }
    toast_hit(window);
}

/// Keeps where each toast is drawn (measured by the page from the window's bottom-right corner),
/// so clicks pass through the window everywhere but on a toast, and hands them to the platform
/// for a native blur where it has one.
pub fn toast_glass(window: &WebviewWindow, glass: &[ToastGlass]) {
    if let Some(state) = lock(&TOAST).as_mut() {
        state.glass = glass.to_vec();
    }
    platform::toast_glass(window, glass);
    toast_hit(window);
}

/// Lets clicks through the toast window everywhere but on a toast that is shown: the window is
/// as tall as the work area and reaches past the toasts for their shadows, and that room must
/// not catch the clicks of the apps below. Where the pointer cannot be read the window takes the
/// clicks while it shows toasts.
fn toast_hit(window: &WebviewWindow) {
    let pointer = pointer_if_known();
    let mut toast = lock(&TOAST);
    let Some(state) = toast.as_mut() else {
        return;
    };
    let Some(rect) = state.rect else {
        return;
    };
    let through = if !state.shown {
        true
    } else {
        match pointer {
            Some(pointer) => !state.glass.iter().any(|g| {
                let x = rect.x + rect.width - g.right - g.width;
                let y = rect.y + rect.height - g.bottom - g.height;
                g.alpha > 0.1
                    && pointer.x >= x
                    && pointer.x <= x + g.width
                    && pointer.y >= y
                    && pointer.y <= y + g.height
            }),
            None => false,
        }
    };
    if state.through != Some(through) {
        state.through = Some(through);
        set_click_through(window, through);
    }
}

/// The pointer in the toast window's own space (points from its top-left corner), or `None`
/// while the window shows no toast or the pointer cannot be read. The page asks often, so the
/// window's click-through is brought up to date here too.
pub fn pointer_in(window: &WebviewWindow) -> Option<LogicalPoint> {
    toast_hit(window);
    let rect = lock(&TOAST).as_ref().filter(|state| state.shown).and_then(|state| state.rect)?;
    let pointer = pointer_if_known()?;
    Some(LogicalPoint { x: pointer.x - rect.x, y: pointer.y - rect.y })
}

// ---------------------------------------------------------------------- popover and tray icon

/// The popover's fixed edge, so that it grows away from the icon when its page changes height.
#[derive(Clone, Copy, Debug)]
struct Anchor {
    x: f64,
    /// The top edge when `hangs` is true, the bottom edge otherwise.
    y: f64,
    /// Whether the popover hangs down from `y` (the taskbar is at the top) or stands on it.
    hangs: bool,
    area: LogicalRect,
}

static POPOVER: Mutex<Option<Anchor>> = Mutex::new(None);

/// The popover's first size, for the time before its frame can be read.
const POPOVER_SIZE: (f64, f64) = (340.0, 420.0);

/// The popover: a window that never shows in a capture. It takes the keyboard focus when it
/// opens and closes when it loses it (`popover.rs`), which here covers a click anywhere else.
pub fn setup_popover(window: &WebviewWindow) {
    platform::prepare_panel(window);
    exclude_from_capture(window);
}

fn popover_frame(anchor: &Anchor, width: f64, height: f64) -> LogicalRect {
    let area = anchor.area;
    let height = height.min(area.height);
    let x = anchor.x.min(area.x + area.width - width).max(area.x);
    let y = if anchor.hangs { anchor.y } else { anchor.y - height };
    LogicalRect { x, y: y.min(area.y + area.height - height).max(area.y), width, height }
}

/// Where the popover goes for a tray icon at `icon` on a screen whose work area is `area`: from
/// the icon towards the inside of the work area, `gap` points off the taskbar, whichever edge
/// the taskbar is on. An icon inside the work area (the overflow panel, an auto-hidden taskbar)
/// has the popover above it in the lower half of the screen and under it in the upper half.
fn anchor_for(icon: LogicalRect, area: LogicalRect, width: f64, gap: f64) -> Anchor {
    let (bottom, right) = (area.y + area.height, area.x + area.width);
    let centred = icon.x + (icon.width - width) / 2.0;
    let lower_half = icon.y + icon.height / 2.0 > area.y + area.height / 2.0;
    if icon.y >= bottom - 1.0 {
        Anchor { x: centred, y: bottom - gap, hangs: false, area }
    } else if icon.y + icon.height <= area.y + 1.0 {
        Anchor { x: centred, y: area.y + gap, hangs: true, area }
    } else if icon.x >= right - 1.0 {
        let y = if lower_half { icon.y + icon.height } else { icon.y };
        Anchor { x: right - gap - width, y, hangs: !lower_half, area }
    } else if icon.x + icon.width <= area.x + 1.0 {
        let y = if lower_half { icon.y + icon.height } else { icon.y };
        Anchor { x: area.x + gap, y, hangs: !lower_half, area }
    } else if lower_half {
        Anchor { x: centred, y: icon.y - gap, hangs: false, area }
    } else {
        Anchor { x: centred, y: icon.y + icon.height + gap, hangs: true, area }
    }
}

/// Puts the popover next to the tray icon, fully on the icon's screen, and gives it the keyboard
/// focus. Where the icon has no rectangle to read (Linux's AppIndicator, a hidden icon), it opens
/// at the pointer, and in the top-right corner of the primary screen when the pointer cannot be
/// read either.
pub fn show_popover(window: &WebviewWindow, tray: Option<&tauri::tray::TrayIcon>, gap: f64) {
    let screens = screens();
    let (width, height) = frame_of(window).map_or(POPOVER_SIZE, |frame| (frame.width, frame.height));
    let icon = tray.and_then(|tray| tray.rect().ok().flatten()).map(|rect| {
        let position = rect.position.to_physical::<f64>(1.0);
        let size = rect.size.to_physical::<f64>(1.0);
        let scale = screen_at_physical(&screens, (position.x, position.y)).map_or(1.0, Screen::window_scale);
        let at = to_logical(position.x, position.y);
        LogicalRect { x: at.x, y: at.y, width: size.width / scale, height: size.height / scale }
    });
    let anchor = match (icon, pointer_if_known()) {
        (Some(icon), _) => {
            let middle = LogicalPoint { x: icon.x + icon.width / 2.0, y: icon.y + icon.height / 2.0 };
            screen_at(&screens, middle).map(|screen| anchor_for(icon, screen.visible, width, gap))
        }
        (None, Some(pointer)) => screen_at(&screens, pointer).map(|screen| {
            let area = screen.visible;
            let hangs = pointer.y <= area.y + area.height / 2.0;
            Anchor { x: pointer.x - width / 2.0, y: if hangs { pointer.y + gap } else { pointer.y - gap }, hangs, area }
        }),
        (None, None) => primary(&screens).map(|screen| {
            let area = screen.visible;
            Anchor { x: area.x + area.width - width - gap, y: area.y + gap, hangs: true, area }
        }),
    };
    if let Some(anchor) = anchor {
        *lock(&POPOVER) = Some(anchor);
        set_frame(window, popover_frame(&anchor, width, height));
    }
    reveal(window, true);
}

/// The popover's page measured itself: the window takes `height` points and keeps the edge that
/// faces the icon.
pub fn fit_popover(window: &WebviewWindow, width: f64, height: f64) {
    let anchor = *lock(&POPOVER);
    match anchor {
        Some(anchor) => set_frame(window, popover_frame(&anchor, width, height)),
        None => {
            let _ = window.set_size(tauri::LogicalSize::new(width, height));
        }
    }
}

/// Whether the tray icon shows a recording now, so a theme change draws the right glyph again.
static TRAY_RECORDING: AtomicBool = AtomicBool::new(false);
/// The text the tray icon shows now.
static TRAY_TEXT: Mutex<Option<String>> = Mutex::new(None);

/// The tray glyph for this platform's panel as it is now (`platform::tray_glyph`), in the kit's
/// recording red while a take records. macOS tints one black template glyph itself; here a black
/// glyph would vanish on a dark taskbar.
pub fn tray_image(recording: bool) -> Option<tauri::image::Image<'static>> {
    let glyph = tauri::image::Image::from_bytes(platform::tray_glyph()).ok()?;
    if !recording {
        return Some(glyph);
    }
    let mut rgba = glyph.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[..3].copy_from_slice(&[0xFF, 0x3B, 0x30]);
    }
    Some(tauri::image::Image::new_owned(rgba, glyph.width(), glyph.height()))
}

/// Draws the tray icon as idle or as recording, when that is not what it shows already.
pub fn tray_look(app: &AppHandle, recording: bool) {
    if TRAY_RECORDING.swap(recording, Ordering::SeqCst) != recording {
        draw_tray(app);
    }
}

fn draw_tray(app: &AppHandle) {
    let recording = TRAY_RECORDING.load(Ordering::SeqCst);
    if let (Some(icon), Some(image)) = (crate::tray::icon(app), tray_image(recording)) {
        let _ = icon.set_icon(Some(image));
    }
}

/// Shows a short text with the tray icon (a countdown's seconds, a take's elapsed time), or
/// takes it away. Linux shows it beside the icon as macOS does; Windows has no such label, so
/// there it is the icon's tooltip.
pub fn tray_text(app: &AppHandle, text: Option<String>) {
    {
        let mut shown = lock(&TRAY_TEXT);
        if *shown == text {
            return;
        }
        shown.clone_from(&text);
    }
    let Some(icon) = crate::tray::icon(app) else {
        return;
    };
    let tooltip = match &text {
        Some(text) => format!("Glimpsfy · {text}"),
        None => "Glimpsfy".to_string(),
    };
    let _ = icon.set_tooltip(Some(tooltip));
    let _ = icon.set_title(text);
}

/// Follows the panel's theme where the glyph depends on it (the Windows taskbar): the icon is
/// drawn again when it changes. Once, after the tray icon is made.
pub fn watch_tray_theme(app: &AppHandle) {
    let app = app.clone();
    platform::watch_panel_theme(Box::new(move || {
        let app = app.clone();
        on_main(move || draw_tray(&app));
    }));
}

// -------------------------------------------------------------------------------------- pins

/// A pin: above other apps' windows, never in a capture, at `frame`, shown without taking the
/// keyboard focus from the app the user is in.
pub fn setup_pin(window: &WebviewWindow, frame: LogicalRect) {
    platform::prepare_panel(window);
    exclude_from_capture(window);
    set_frame(window, frame);
    reveal(window, false);
}

/// Whether the key that wakes a locked pin is held now: Alt, which stands where ⌥ does. `false`
/// where the system tells no app the keys other apps receive. Callable from any thread.
pub fn option_key_down() -> bool {
    platform::option_key_down()
}

// --------------------------------------------------------------------------------- clipboard

/// Puts plain text on the clipboard.
pub fn copy_text(text: &str) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let app = app().ok_or("the app is not running yet")?;
    app.clipboard().write_text(text.to_string()).map_err(|error| error.to_string())
}

/// Puts one image on the clipboard from its pixels. The clipboard plugin takes one image, so of
/// the images of all displays only the first is copied here.
pub fn copy_frames(frames: &[&glimpsfy_capture::Frame]) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let Some(frame) = frames.first() else {
        return Ok(());
    };
    let app = app().ok_or("the app is not running yet")?;
    let image = tauri::image::Image::new_owned(frame.to_rgba(), frame.size.width, frame.size.height);
    app.clipboard().write_image(&image).map_err(|error| error.to_string())
}

/// Puts a PNG on the clipboard as an image.
pub fn copy_png(png: &[u8]) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let app = app().ok_or("the app is not running yet")?;
    let image = tauri::image::Image::from_bytes(png).map_err(|error| error.to_string())?;
    app.clipboard().write_image(&image).map_err(|error| error.to_string())
}

/// Puts a PNG or a JPEG on the clipboard. The clipboard plugin takes pixels, and nothing in the
/// shell decodes a JPEG, so a JPEG goes as the file itself is asked to go, by the caller.
pub fn copy_image(bytes: &[u8], jpeg: bool) -> Result<(), String> {
    // Windows decodes the JPEG itself (`platform::jpeg_rgba`) and hands the plugin its pixels.
    #[cfg(target_os = "windows")]
    if jpeg {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        let app = app().ok_or("the app is not running yet")?;
        let (rgba, width, height) = platform::jpeg_rgba(bytes)?;
        let image = tauri::image::Image::new_owned(rgba, width, height);
        return app.clipboard().write_image(&image).map_err(|error| error.to_string());
    }
    // Linux hands the JPEG to GdkPixbuf and GTK's clipboard (`platform::copy_jpeg`).
    #[cfg(target_os = "linux")]
    if jpeg {
        return platform::copy_jpeg(bytes);
    }
    if jpeg {
        return Err("a JPEG cannot be put on the clipboard as an image on this platform yet".into());
    }
    copy_png(bytes)
}

/// Puts the file itself on the clipboard, so pasting into a file manager, a mail or a chat app
/// adds the recording or the image (`platform::copy_file`).
pub fn copy_file(path: &Path) -> Result<(), String> {
    platform::copy_file(path)
}

/// The image the clipboard holds (`clipboard.rs`): the first image file among the files it names
/// (`platform::clipboard_files`), or else its picture as a PNG. When it names any file its picture
/// is never read, because a file manager may put the file's icon there. The plugin hands the
/// picture over as RGBA without a colour profile, so the PNG is sRGB, and it takes the pixels per
/// point of the screen under the pointer. Not on the main thread, which Linux's files come through.
pub fn read_image() -> Option<crate::clipboard::Pasted> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let files = platform::clipboard_files();
    if !files.is_empty() {
        return crate::clipboard::first_image(files);
    }
    let image = app()?.clipboard().read_image().ok()?;
    let (width, height) = (image.width(), image.height());
    let scale = screen_area(None).map(|(_, scale)| scale).unwrap_or(1.0);
    let frame = glimpsfy_capture::Frame::new(
        PhysicalSize::new(width, height),
        width as usize * 4,
        glimpsfy_capture::PixelFormat::Rgba8,
        glimpsfy_capture::ColorInfo::for_space(ColorSpace::Srgb),
        ScaleFactor(scale),
        image.rgba().to_vec(),
    )
    .ok()?;
    frame.encode_png(glimpsfy_capture::PngEffort::Fast).ok().map(crate::clipboard::Pasted::Image)
}

/// Starts dragging `file` out of `window`, with `image` under the pointer at `size` points
/// (`platform::drag_out`).
pub fn drag_out(window: &WebviewWindow, file: &Path, image: &Path, size: (f64, f64)) -> Result<(), String> {
    platform::drag_out(window, file, image, size)
}

/// Plays the system's capture sound where it has one (`platform::shutter`).
pub fn shutter() {
    platform::shutter();
}

/// Brings Glimpsfy to the front before it opens a file panel. The dialog plugin's panels are
/// windows of their own here and come to the front by themselves, so nothing is needed yet.
pub fn bring_forward(_app: &AppHandle) {}

// ------------------------------------------------------------------------------------ system

/// The process id of the app whose window has the keyboard focus, where the system tells.
pub fn frontmost_pid() -> Option<i32> {
    platform::frontmost_pid()
}

/// The user's preferred languages, most wanted first, as tags such as `tr-TR` or `pt_BR`.
pub fn preferred_languages() -> Vec<String> {
    platform::preferred_languages()
}

/// Whether an app on this computer opens `url` (`platform::can_open_url`).
pub fn can_open_url(url: &str) -> bool {
    platform::can_open_url(url)
}

/// Opens a pane of the system's settings that is not a permission's; `false` when this platform
/// has no such pane.
pub fn open_system_settings(app: &AppHandle, pane: SystemPane) -> Result<bool, String> {
    use tauri_plugin_opener::OpenerExt;
    match platform::system_settings_url(pane) {
        Some(url) => app.opener().open_url(url, None::<&str>).map(|()| true).map_err(|error| error.to_string()),
        None => Ok(false),
    }
}

/// Opens the pane of the system's settings where `permission` is granted, when there is one.
pub fn open_permission_settings(app: &AppHandle, permission: Permission) {
    use tauri_plugin_opener::OpenerExt;
    if let Some(url) = platform::permission_settings_url(permission) {
        let _ = app.opener().open_url(url, None::<&str>);
    }
}

/// Start at login, through an entry the shell writes itself (`platform::set_login_item`): an XDG
/// autostart entry on Linux, and on Windows the current user's Run value under the autostart
/// plugin's name, which leaves the path unquoted and writes for every user from an elevated copy.
pub fn set_login_item(app: &AppHandle, on: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return platform::set_login_item(app, on);
    #[cfg(target_os = "windows")]
    platform::set_login_item(app, on)
}

// ------------------------------------------------------------------------------------- linux

/// Before GTK starts: GTK goes on its X11 backend, and a process without any display says so
/// and exits (`platform::prepare_environment`). First thing in `run`.
#[cfg(target_os = "linux")]
pub fn prepare_environment() {
    platform::prepare_environment();
}

/// A Linux session's start, after the tray icon and the windows are made
/// (`platform::start`).
#[cfg(target_os = "linux")]
pub fn start(app: &AppHandle) {
    platform::start(app);
}

/// Whether anything on this desktop shows the tray icon (`platform::tray_host_present`). It asks
/// the session bus, so it is called off the main thread.
#[cfg(target_os = "linux")]
pub fn tray_host_present() -> bool {
    platform::tray_host_present()
}

/// Whether the last look for a tray host found none (`platform::tray_host_absent`), without
/// asking the bus again.
#[cfg(target_os = "linux")]
pub fn tray_host_absent() -> bool {
    platform::tray_host_absent()
}

#[cfg(target_os = "linux")]
pub use platform::{bind_portal_shortcuts, close_on_ctrl_w, portal_trigger, PortalShortcut};

/// A release build, which is what an installer or a package puts on a machine. A development
/// run never asks for updates and never writes the login item again.
pub fn is_bundled() -> bool {
    !cfg!(debug_assertions)
}

/// Whether this copy can replace itself with a downloaded update (`platform::can_self_update`).
/// Where it cannot (a `.deb` or `.rpm` install), the updater only reports the newer version.
pub fn can_self_update() -> bool {
    platform::can_self_update()
}

// -------------------------------------------------------------------------------- permissions

/// The permissions of `permissions.rs` on Windows and Linux. Neither system asks before a screen
/// capture or has an accessibility switch, so those read as `NotRequired`.
pub mod permissions {
    use glimpsfy_capture::PermissionState;
    use tauri::AppHandle;

    use super::platform;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Media {
        Audio,
        Video,
    }

    pub fn accessibility() -> PermissionState {
        PermissionState::NotRequired
    }

    pub fn request_accessibility() -> PermissionState {
        PermissionState::NotRequired
    }

    /// The probe's answer (`permissions::probe`): no permission stands between an app and the
    /// screen here.
    pub fn screen_recording_granted() -> bool {
        true
    }

    /// No permission is granted to a later process only, so a restart is never needed.
    pub fn granted_to_a_new_process() -> bool {
        false
    }

    /// Whether the system lets desktop apps use the microphone or the camera
    /// (`platform::media_permission`).
    pub fn media(media: Media) -> PermissionState {
        platform::media_permission(media)
    }

    /// Neither system has a prompt an app can raise: the answer is the state as it is, and the
    /// user changes it in the system's settings (`open_permission_settings`).
    pub fn request_media(_app: &AppHandle, which: Media) -> PermissionState {
        media(which)
    }
}

// ---------------------------------------------------------------------------- screen recording

/// The recorder's panels, the webcam bubble, the click and key watch and the window ids the
/// capture filter needs: what `macos::recording` gives `recording/mod.rs`.
pub mod recording {
    use std::path::Path;
    use std::sync::Mutex;

    use glimpsfy_capture::{DisplayId, LogicalPoint, LogicalRect, WindowId};
    use glimpsfy_record::session::OwnWindows;
    use tauri::{AppHandle, Manager, WebviewWindow};

    use super::{exclude_from_capture, lock, platform, reveal, set_click_through, set_frame, EXCLUDED};
    use crate::recording::setup::GlassPiece;

    /// The recorder's webview panels.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Panel {
        /// The target over a whole display: the setup, the countdown and the red frame.
        Target,
        /// The setup and control bars.
        Bar,
        /// The recorded canvas: click rings and key caps, click-through.
        Canvas,
        /// The webcam bubble's hover controls.
        BubbleControls,
    }

    /// The window that had the keyboard before the recorder's target took it for the setup and
    /// the countdown; it gets the keyboard back when the take goes live.
    static BEFORE_TARGET: Mutex<Option<platform::Foreground>> = Mutex::new(None);

    /// Makes a `rec-*` window a panel of the recorder: never in a capture, except the canvas,
    /// which the recording shows on purpose. Only the target and the bars take the keyboard, and
    /// the canvas lets every click through.
    pub fn setup_panel(window: &WebviewWindow, panel: Panel) {
        platform::prepare_panel(window);
        if panel != Panel::Canvas {
            exclude_from_capture(window);
        }
        if matches!(panel, Panel::Canvas | Panel::BubbleControls) {
            let _ = window.set_focusable(false);
        }
        if panel == Panel::Canvas {
            set_click_through(window, true);
        }
    }

    /// Puts a panel at `frame` (global top-left points) and shows it; `key` gives it the
    /// keyboard focus, `through` lets every click pass to the app below.
    pub fn show(window: &WebviewWindow, frame: LogicalRect, key: bool, through: bool) {
        if key {
            let mut before = lock(&BEFORE_TARGET);
            if before.is_none() {
                // A setup opened from a capture's overlay goes back to the app the capture left.
                *before = lock(&super::BEFORE_OVERLAY).take().or_else(platform::foreground);
            }
        }
        set_frame(window, frame);
        reveal(window, key);
        set_click_through(window, through);
    }

    pub fn hide(window: &WebviewWindow) {
        if window.label() == crate::recording::TARGET {
            *lock(&BEFORE_TARGET) = None;
        }
        super::conceal(window);
    }

    /// The take goes live: the target keeps the red frame, lets every click through and hands
    /// the keyboard back to the app the user records. Where the system cannot keep the target
    /// out of the recording, it hides instead, so the file shows no frame around the area; the
    /// answer says so, and the take is armed once the screen has lost it.
    pub fn pass_through(window: &WebviewWindow) -> bool {
        let concealed = !lock(&EXCLUDED).contains(window.label());
        if concealed {
            super::conceal(window);
        } else {
            set_click_through(window, true);
        }
        let before = lock(&BEFORE_TARGET).take();
        if let Some(before) = before {
            super::on_main(move || platform::restore_foreground(before));
        }
        concealed
    }

    /// Lets the bar's window take the mouse (the pointer is over its glass) or pass it to what
    /// lies below (the pointer is over the stage's empty part).
    pub fn take_mouse(window: &WebviewWindow, takes: bool) {
        set_click_through(window, !takes);
    }

    /// Puts a native blur and the kit's shadow under each piece of the bar's glass, where the
    /// platform has them (`platform::backdrop_show`).
    pub fn backdrop_show(bar: &WebviewWindow, frame: LogicalRect, pieces: &[GlassPiece]) {
        platform::backdrop_show(bar, frame, pieces);
    }

    pub fn backdrop_hide() {
        platform::backdrop_hide();
    }

    /// The capture backend's ids of the app's own windows: those the file must not show, and
    /// those it shows (the canvas and the bubble). Empty where the platform tells no ids; the
    /// capture exclusion and [`super::hide_for_capture`] keep the windows out then.
    pub fn own_windows(app: &AppHandle) -> OwnWindows {
        let mut own = OwnWindows::default();
        for (label, window) in app.webview_windows() {
            let Some(id) = platform::window_id(&window) else {
                continue;
            };
            if label == crate::recording::CANVAS {
                own.shown.push(id);
            } else {
                own.hidden.push(id);
            }
        }
        own.shown.extend(bubble_window_id());
        own
    }

    /// The work area of a display, in global top-left points.
    pub fn visible_frame(display: DisplayId) -> Option<LogicalRect> {
        super::screens().into_iter().find(|s| s.display.id == display).map(|s| s.visible)
    }

    /// The pointer and whether the left button is down, or `None` where the system does not tell
    /// where the pointer is: the watch thread has nothing to follow then.
    pub fn pointer_and_button() -> Option<(LogicalPoint, bool)> {
        Some((super::pointer_if_known()?, platform::left_button_down()))
    }

    /// Whether the pointer is over one of `rects` (global points). Where the pointer cannot be
    /// read the answer is yes, so the bar keeps taking the mouse.
    pub fn pointer_over(rects: &[LogicalRect]) -> bool {
        super::pointer_if_known().is_none_or(|pointer| rects.iter().any(|r| r.contains(pointer)))
    }

    /// What a take's click and key watch reports.
    // Nothing reads the fields until a platform's watch is written.
    #[allow(dead_code)]
    pub struct InputWatch {
        pub clicks: bool,
        pub keys: bool,
        pub only_shortcuts: bool,
        pub on_click: fn(&AppHandle, LogicalPoint),
        pub on_keys: fn(&AppHandle, Vec<String>),
    }

    /// Watches clicks and keys in every other app while a take runs, for the click ring and the
    /// key caps (`platform::watch_input`).
    pub fn watch_input(app: &AppHandle, watch: InputWatch) {
        platform::watch_input(app, watch);
    }

    pub fn unwatch_input() {
        platform::unwatch_input();
    }

    /// Shows the webcam bubble at `frame` with the camera `device`, round or with rounded
    /// corners (`platform::bubble_show`).
    pub fn bubble_show(device: Option<&str>, frame: LogicalRect, round: bool) -> Result<(), String> {
        platform::bubble_show(device, frame, round)
    }

    /// Moves the bubble to `frame`, sliding when `animate`.
    pub fn bubble_move(frame: LogicalRect, round: bool, animate: bool) {
        platform::bubble_move(frame, round, animate);
    }

    /// Hides the bubble and turns the camera off.
    pub fn bubble_hide() {
        platform::bubble_hide();
    }

    fn bubble_window_id() -> Option<WindowId> {
        platform::bubble_window_id()
    }

    /// Puts the file itself on the clipboard.
    pub fn copy_file(path: &Path) -> Result<(), String> {
        super::copy_file(path)
    }
}
