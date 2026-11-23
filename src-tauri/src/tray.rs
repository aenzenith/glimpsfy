//! The menu bar icon and its native menu. A left click opens the popover (`popover.rs`) on macOS
//! and Windows; the right button opens this menu, and Linux, whose AppIndicator reports no
//! clicks, opens it with either button.
//!
//! The menu holds every capture mode with its shortcut from Settings › Shortcuts, the one-shot
//! delay, History, Settings and Quit, in the language of Settings › General. It is built again
//! whenever the settings are saved (`settings://changed`) and when the delay changes, so its
//! words, its shortcuts and its check marks are never stale. During a countdown the icon shows
//! the seconds left and the menu offers to cancel.

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Listener, Manager, Wry};

use crate::commands::capture::CaptureMode;
use crate::commands::windows::WindowKind;
use crate::{events, i18n, session, trace, windows, AppState};

const TRAY_ID: &str = "glimpsfy";
const CAPTURE_AREA: &str = "capture-area";
const CAPTURE_WINDOW: &str = "capture-window";
const CAPTURE_DISPLAY: &str = "capture-display";
const CAPTURE_ALL: &str = "capture-all-displays";
const REPEAT_AREA: &str = "repeat-last-area";
const CAPTURE_DELAYED: &str = "capture-area-delayed";
const COPY_TEXT: &str = "copy-text";
const SCROLL_CAPTURE: &str = "scroll-capture";
const CANCEL_DELAY: &str = "cancel-delay";
const DELAY_PREFIX: &str = "delay-";
const HISTORY: &str = "history";
const SETTINGS: &str = "settings";
const QUIT: &str = "quit";

/// The kit's menu bar glyph, black on transparent: a template image, which macOS tints for a
/// light or dark menu bar.
const ICON: &[u8] = include_bytes!("../icons/tray.png");

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(ICON)?)
        .icon_as_template(true)
        .tooltip("Glimpsfy")
        .menu(&menu)
        // A left click opens the popover (`popover.rs`); the right button keeps this menu. Linux
        // reports no clicks, so its menu opens with either button.
        .show_menu_on_left_click(cfg!(target_os = "linux"))
        .on_tray_icon_event(|tray, event| crate::popover::tray_event(tray.app_handle(), &event))
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .build(app)?;

    // The menu follows the settings: its language and its shortcuts.
    let handle = app.clone();
    app.listen_any(events::SETTINGS_CHANGED, move |_| refresh(&handle));
    Ok(())
}

/// Builds the menu again from the current settings and delay.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match build_menu(app) {
        Ok(menu) => {
            if let Err(error) = tray.set_menu(Some(menu)) {
                trace::log(format!("[glimpsfy] menu: {error}"));
            }
        }
        Err(error) => trace::log(format!("[glimpsfy] menu: {error}")),
    }
}

/// Shows the seconds left of a countdown beside the icon, or nothing.
pub fn show_countdown(app: &AppHandle, remaining: Option<u32>) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_title(remaining.map(|s| format!("{s}")));
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let settings = app.state::<AppState>().settings.get();
    let language = settings.general.language;
    let word = |key: &str| i18n::text(language, key, &[]);
    let keys = &settings.shortcuts;

    let mut items: Vec<Box<dyn tauri::menu::IsMenuItem<Wry>>> = Vec::new();
    let modes = [
        (CAPTURE_AREA, word("tray.area"), &keys.capture_area),
        (CAPTURE_WINDOW, word("tray.window"), &keys.capture_window),
        (CAPTURE_DISPLAY, word("tray.display"), &keys.capture_display),
        (CAPTURE_ALL, word("tray.all"), &keys.capture_all_displays),
        (REPEAT_AREA, word("tray.repeat"), &keys.repeat_last_area),
        (CAPTURE_DELAYED, word("tray.delayed"), &keys.capture_area_delayed),
        (COPY_TEXT, word("tray.copyText"), &keys.copy_text),
    ];
    for (id, text, key) in modes {
        items.push(Box::new(item(app, id, text, key.as_deref())?));
    }
    // Only the macOS build streams the selection and posts scroll events (`scroll.rs`).
    if cfg!(target_os = "macos") {
        items.push(Box::new(item(app, SCROLL_CAPTURE, word("tray.scroll"), keys.scroll_capture.as_deref())?));
    }
    items.push(Box::new(PredefinedMenuItem::separator(app)?));

    if session::delay::is_running() {
        items.push(Box::new(item(app, CANCEL_DELAY, word("tray.cancelDelay"), None)?));
    }
    let pending = session::delay::pending();
    let mut choices: Vec<CheckMenuItem<Wry>> = Vec::new();
    for seconds in session::delay::CHOICES {
        let text = if seconds == 0 {
            word("tray.off")
        } else {
            i18n::text(language, "tray.seconds", &[("n", &seconds.to_string())])
        };
        choices.push(CheckMenuItem::with_id(
            app,
            format!("{DELAY_PREFIX}{seconds}"),
            text,
            true,
            pending == seconds,
            None::<&str>,
        )?);
    }
    let choice_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> =
        choices.iter().map(|c| c as &dyn tauri::menu::IsMenuItem<Wry>).collect();
    items.push(Box::new(Submenu::with_items(app, word("tray.delay"), true, &choice_refs)?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));

    items.push(Box::new(item(app, HISTORY, word("tray.history"), keys.open_history.as_deref())?));
    items.push(Box::new(item(app, SETTINGS, word("tray.settings"), Some("CommandOrControl+,"))?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(item(app, QUIT, word("tray.quit"), Some("CommandOrControl+Q"))?));

    let refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = items.iter().map(|i| i.as_ref()).collect();
    Menu::with_items(app, &refs)
}

/// A menu item with its shortcut, or without one when the shortcut does not parse as a menu
/// accelerator.
fn item(app: &AppHandle, id: &str, text: impl AsRef<str>, accelerator: Option<&str>) -> tauri::Result<MenuItem<Wry>> {
    let text = text.as_ref();
    let accelerator = accelerator.filter(|a| !a.is_empty());
    MenuItem::with_id(app, id, text, true, accelerator)
        .or_else(|_| MenuItem::with_id(app, id, text, true, None::<&str>))
}

pub(crate) fn on_menu(app: &AppHandle, id: &str) {
    let mode = match id {
        CAPTURE_AREA => Some(CaptureMode::Area),
        CAPTURE_WINDOW => Some(CaptureMode::Window),
        CAPTURE_DISPLAY => Some(CaptureMode::Display),
        CAPTURE_ALL => Some(CaptureMode::AllDisplays),
        REPEAT_AREA => Some(CaptureMode::RepeatLastArea),
        CAPTURE_DELAYED => Some(CaptureMode::AreaDelayed),
        COPY_TEXT => Some(CaptureMode::CopyText),
        SCROLL_CAPTURE => Some(CaptureMode::ScrollCapture),
        _ => None,
    };
    if let Some(mode) = mode {
        session::start_from_menu(app, mode);
        return;
    }
    if let Some(seconds) = id.strip_prefix(DELAY_PREFIX).and_then(|s| s.parse::<u32>().ok()) {
        if let Err(error) = session::delay::set_pending(app, seconds) {
            trace::log(format!("[glimpsfy] delay from the menu: {error}"));
        }
        return;
    }
    match id {
        CANCEL_DELAY => {
            session::delay::cancel(app);
        }
        HISTORY => windows::open(app, WindowKind::History),
        SETTINGS => windows::open(app, WindowKind::Settings),
        QUIT => app.exit(0),
        _ => {}
    }
}

// ------------------------------------------------------------- for the app shell (popover.rs)

/// Shows or hides the menu bar icon (Settings › General).
pub fn set_visible(app: &AppHandle, visible: bool) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(visible);
    }
}

/// The menu bar icon, which the popover hangs from.
pub fn icon(app: &AppHandle) -> Option<tauri::tray::TrayIcon> {
    app.tray_by_id(TRAY_ID)
}
