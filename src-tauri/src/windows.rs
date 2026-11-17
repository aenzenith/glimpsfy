//! Every window of the app: the warm overlay pool (one per display), the toast, and the
//! ordinary windows opened on demand (settings, history, editor, first run…).
//!
//! All windows load the same `index.html`; `src/main.ts` mounts the page that belongs to the
//! window's label (`overlay-<display id>`, `toast`, `settings`…). Windows are made in code, never
//! in `tauri.conf.json`, because the overlays depend on the displays connected at run time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use glimpsfy_capture::DisplayId;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, EventTarget, Listener, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::capture::CaptureId;
use crate::commands::error::{CommandError, ErrorCode};
use crate::commands::windows::WindowKind;
use crate::{events, session, trace, AppState};

pub const OVERLAY_PREFIX: &str = "overlay-";
pub const TOAST: &str = "toast";

pub fn overlay_label(display: DisplayId) -> String {
    format!("{OVERLAY_PREFIX}{}", display.0)
}

pub fn overlay_display(label: &str) -> Option<DisplayId> {
    label.strip_prefix(OVERLAY_PREFIX)?.parse().ok().map(DisplayId)
}

fn overlays(app: &AppHandle) -> Vec<(DisplayId, WebviewWindow)> {
    app.webview_windows().into_iter().filter_map(|(label, w)| Some((overlay_display(&label)?, w))).collect()
}

/// Makes the windows that wait for use and starts watching the displays. Once, at start, on the
/// main thread.
pub fn setup(app: &AppHandle) {
    watch_selections(app);
    #[cfg(target_os = "macos")]
    {
        sync_overlays(app);
        if let Ok(toast) = build_panel(app, TOAST, "Glimpsfy") {
            crate::macos::on_main(app, move |mtm| crate::macos::setup_toast(&toast, mtm));
        }
        crate::macos::watch_displays(app);
    }
}

/// A borderless, transparent, hidden webview window for a panel (an overlay or the toast).
fn build_panel(app: &AppHandle, label: &str, title: &str) -> tauri::Result<WebviewWindow> {
    let build = || {
        WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
            .title(title)
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
            .build()
    };
    #[cfg(target_os = "macos")]
    return crate::macos::without_activating(build);
    #[cfg(not(target_os = "macos"))]
    build()
}

// ---------------------------------------------------------------------------------- overlays

/// One overlay window per display, each covering its display: missing ones are made, moved
/// displays get their window moved, and windows of removed displays go.
#[cfg(target_os = "macos")]
pub fn sync_overlays(app: &AppHandle) {
    let handle = app.clone();
    crate::macos::on_main(app, move |mtm| {
        let screens = crate::macos::screens(mtm);
        for screen in &screens {
            let label = overlay_label(screen.id);
            match handle.get_webview_window(&label) {
                Some(window) => crate::macos::fit_overlay(&window, screen, mtm),
                None => match build_panel(&handle, &label, "Glimpsfy overlay") {
                    Ok(window) => crate::macos::setup_overlay(&window, screen, mtm),
                    Err(error) => trace::log(format!("[glimpsfy] overlay for display {}: {error}", screen.id.0)),
                },
            }
        }
        for (display, window) in overlays(&handle) {
            if !screens.iter().any(|s| s.id == display) {
                crate::macos::destroy_panel(&handle, window);
            }
        }
    });
}

/// Called after the display layout changed: a running capture goes on without a removed display
/// and is cancelled after any other change (`session::displays_changed`), the overlay pool
/// follows the displays and the pages hear about it.
pub fn displays_changed(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.backend.invalidate();
    session::displays_changed(app);
    #[cfg(target_os = "macos")]
    sync_overlays(app);
    if let Ok(displays) = state.backend.displays() {
        let _ = app.emit(events::DISPLAYS_CHANGED, displays);
    }
    let backend = state.backend.clone();
    std::thread::spawn(move || backend.prepare());
}

/// Orders every overlay in, invisible and click-through, while the capture runs.
pub fn prewarm_overlays(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        crate::macos::on_main(app, move |mtm| {
            for (_, window) in overlays(&handle) {
                crate::macos::prewarm_overlay(&window, mtm);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Shows one display's overlay; `key` gives it the keyboard.
pub fn show_overlay(app: &AppHandle, display: DisplayId, key: bool) {
    let Some(window) = app.get_webview_window(&overlay_label(display)) else {
        return;
    };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::show_overlay(&window, key, mtm));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = window.show();
        if key {
            let _ = window.set_focus();
        }
    }
}

pub fn hide_overlays(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        crate::macos::on_main(app, move |mtm| {
            for (_, window) in overlays(&handle) {
                crate::macos::hide_overlay(&window, mtm);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    for (_, window) in overlays(app) {
        let _ = window.hide();
    }
}

/// While a capture is open, the overlay under the pointer takes the keyboard, so Esc and the
/// space bar act on the display the user is looking at, and hover reaches its page. Once a
/// display holds a selection, its overlay keeps the keyboard wherever the pointer goes, so Copy,
/// Save and the arrow keys still reach the selection from another display.
pub fn follow_pointer(app: &AppHandle, id: CaptureId) {
    #[cfg(target_os = "macos")]
    {
        let app = app.clone();
        std::thread::spawn(move || {
            let mut last = None;
            loop {
                std::thread::sleep(Duration::from_millis(30));
                let state = app.state::<AppState>();
                let Some(session) = state.sessions.get(id) else {
                    return;
                };
                let pointer = crate::macos::pointer();
                let under = session.displays.iter().find(|d| d.display.frame.contains(pointer)).map(|d| d.display.id);
                let holder = session.holder();
                let Some(display) = holder.or(under) else {
                    continue;
                };
                // A click makes the panel under it key on its own (AppKit), so while the pointer
                // is away from the selection the keyboard is taken back at every check; the call
                // does nothing while the holder is still key.
                let away = holder.is_some() && under != holder;
                if (last.is_some() && last != Some(display)) || away {
                    if let Some(window) = app.get_webview_window(&overlay_label(display)) {
                        crate::macos::on_main(&app, move |mtm| crate::macos::key_overlay(&window, mtm));
                    }
                }
                last = Some(display);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, id);
}

/// The overlay pages say which display holds the selection; [`follow_pointer`] keeps the
/// keyboard there.
fn watch_selections(app: &AppHandle) {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Selection {
        capture_id: CaptureId,
        display_id: DisplayId,
    }
    for (name, held) in [(events::OVERLAY_SELECTION_STARTED, true), (events::OVERLAY_SELECTION_ENDED, false)] {
        let handle = app.clone();
        app.listen_any(name, move |event| {
            let Ok(selection) = serde_json::from_str::<Selection>(event.payload()) else {
                return;
            };
            if let Some(session) = handle.state::<AppState>().sessions.get(selection.capture_id) {
                session.hold(selection.display_id, held);
            }
        });
    }
}

// ------------------------------------------------------------------------------------ toasts

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ToastKind {
    Copied,
    Saved,
    Failed,
    /// Recognised text reached the clipboard; `text` holds it, and the toast quotes its first words.
    TextCopied,
    /// A recording stopped because the disk was almost full, and what was recorded is saved at
    /// `path`. One toast says both, so the user never sees "Saved" and then a failure.
    DiskFull,
}

/// One toast; the toast page words it in the user's language from `kind` and the details.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    /// A small PNG of the capture as a data URL.
    pub thumbnail: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: Option<u64>,
    /// The saved file, for Show in Finder.
    pub path: Option<String>,
    /// The folder's own name, for "Saved to …".
    pub folder: Option<String>,
    pub error_code: Option<ErrorCode>,
    /// The failure's English message, for the log line under the title.
    pub message: Option<String>,
    /// The copied text of a `TextCopied` toast.
    pub text: Option<String>,
    /// The file format of a `Copied` or `Saved` toast: "png", "jpeg" or "webp" for a capture,
    /// "mp4" or "gif" for a recording or an export.
    pub format: Option<&'static str>,
    /// The save folder that could not take the file, so it went to Pictures › Glimpsfy instead;
    /// the toast names it (capture area).
    pub fallback_from: Option<String>,
}

impl Toast {
    pub fn new(kind: ToastKind) -> Toast {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Toast {
            id: NEXT.fetch_add(1, Ordering::SeqCst),
            kind,
            thumbnail: None,
            width: None,
            height: None,
            bytes: None,
            path: None,
            folder: None,
            error_code: None,
            message: None,
            text: None,
            format: None,
            fallback_from: None,
        }
    }
}

/// How the toast page wants its window: `width` points wide, with its bottom-right corner
/// `past_right` points right of and `past_below` points under the bottom-right corner of the
/// display's visible area, and as tall as that area and `past_below`. The window reaches past the
/// visible area so the toasts' shadows are not cut at its edge, over the Dock and the screen's
/// edge, and it keeps one size while toasts come and go, so it never resizes under a moving one;
/// clicks go through it everywhere but on a toast (`macos::toast_hit`). It stays on screen,
/// invisible while `shown` is false, so its page keeps drawing at full speed and the first toast
/// moves as smoothly as the others.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastFrame {
    pub width: f64,
    pub past_right: f64,
    pub past_below: f64,
    pub shown: bool,
}

/// Where one toast is drawn in the toast window at this frame, measured by the page from the
/// window's bottom-right corner (the stack's anchor). The core puts the toast's native blur there
/// (`macos::toast_glass`); the page moves the toasts itself and reports every frame of a move, so
/// the blur follows.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastGlass {
    /// The toast's key in the page, which keeps its blur while it moves.
    pub key: String,
    pub right: f64,
    pub bottom: f64,
    pub width: f64,
    pub height: f64,
    /// The toast's corner radius, which the blur's mask follows.
    pub radius: f64,
    /// The toast's opacity, 0 to 1, which its blur takes while it comes and goes.
    pub alpha: f64,
}

/// The display the toasts stack on: the one the last capture was taken on.
static TOAST_DISPLAY: Mutex<Option<DisplayId>> = Mutex::new(None);

pub fn toast(app: &AppHandle, display: Option<DisplayId>, toast: Toast) {
    if display.is_some() {
        *TOAST_DISPLAY.lock().unwrap_or_else(|p| p.into_inner()) = display;
    }
    let _ = app.emit_to(EventTarget::webview_window(TOAST), events::TOAST_SHOW, toast);
}

pub fn toast_failure(app: &AppHandle, display: Option<DisplayId>, error: &CommandError) {
    let mut failure = Toast::new(ToastKind::Failed);
    failure.error_code = Some(error.code);
    failure.message = Some(error.message.clone());
    toast(app, display, failure);
}

/// The toast page drew a frame: the window takes `frame` at the bottom-right corner of the
/// capture's display, where the stack grows upward from above the Dock, and each toast in
/// `glass` gets its native blur.
pub fn toast_fit(app: &AppHandle, frame: ToastFrame, glass: Vec<ToastGlass>) {
    let Some(window) = app.get_webview_window(TOAST) else {
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let display = *TOAST_DISPLAY.lock().unwrap_or_else(|p| p.into_inner());
        crate::macos::on_main(app, move |mtm| {
            let screens = crate::macos::screens(mtm);
            let screen = display.and_then(|d| screens.iter().find(|s| s.id == d)).or(screens.first());
            if let Some(screen) = screen {
                crate::macos::place_toast(&window, screen, frame, mtm);
                crate::macos::toast_glass(&window, &glass, mtm);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, glass);
        let _ = if frame.shown { window.show() } else { window.hide() };
    }
}

/// The pointer in the toast window's own space (points from its top-left corner), or `None` while
/// the window is off screen. The toast's panel never becomes key, and WebKit follows the pointer
/// only in a key window: the page learns about a click and nothing else, so it asks here where
/// the pointer is to pause its timers and to light the control under the pointer.
pub fn toast_pointer(app: &AppHandle) -> Option<glimpsfy_capture::LogicalPoint> {
    let window = app.get_webview_window(TOAST)?;
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::macos::on_main(app, move |mtm| {
            let _ = tx.send(crate::macos::pointer_in(&window, mtm));
        });
        rx.recv_timeout(std::time::Duration::from_millis(250)).ok().flatten()
    }
    #[cfg(not(target_os = "macos"))]
    {
        if !window.is_visible().unwrap_or(false) {
            return None;
        }
        let pointer = window.cursor_position().ok()?;
        let origin = window.inner_position().ok()?;
        let scale = window.scale_factor().ok()?;
        Some(glimpsfy_capture::LogicalPoint {
            x: (pointer.x - f64::from(origin.x)) / scale,
            y: (pointer.y - f64::from(origin.y)) / scale,
        })
    }
}

// -------------------------------------------------------------------------- ordinary windows

/// Opens (or brings forward) one of the app's ordinary windows on the active screen
/// (`active_point`). A new window is made hidden and shown once it is in place, so it never
/// flashes on the main screen first; an open window moves only when it sits on another screen.
pub fn open(app: &AppHandle, kind: WindowKind) {
    let label = kind.label();
    // Read before the window is made or shown, so it never counts itself as the active one.
    let at = active_point(app);
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        bring_forward(app, window, at, true);
        return;
    }
    let (width, height) = kind.size();
    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .title(kind.title())
        .inner_size(width, height)
        .min_inner_size(width.min(480.0), height.min(360.0))
        // Where the window stays when no screen can be read.
        .center()
        .visible(false)
        .build();
    match built {
        Ok(window) => bring_forward(app, window, at, false),
        Err(error) => trace::log(format!("[glimpsfy] window {label}: {error}")),
    }
}

/// The middle of the window the user works in: the front ordinary window (layer 0) of the app in
/// front, Glimpsfy's own included, or the front ordinary window of any app when that app shows
/// none (the desktop was clicked last). `None` sends the window to the pointer's screen.
///
/// The pointer alone cannot say it: when the displays share one Space ("Displays have separate
/// Spaces" is off), the menu bar lives only on the main display, so every click on the icon, its
/// menu or the popover leaves the pointer there, whichever screen the user works on.
#[cfg(target_os = "macos")]
fn active_point(app: &AppHandle) -> Option<glimpsfy_capture::LogicalPoint> {
    let options =
        glimpsfy_capture::WindowListOptions { on_screen_only: true, exclude_desktop: true, exclude_own: false };
    let listed = app.state::<AppState>().backend.windows(options).ok()?;
    // Small layer-0 windows are not where the user works: a browser's hover cards and bubbles,
    // some apps' invisible key catchers. A 270 × 80 one of Chrome's came first in a test.
    let ordinary: Vec<_> = listed
        .iter()
        .filter(|w| w.layer == 0 && w.alpha > 0.0 && w.bounds.width >= 160.0 && w.bounds.height >= 120.0)
        .collect();
    let front = crate::macos::frontmost_pid();
    let window = ordinary.iter().find(|w| Some(w.pid) == front).or_else(|| ordinary.first())?;
    Some(glimpsfy_capture::LogicalPoint {
        x: window.bounds.x + window.bounds.width / 2.0,
        y: window.bounds.y + window.bounds.height / 2.0,
    })
}

#[cfg(not(target_os = "macos"))]
fn active_point(_app: &AppHandle) -> Option<glimpsfy_capture::LogicalPoint> {
    None
}

/// Moves an ordinary window to the screen that holds `at` (the pointer's without it), then shows
/// it with the keyboard.
fn bring_forward(
    app: &AppHandle,
    window: WebviewWindow,
    at: Option<glimpsfy_capture::LogicalPoint>,
    only_if_elsewhere: bool,
) {
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| {
        crate::macos::center_on_screen(&window, at, only_if_elsewhere, mtm);
        let _ = window.show();
        let _ = window.set_focus();
    });
    #[cfg(not(target_os = "macos"))]
    {
        let _ = at;
        center_on_cursor_monitor(app, &window, only_if_elsewhere);
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Without AppKit: the middle of the work area of the monitor under the cursor, in physical
/// pixels, which Windows and X11 use for every monitor alike. Wayland reports no cursor, so
/// there the window stays where `center` put it.
#[cfg(not(target_os = "macos"))]
fn center_on_cursor_monitor(app: &AppHandle, window: &WebviewWindow, only_if_elsewhere: bool) {
    let Ok(cursor) = app.cursor_position() else {
        return;
    };
    let Ok(Some(monitor)) = app.monitor_from_point(cursor.x, cursor.y) else {
        return;
    };
    if only_if_elsewhere {
        if let Ok(Some(current)) = window.current_monitor() {
            if current.position() == monitor.position() && current.size() == monitor.size() {
                return;
            }
        }
    }
    let Ok(size) = window.outer_size() else {
        return;
    };
    let area = monitor.work_area();
    let (area_w, area_h) = (i64::from(area.size.width), i64::from(area.size.height));
    let (w, h) = (i64::from(size.width).min(area_w), i64::from(size.height).min(area_h));
    let x = i64::from(area.position.x) + (area_w - w) / 2;
    let y = i64::from(area.position.y) + (area_h - h) / 2;
    let _ = window.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
}
