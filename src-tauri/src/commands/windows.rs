//! Window commands: open the app's windows, fit the toast, the popover, the pins, the pickers,
//! links and quitting. Owner: app shell (milestone 1). The native side is `popover.rs`,
//! `pins.rs` and the app shell section of `macos.rs`, and `desktop/` on Windows and Linux.

use std::path::PathBuf;

use glimpsfy_capture::LogicalRect;
use serde::{Deserialize, Serialize};
use tauri::ipc::Response;
use tauri::{AppHandle, WebviewWindow};

use super::error::{CommandError, CommandResult};
use crate::pins::{self, PinInfo, PinSource};
use crate::popover;

/// The windows a page can open. Overlays, the toast and pins are made by the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowKind {
    Editor,
    Recorder,
    Popover,
    History,
    Settings,
    Firstrun,
}

impl WindowKind {
    pub fn label(self) -> &'static str {
        match self {
            WindowKind::Editor => "editor",
            WindowKind::Recorder => "recorder",
            WindowKind::Popover => "popover",
            WindowKind::History => "history",
            WindowKind::Settings => "settings",
            WindowKind::Firstrun => "firstrun",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            WindowKind::Editor => "Glimpsfy Editor",
            WindowKind::Recorder => "Glimpsfy Recorder",
            WindowKind::Popover => "Glimpsfy",
            WindowKind::History => "Glimpsfy History",
            WindowKind::Settings => "Glimpsfy Settings",
            WindowKind::Firstrun => "Welcome to Glimpsfy",
        }
    }

    /// The smallest size in points the user can make the window. Settings stops at 640 pt wide:
    /// below that its sidebar, a folder path and a device menu no longer fit side by side.
    pub fn min_size(self) -> (f64, f64) {
        let (width, height) = self.size();
        match self {
            WindowKind::Settings => (640.0, height.min(360.0)),
            _ => (width.min(480.0), height.min(360.0)),
        }
    }

    /// The window's first size in points (DESIGN.md's window sizes).
    pub fn size(self) -> (f64, f64) {
        match self {
            WindowKind::Editor => (1120.0, 760.0),
            // The text panel of cut-by-text (340 pt) sits beside the video; below 1280 pt wide the
            // window closes it, so the video keeps its room.
            WindowKind::Recorder => (1280.0, 760.0),
            WindowKind::Popover => (340.0, 520.0),
            WindowKind::History => (1040.0, 700.0),
            WindowKind::Settings => (760.0, 640.0),
            WindowKind::Firstrun => (600.0, 560.0),
        }
    }
}

#[tauri::command]
pub fn window_open(app: AppHandle, kind: WindowKind) -> CommandResult<()> {
    if kind == WindowKind::Popover {
        popover::show(&app);
        return Ok(());
    }
    crate::windows::open(&app, kind);
    Ok(())
}

/// A section of the settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsSection {
    General,
    Shortcuts,
    Output,
    Beautify,
    Recording,
    Ocr,
    Upload,
    Agents,
    About,
}

/// To the settings window: a section was asked for; the page reads it with
/// `window_take_settings_section`. No payload.
pub const SETTINGS_SECTION: &str = "settings://section-requested";

/// The section asked for last, until the settings page takes it. A window that is still loading
/// misses the event, so the page also takes it once it starts.
static PENDING_SECTION: std::sync::Mutex<Option<SettingsSection>> = std::sync::Mutex::new(None);

/// Opens Settings at `section` (such as Upload, when no upload host is chosen yet) instead of the
/// section shown last.
#[tauri::command]
pub fn window_open_settings(app: AppHandle, section: SettingsSection) {
    *PENDING_SECTION.lock().unwrap_or_else(|p| p.into_inner()) = Some(section);
    crate::windows::open(&app, WindowKind::Settings);
    let target = tauri::EventTarget::webview_window(WindowKind::Settings.label());
    let _ = tauri::Emitter::emit_to(&app, target, SETTINGS_SECTION, ());
}

/// The section `window_open_settings` asked for, once; `None` when none is waiting.
#[tauri::command]
pub fn window_take_settings_section() -> Option<SettingsSection> {
    PENDING_SECTION.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// The toast page drew a frame: the window takes `frame` (invisible while it shows no toast), and
/// each toast in `glass` gets its native blur where it is drawn.
#[tauri::command]
pub fn toast_fit(app: AppHandle, frame: crate::windows::ToastFrame, glass: Vec<crate::windows::ToastGlass>) {
    crate::windows::toast_fit(&app, frame, glass);
}

/// The pointer in the toast window (points from its top-left corner), or `None` while the window
/// is hidden. The page cannot follow the pointer itself in a panel that never becomes key.
#[tauri::command]
pub fn toast_pointer(app: AppHandle) -> Option<glimpsfy_capture::LogicalPoint> {
    crate::windows::toast_pointer(&app)
}

#[tauri::command]
pub fn app_quit(app: AppHandle) {
    app.exit(0);
}

/// A page's error or warning, written to the core's log in dev builds, so a failure in a hidden
/// window (an overlay, the toast) is not lost.
#[tauri::command]
pub fn app_log(window: tauri::WebviewWindow, level: String, message: String) {
    crate::trace::log(format!("[page {}] {level}: {message}", window.label()));
}

/// Puts text on the clipboard: a link, Markdown, recognised text.
#[tauri::command]
pub fn app_copy_text(app: AppHandle, text: String) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        crate::macos::copy_text(&text).map_err(CommandError::internal)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        crate::desktop::copy_text(&text).map_err(CommandError::internal)
    }
}

/// Opens a link with the app the system picks for it: a web page in the default browser, a
/// `mailto:` address in the mail app and a `tel:` number in the phone app (FaceTime on macOS). No
/// other scheme opens (`openable_url`), so a provider description, a pasted link or a recognised
/// text cannot start anything else.
#[tauri::command]
pub fn app_open_url(app: AppHandle, url: String) -> CommandResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let url = openable_url(&url).ok_or_else(|| CommandError::invalid("only http, https, mailto and tel links open"))?;
    app.opener().open_url(url, None::<&str>).map_err(|e| CommandError::internal(e.to_string()))
}

/// Whether an app on this computer opens `url`, for the schemes `app_open_url` takes only; the
/// text layer greys an action out and says why before the click instead of failing after it.
#[tauri::command]
pub fn app_can_open_url(url: String) -> bool {
    let Some(url) = openable_url(&url) else { return false };
    #[cfg(target_os = "macos")]
    {
        crate::macos::can_open_url(&url)
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::desktop::can_open_url(&url)
    }
}

/// Adds an event to the user's calendar without a permission: the core writes it as a new `.ics`
/// file into `<cache>/events/` and opens it with the system's calendar app (`calendar.rs`).
#[tauri::command]
pub async fn app_add_to_calendar(app: AppHandle, event: crate::calendar::CalendarEvent) -> CommandResult<()> {
    tauri::async_runtime::spawn_blocking(move || crate::calendar::add(&app, &event).map(|_| ()))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
}

/// The address `app_open_url` hands to the system, without the spaces around it, or `None` when
/// it must not open. `http` and `https` take any address; `mailto:` takes one plain address with
/// no query, since `?attach=` or `?bcc=` would let a picture on screen write the mail;
/// `tel:` takes `+`, digits and the separators `-().` only. Any other scheme (`javascript:`,
/// `file:`, `smb:`, `facetime:`, an app's own) and any control character or inner space answer
/// `None`.
pub fn openable_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let (scheme, rest) = url.split_once(':')?;
    let ok = match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.strip_prefix("//").is_some_and(|host| !host.is_empty() && !host.starts_with('/')),
        "mailto" => mail_address(rest),
        "tel" => {
            let number = rest.strip_prefix('+').unwrap_or(rest);
            number.chars().filter(char::is_ascii_digit).count() >= 3
                && number.chars().all(|c| c.is_ascii_digit() || "-().".contains(c))
        }
        _ => false,
    };
    ok.then(|| url.to_string())
}

/// One plain address: ASCII letters, digits and `._+-` before the `@`, and a domain of letters,
/// digits and hyphens with a last label of two or more letters.
fn mail_address(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else { return false };
    let labels: Vec<&str> = domain.split('.').collect();
    !local.is_empty()
        && !local.starts_with('.')
        && !local.ends_with('.')
        && local.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
        && labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
}

/// A pane of System Settings that is not a permission's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SystemPane {
    /// Keyboard › Keyboard Shortcuts, where the system's own screenshot keys are turned off.
    KeyboardShortcuts,
    /// General › Login Items, where a refused login item is allowed.
    LoginItems,
}

/// Opens a pane of the system's settings: System Settings on macOS, the matching page of
/// Settings on Windows (`ms-settings:`). Linux desktops share no such pages, so there the command
/// answers `not_implemented`, and `app_platform` says so beforehand (`systemSettingsPanes`).
#[tauri::command]
pub fn app_open_system_settings(app: AppHandle, pane: SystemPane) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_opener::OpenerExt;
        let url = match pane {
            SystemPane::KeyboardShortcuts => {
                "x-apple.systempreferences:com.apple.Keyboard-Settings.extension?Shortcuts"
            }
            SystemPane::LoginItems => "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        };
        app.opener().open_url(url, None::<&str>).map_err(|e| CommandError::internal(e.to_string()))
    }
    #[cfg(not(target_os = "macos"))]
    match crate::desktop::open_system_settings(&app, pane) {
        Ok(true) => Ok(()),
        Ok(false) => Err(CommandError::not_implemented("opening the system's settings on this platform")),
        Err(error) => Err(CommandError::internal(error)),
    }
}

// ------------------------------------------------------------------------------ the platform

/// The system Glimpsfy runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Macos,
    Windows,
    Linux,
}

/// The kind of session a Linux desktop runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplaySession {
    X11,
    Wayland,
}

/// What this platform can do, so a page hides or changes what it cannot. A feature whose native
/// part is not written yet reads `false` until it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// A single window can be listed, snapped to, captured and recorded on its own. `false` on
    /// Wayland, where an app sees no other app's windows.
    pub window_capture: bool,
    /// The system keeps Glimpsfy's own windows (toast, pins, popover, the recorder's bars) out of
    /// captures and recordings. Where it is `false` they are hidden around a still capture
    /// instead, and a recording shows whichever of them is on screen.
    pub capture_exclusion: bool,
    /// The capture overlay takes keys without becoming the active window. Where it is `false`
    /// the overlay takes the keyboard focus and gives it back when it closes.
    pub non_activating_overlay: bool,
    /// The click ring and the key caps of a recording (clicks and keys of other apps can be
    /// watched).
    pub input_overlays: bool,
    /// The webcam bubble of a recording.
    pub camera_bubble: bool,
    /// A pin can show on every Space or virtual desktop (`pin_set_all_spaces`).
    pub pin_all_spaces: bool,
    /// A pin can be locked (click-through) and woken by holding ⌥ or Alt (`pin_set_locked`).
    pub pin_lock: bool,
    /// The toast and the recorder's bar get a native blur behind them. Where it is `false` the
    /// pages draw their own background.
    pub native_glass: bool,
    /// A capture can be dragged out of the popover as a file (`popover_drag_out`).
    pub drag_out: bool,
    /// A file itself can be put on the clipboard: a recording, a GIF, an image that is neither
    /// PNG nor JPEG (`record_export` with copy, `history_copy`).
    pub copy_file: bool,
    /// The system has a capture sound for Settings › General › Shutter sound.
    pub shutter_sound: bool,
    /// `app_open_system_settings` opens a page of the system's settings.
    pub system_settings_panes: bool,
    /// Screen capture needs a permission the user grants (the first-run window asks for it).
    pub screen_recording_permission: bool,
    /// This copy can install an update by itself. Where it is `false` (a `.deb` or `.rpm`
    /// install) `update://changed` only reports `available` with `releaseUrl`.
    pub self_update: bool,
    /// The system gives the app global shortcuts. Where it is `false` (Wayland) the user binds
    /// `glimpsfy --action <name>` in the desktop's own keyboard settings.
    pub global_shortcuts: bool,
    /// The tray icon reports clicks, so a left click opens the popover or captures (Settings ›
    /// General › Icon click). Where it is `false` (Linux) the icon only opens its menu.
    pub tray_popover: bool,
    /// The system tells where the pointer is. Where it is `false` (Wayland) "the display under
    /// the pointer" is the primary display, and windows open there.
    pub global_pointer: bool,
    /// A transparent window shows what lies under it. Where it is `false` (an X11 window manager
    /// without a compositor) its transparent parts are black, so a page draws an opaque
    /// background, and a dim over the live screen cannot be drawn.
    pub transparent_windows: bool,
    /// A window capture can keep the window's shadow (Settings › Output › Shadow). Windows draws
    /// the shadow outside every surface a capture reads, so it is `false` there.
    pub window_shadow: bool,
    /// The subjects of an image can be lifted from its background: the editor's Remove
    /// background and the overlay's Copy subject (`subject_*`). Vision does it on macOS; Windows
    /// and Linux have no engine for it yet, so both stay hidden there.
    pub subject_lift: bool,
    /// Send to app brings an app forward and presses its paste key (`send_to_app`). Only macOS
    /// has it for now; Copy path works everywhere.
    pub send_to_app: bool,
    /// The local MCP server and its approval card (Settings › Agents). Only macOS has them.
    pub agents: bool,
    /// The scrolling capture (⌘↓ in the overlay, `capture_start_scroll`): a live stream of the
    /// selection while the page under it scrolls. Only macOS has it for now.
    pub scroll_capture: bool,
    /// Captures and recordings can leave the desktop icons, notification banners and widgets
    /// out (Settings › Output and Settings › Recording). ScreenCaptureKit's filter does it on
    /// macOS; the other systems have no such filter, so the switches are hidden there.
    pub clean_desktop: bool,
}

/// What `app_platform` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Platform {
    pub os: Os,
    /// The Linux session; `null` on macOS and Windows.
    pub session: Option<DisplaySession>,
    pub capabilities: Capabilities,
    /// Where a `<video>` loads a history recording from when the webview cannot play the app's
    /// URL schemes: a loopback address on Linux, whose WebKitGTK player loads none
    /// (`desktop/loopback.rs`); `null` on macOS and Windows, which play `glimpsfy-history://`.
    pub video_base: Option<String>,
}

/// The system, the Linux session and what the platform can do, so a page hides or changes what
/// this platform cannot do. It does not change while the app runs.
#[tauri::command]
pub fn app_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform {
            os: Os::Macos,
            session: None,
            capabilities: Capabilities {
                window_capture: true,
                capture_exclusion: true,
                non_activating_overlay: true,
                input_overlays: true,
                camera_bubble: true,
                pin_all_spaces: true,
                pin_lock: true,
                native_glass: true,
                drag_out: true,
                copy_file: true,
                shutter_sound: true,
                system_settings_panes: true,
                screen_recording_permission: true,
                self_update: true,
                global_shortcuts: true,
                tray_popover: true,
                global_pointer: true,
                transparent_windows: true,
                window_shadow: true,
                subject_lift: true,
                send_to_app: true,
                agents: true,
                scroll_capture: true,
                clean_desktop: true,
            },
            video_base: None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    crate::desktop::describe()
}

/// Quits and starts Glimpsfy again, as macOS asks after the screen recording permission changes.
#[tauri::command]
pub fn app_restart(app: AppHandle) {
    app.restart();
}

/// A file type the file picker offers.
#[derive(Clone, Debug, Deserialize)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// Lets the user pick a folder, starting in `start`; `None` when they cancel.
#[tauri::command]
pub async fn app_choose_folder(app: AppHandle, start: Option<PathBuf>) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file();
    if let Some(start) = start.filter(|p| p.is_dir()) {
        dialog = dialog.set_directory(start);
    }
    dialog.blocking_pick_folder().and_then(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned())
}

/// Lets the user pick one file of the given types; `None` when they cancel. While a capture is
/// open (the beautify panel's image), its overlays step aside for the panel, as they do for Save
/// as, and come back with the capture as it was.
#[tauri::command]
pub async fn app_choose_file(app: AppHandle, filters: Vec<FileFilter>) -> Option<String> {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file();
    for filter in &filters {
        let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(&filter.name, &extensions);
    }
    let capture = app.state::<crate::AppState>().sessions.current().map(|s| s.id);
    if capture.is_some() {
        // The overlays sit above every panel, and a menu bar app's panel opens behind the front
        // app.
        crate::windows::hide_overlays(&app);
        crate::output::bring_forward(&app);
    }
    let picked = dialog.blocking_pick_file().and_then(|p| p.into_path().ok());
    if let Some(path) = &picked {
        // A beautify panel shows the picked image before any style names it (`frames.rs`).
        crate::frames::allow_background(path);
    }
    let picked = picked.map(|p| p.to_string_lossy().into_owned());
    if let Some(id) = capture {
        crate::session::reshow(&app, id);
    }
    picked
}

// ----------------------------------------------------------------------------------- popover

/// Closes the popover (Esc, or an action that leaves it).
#[tauri::command]
pub fn popover_hide(app: AppHandle) {
    popover::hide(&app);
}

/// The popover's page measured itself: the window takes `height` points under the icon.
#[tauri::command]
pub fn popover_fit(app: AppHandle, height: f64) {
    popover::fit(&app, height);
}

/// Starts dragging a history entry out of the popover as a file: the image the entry shows
/// (`Entry::media_path`), which is the history's own copy when a later Copy made one after a
/// Save. The older saved file is never handed out instead, since it may lack a redaction made
/// since. The page calls it once the pointer moved a few points with the button down.
#[tauri::command]
pub async fn popover_drag_out(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: glimpsfy_history::EntryId,
) -> CommandResult<()> {
    let entry = super::history::history_get(app.clone(), entry_id).await?;
    let file = entry
        .media_path()
        .filter(|p| p.exists())
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| CommandError::not_found("the file of this history entry"))?;
    let image = entry.thumbnail.clone().unwrap_or_else(|| file.clone());
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::macos::on_main(&app, move |mtm| {
            let _ = tx.send(crate::macos::drag_out(&window, &file, &image, (64.0, 44.0), mtm));
        });
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| CommandError::internal("the drag did not start in time"))?
            .map_err(CommandError::internal)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        if !crate::desktop::describe().capabilities.drag_out {
            return Err(CommandError::not_implemented("dragging a capture out on this platform"));
        }
        crate::desktop::drag_out(&window, &file, &image, (64.0, 44.0)).map_err(CommandError::internal)
    }
}

// -------------------------------------------------------------------------------------- pins

/// Opens a pin of a capture's selection, a history entry or an image file. The overlay closes
/// its capture itself once this returns.
#[tauri::command]
pub async fn pin_open(app: AppHandle, source: PinSource) -> CommandResult<PinInfo> {
    pins::open(&app, source).await
}

/// The pin this window shows.
#[tauri::command]
pub fn pin_info(window: WebviewWindow) -> CommandResult<PinInfo> {
    pins::info(pins::id_of(&window)?)
}

/// The image of this pin window as bytes; the page makes a blob URL of them.
#[tauri::command]
pub async fn pin_image(window: WebviewWindow) -> CommandResult<Response> {
    let id = pins::id_of(&window)?;
    let (bytes, _) = tauri::async_runtime::spawn_blocking(move || pins::image(id))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))??;
    Ok(Response::new(bytes))
}

/// This pin window's frame, in global points from the top-left of the primary display.
#[tauri::command]
pub fn pin_frame(app: AppHandle, window: WebviewWindow) -> CommandResult<LogicalRect> {
    pins::frame(&app, pins::id_of(&window)?)
}

/// Moves and sizes this pin window (a corner drag, a zoom, collapse and restore).
#[tauri::command]
pub fn pin_set_frame(app: AppHandle, window: WebviewWindow, frame: LogicalRect) -> CommandResult<()> {
    pins::set_frame(&app, pins::id_of(&window)?, frame)
}

/// Locks this pin (clicks go through it until ⌥ is held) or unlocks it. While locked, the page
/// hears `pin://option-key` as ⌥ goes down and up.
#[tauri::command]
pub fn pin_set_locked(app: AppHandle, window: WebviewWindow, locked: bool) -> CommandResult<()> {
    pins::set_locked(&app, pins::id_of(&window)?, locked)
}

#[tauri::command]
pub fn pin_set_all_spaces(app: AppHandle, window: WebviewWindow, all: bool) -> CommandResult<()> {
    pins::set_all_spaces(&app, pins::id_of(&window)?, all)
}

/// Copies this pin's image and shows the Copied toast.
#[tauri::command]
pub async fn pin_copy(app: AppHandle, window: WebviewWindow) -> CommandResult<()> {
    let id = pins::id_of(&window)?;
    let (tx, rx) = std::sync::mpsc::channel();
    // The pasteboard is written on the main thread, as AppKit expects.
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = tx.send(pins::copy(id));
    });
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| CommandError::internal("the copy did not finish"))??;
    let info = pins::info(id)?;
    let mut toast = crate::windows::Toast::new(crate::windows::ToastKind::Copied);
    toast.width = Some(info.pixel_width);
    toast.height = Some(info.pixel_height);
    crate::windows::toast(&handle, None, toast);
    Ok(())
}

/// Saves this pin's image into the save folder (never over a file) and shows the Saved toast.
#[tauri::command]
pub async fn pin_save(app: AppHandle, window: WebviewWindow) -> CommandResult<String> {
    let id = pins::id_of(&window)?;
    let path = tauri::async_runtime::spawn_blocking(move || pins::save(&app, id))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))??;
    Ok(path.to_string_lossy().into_owned())
}

/// Opens this pin's image in the editor. A pin of a history entry opens the entry, drawings
/// included (`history_open_in_editor`); any other pin opens its own copy of the image, so the
/// editor works on what the pin shows even when the source file has moved.
#[tauri::command]
pub async fn pin_edit(app: AppHandle, window: WebviewWindow) -> CommandResult<()> {
    let id = pins::id_of(&window)?;
    match pins::info(id)?.entry_id {
        Some(entry_id) => super::history::history_open_in_editor(app, entry_id).await,
        None => super::render::editor_open(app, pins::file(id)?, None).await.map(|_| ()),
    }
}

#[tauri::command]
pub fn pin_close(app: AppHandle, window: WebviewWindow) -> CommandResult<()> {
    pins::close(&app, pins::id_of(&window)?);
    Ok(())
}

#[tauri::command]
pub fn pin_close_all(app: AppHandle) {
    pins::close_all(&app);
}

#[cfg(test)]
mod tests {
    use super::openable_url;

    #[test]
    fn only_web_mail_and_phone_links_open() {
        let opens = [
            ("https://example.com/a?b=1#c", "https://example.com/a?b=1#c"),
            ("  http://lokantadeniz.com/menu  ", "http://lokantadeniz.com/menu"),
            ("HTTPS://Example.com", "HTTPS://Example.com"),
            ("mailto:ayse.kaya@ornekfirma.com.tr", "mailto:ayse.kaya@ornekfirma.com.tr"),
            ("tel:+905324182764", "tel:+905324182764"),
            ("tel:(0212)555-01.47", "tel:(0212)555-01.47"),
        ];
        for (url, expected) in opens {
            assert_eq!(openable_url(url).as_deref(), Some(expected), "{url:?} should open");
        }
        let refused = [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "jAvAsCrIpT:alert(document.cookie)",
            " javascript:alert(1)",
            "file:///etc/passwd",
            "FILE:///Users",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "smb://evil.example/share",
            "facetime:+905324182764",
            "x-apple.systempreferences:com.apple.Keyboard-Settings.extension",
            "https://example.com/\u{0}",
            "https://example.com/a\nb",
            "https://exa mple.com",
            "https:///etc/passwd",
            "https:",
            "mailto:a@b.co?attach=/etc/passwd",
            "mailto:a@b.co?bcc=c@d.co",
            "mailto:a@b.co,c@d.co",
            "mailto:a@b.co%3Fattach=x",
            "mailto:",
            "mailto:not-an-address",
            "tel:+90abc",
            "tel:12345;phone-context=evil",
            "tel:+",
            "tel:1",
            "",
        ];
        for url in refused {
            assert_eq!(openable_url(url), None, "{url:?} must not open");
        }
    }
}
