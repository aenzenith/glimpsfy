//! The menu bar popover: a keyable panel that hangs under the menu bar icon, made hidden at start
//! so a click opens it at once. Owner: app shell (milestone 1).
//!
//! A left click on the icon opens or closes it (or captures an area, when Settings › General says
//! so); the right button keeps the native menu of `tray.rs`. It closes on Esc (its page), when it
//! loses the keyboard, and on a click anywhere outside Glimpsfy. It never brings Glimpsfy to the
//! front, so the app the user is in stays active behind it.
//!
//! Its one-shot delay is the capture area's (`capture_set_delay`), so the popover only sets it.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::capture::CaptureMode;
use crate::settings::IconClick;
use crate::{session, trace, tray, AppState};

pub const LABEL: &str = "popover";
/// The popover's width in points (DESIGN.md); its height follows its page (`fit`).
const WIDTH: f64 = 340.0;
/// The gap between the menu bar and the popover's top edge.
const GAP: f64 = 6.0;

/// To the popover page, each time it opens, so it reloads its recent captures and the
/// permission. No payload.
pub const OPENED: &str = "popover://opened";

/// When the popover last hid. A click on the icon while it is open first takes its keyboard
/// (hiding it) and then arrives as a click, which would open it again at once.
static LAST_HIDE: Mutex<Option<Instant>> = Mutex::new(None);

pub fn tray_event(app: &AppHandle, event: &TrayIconEvent) {
    let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event else {
        return;
    };
    // ⌥-click stops a take (or ends its countdown), or else an interval series, without opening
    // anything over the screen being captured. The click event carries no modifiers, so AppKit's
    // current flags are read.
    #[cfg(target_os = "macos")]
    if crate::macos::option_key_down() && (crate::recording::stop_from_tray(app) || session::interval::stop(app)) {
        return;
    }
    match app.state::<AppState>().settings.get().general.icon_click {
        IconClick::OpenMenu => toggle(app),
        IconClick::CaptureArea => {
            if let Err(error) = session::start(app, CaptureMode::Area, Instant::now()) {
                trace::log(format!("[glimpsfy] capture from the icon: {error}"));
            }
        }
    }
}

/// Makes the hidden popover and the outside-click watch. Once, at start, on the main thread.
pub fn setup(app: &AppHandle) {
    if let Err(error) = build(app) {
        trace::log(format!("[glimpsfy] popover: {error}"));
        return;
    }
    #[cfg(target_os = "macos")]
    crate::macos::on_outside_click(app, hide);
}

fn build(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let make = || {
        WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
            .title("Glimpsfy")
            .decorations(false)
            .transparent(true)
            .shadow(true)
            .resizable(false)
            .skip_taskbar(true)
            .always_on_top(true)
            .visible(false)
            .focused(false)
            .accept_first_mouse(true)
            .disable_drag_drop_handler()
            .inner_size(WIDTH, 420.0)
            .build()
    };
    #[cfg(target_os = "macos")]
    let window = {
        let window = crate::macos::without_activating(make)?;
        let panel = window.clone();
        crate::macos::on_main(app, move |mtm| crate::macos::setup_popover(&panel, mtm));
        window
    };
    #[cfg(not(target_os = "macos"))]
    let window = make()?;

    let handle = app.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(false) = event {
            hide(&handle);
        }
    });
    Ok(window)
}

fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL).or_else(|| build(app).ok())
}

pub fn is_visible(app: &AppHandle) -> bool {
    app.get_webview_window(LABEL).is_some_and(|w| w.is_visible().unwrap_or(false))
}

pub fn toggle(app: &AppHandle) {
    if is_visible(app) {
        hide(app);
        return;
    }
    let just_hidden =
        LAST_HIDE.lock().ok().and_then(|at| *at).is_some_and(|at| at.elapsed() < Duration::from_millis(250));
    if !just_hidden {
        show(app);
    }
}

/// Opens the popover under the icon with the keyboard, without activating Glimpsfy.
pub fn show(app: &AppHandle) {
    let Some(window) = window(app) else {
        return;
    };
    let _ = window.emit(OPENED, ());
    #[cfg(target_os = "macos")]
    {
        let tray = tray::icon(app);
        crate::macos::on_main(app, move |mtm| crate::macos::show_popover(&window, tray.as_ref(), GAP, mtm));
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(rect) = tray::icon(app).and_then(|t| t.rect().ok().flatten()) {
            let _ = window.set_position(rect.position);
        }
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn hide(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if !window.is_visible().unwrap_or(false) {
        return;
    }
    let _ = window.hide();
    if let Ok(mut at) = LAST_HIDE.lock() {
        *at = Some(Instant::now());
    }
}

/// The page measured itself: the popover takes `height` points and keeps its top edge.
pub fn fit(app: &AppHandle, height: f64) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let height = height.clamp(120.0, 900.0);
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::fit_popover(&window, height, mtm));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = window.set_size(tauri::LogicalSize::new(WIDTH, height));
    }
}

/// Glimpsfy was started while it runs. With the menu bar icon hidden, Settings opens, since it
/// is the only way back to them; otherwise nothing happens.
pub fn second_launch(app: &AppHandle) {
    if !app.state::<AppState>().settings.get().general.show_menu_bar_icon {
        crate::windows::open(app, crate::commands::windows::WindowKind::Settings);
    }
}
