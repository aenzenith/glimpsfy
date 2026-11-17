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
use serde::Serialize;
use tauri::{AppHandle, Emitter, EventTarget, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::capture::CaptureId;
use crate::commands::error::{CommandError, ErrorCode};
use crate::commands::windows::WindowKind;
use crate::{events, session, trace, AppState};

pub const OVERLAY_PREFIX: &str = "overlay-";
pub const TOAST: &str = "toast";
/// The toast window's width: the 380 px toast and room for its shadow.
const TOAST_WIDTH: f64 = 412.0;

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

/// While a capture is open, the overlay under the pointer takes the keyboard, so Esc, the arrow
/// keys and ⌘C act on the display the user is looking at, and hover reaches its page.
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
                let Some(entry) = session.displays.iter().find(|d| d.display.frame.contains(pointer)) else {
                    continue;
                };
                let display = entry.display.id;
                if last != Some(display) {
                    if last.is_some() {
                        if let Some(window) = app.get_webview_window(&overlay_label(display)) {
                            crate::macos::on_main(&app, move |mtm| crate::macos::key_overlay(&window, mtm));
                        }
                    }
                    last = Some(display);
                }
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, id);
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

/// The toast page measured its stack: the window takes that height at the top-right corner of
/// the capture's display, or goes away at 0.
pub fn toast_fit(app: &AppHandle, height: f64) {
    let Some(window) = app.get_webview_window(TOAST) else {
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let display = *TOAST_DISPLAY.lock().unwrap_or_else(|p| p.into_inner());
        crate::macos::on_main(app, move |mtm| {
            if height <= 0.0 {
                crate::macos::order_out(&window, mtm);
                return;
            }
            let screens = crate::macos::screens(mtm);
            let screen = display.and_then(|d| screens.iter().find(|s| s.id == d)).or(screens.first());
            if let Some(screen) = screen {
                crate::macos::place_toast(&window, screen, TOAST_WIDTH, height, mtm);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = if height <= 0.0 { window.hide() } else { window.show() };
    }
}

// -------------------------------------------------------------------------- ordinary windows

/// Opens (or brings forward) one of the app's ordinary windows.
pub fn open(app: &AppHandle, kind: WindowKind) {
    let label = kind.label();
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }
    let (width, height) = kind.size();
    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .title(kind.title())
        .inner_size(width, height)
        .min_inner_size(width.min(480.0), height.min(360.0))
        .center()
        .visible(true)
        .focused(true)
        .build();
    match built {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(error) => trace::log(format!("[glimpsfy] window {label}: {error}")),
    }
}
