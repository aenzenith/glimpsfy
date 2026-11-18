//! Window commands: open the app's windows, fit the toast, the popover, the pins, the pickers,
//! links and quitting. Owner: app shell (milestone 1). The native side is `popover.rs`,
//! `pins.rs` and the app shell section of `macos.rs`.

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

    /// The window's first size in points (DESIGN.md's window sizes).
    pub fn size(self) -> (f64, f64) {
        match self {
            WindowKind::Editor => (1120.0, 760.0),
            WindowKind::Recorder => (1120.0, 720.0),
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
    Recording,
    Ocr,
    Upload,
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
        use tauri_plugin_clipboard_manager::ClipboardExt;
        app.clipboard().write_text(text).map_err(|e| CommandError::internal(e.to_string()))
    }
}

/// Opens a web page in the default browser. Only `http` and `https` addresses open, so a provider
/// description or a pasted link cannot start another app.
#[tauri::command]
pub fn app_open_url(app: AppHandle, url: String) -> CommandResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(CommandError::invalid("only http and https links open in the browser"));
    }
    app.opener().open_url(url.trim(), None::<&str>).map_err(|e| CommandError::internal(e.to_string()))
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

#[tauri::command]
pub fn app_open_system_settings(app: AppHandle, pane: SystemPane) -> CommandResult<()> {
    use tauri_plugin_opener::OpenerExt;
    if !cfg!(target_os = "macos") {
        return Err(CommandError::not_implemented("System Settings panes outside macOS"));
    }
    let url = match pane {
        SystemPane::KeyboardShortcuts => "x-apple.systempreferences:com.apple.Keyboard-Settings.extension?Shortcuts",
        SystemPane::LoginItems => "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
    };
    app.opener().open_url(url, None::<&str>).map_err(|e| CommandError::internal(e.to_string()))
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

/// Lets the user pick one file of the given types; `None` when they cancel.
#[tauri::command]
pub async fn app_choose_file(app: AppHandle, filters: Vec<FileFilter>) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file();
    for filter in &filters {
        let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(&filter.name, &extensions);
    }
    dialog.blocking_pick_file().and_then(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned())
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
        let _ = (app, window, file, image);
        Err(CommandError::not_implemented("dragging a capture out outside macOS"))
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
