//! Output commands: copy, save, save as and reveal a capture. Owner: capture and output
//! (milestone 1). The work is `output.rs`; these run it off the main thread, so the overlays
//! never stall.
//!
//! After every output the core sends `output://handoff` with an [`events::CaptureHandoff`],
//! which holds everything the history needs to store the capture; the history subscribes to it,
//! and output never writes to the history itself.

use std::path::PathBuf;

use glimpsfy_capture::ColorSpace;
use glimpsfy_history::EntryKind;
use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use super::capture::{CaptureId, CaptureMode, Selection};
use super::error::{CommandError, CommandResult, ErrorCode};

/// With `output://copied` and `output://saved`, and as the commands' result.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDone {
    pub capture_id: CaptureId,
    /// The image's size in pixels.
    pub width: u32,
    pub height: u32,
    /// The encoded file's size.
    pub bytes: u64,
    /// The saved file; `None` for a copy.
    pub path: Option<String>,
    /// The file format: "png", "jpeg" or "webp" (a copy is always "png").
    pub format: &'static str,
    /// The save folder of the settings when it could not be reached or written, so the file went
    /// to the default folder (Pictures › Glimpsfy) instead.
    pub fallback_from: Option<String>,
}

/// With `output://failed`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFailed {
    pub capture_id: CaptureId,
    pub code: ErrorCode,
    pub message: String,
}

/// Event names of the output area and their payloads.
pub mod events {
    use super::*;

    /// One image of a capture left the app (copied, saved or both), to every window and to the
    /// core's own listeners. Payload: [`CaptureHandoff`]. Sent once per image, after
    /// `output://copied` or `output://saved`. The history records captures from it; output never
    /// writes to the history itself.
    pub const HANDOFF: &str = "output://handoff";

    /// Everything the history needs to keep one delivered image. Optional fields are `null`
    /// when the system did not tell; the history ignores the fields it has no use for.
    #[derive(Clone, Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CaptureHandoff {
        /// Always `screenshot` from output (recordings and text copies hand off elsewhere).
        pub kind: EntryKind,
        /// The capture the image belongs to; 0 for an image from the editor.
        pub capture_id: CaptureId,
        /// The mode that took it (`area`, `window`, `display`, `allDisplays`…).
        pub mode: CaptureMode,
        /// Whether the image is on the clipboard now.
        pub copied: bool,
        /// The file the user saved, which the history must never delete on its own.
        pub saved_path: Option<PathBuf>,
        /// A PNG of a clipboard-only capture in the app data folder (`history-inbox/`), written
        /// only while the history is on. The history moves it into its own folder before it writes
        /// the entry (and back when the entry cannot be written), so output's deletion of files
        /// there that are more than a day old only meets files the history did not take.
        pub image_path: Option<PathBuf>,
        /// The format of `saved_path`: "png", "jpeg" or "webp" (`image_path` is always PNG).
        pub format: &'static str,
        /// The delivered image's size in pixels, after any scaling to 1×.
        pub width: u32,
        pub height: u32,
        /// Pixels per point of the delivered image: 2 for a Retina capture kept at 2×, 1 after
        /// scaling to 1×.
        pub scale: f64,
        /// The size of the saved file, or of the PNG for a copy.
        pub bytes: u64,
        pub color_space: ColorSpace,
        /// The frontmost app at capture time, or the captured window's app.
        pub app_name: Option<String>,
        pub bundle_id: Option<String>,
        /// The captured window's title, or the frontmost window's for an area.
        pub window_title: Option<String>,
        pub display_id: Option<u32>,
        /// The display's name ("Built-in Retina Display", "LG HDR 4K").
        pub display_name: Option<String>,
        /// When the pixels were captured, in milliseconds since the Unix epoch.
        pub created_at_ms: i64,
    }
}

async fn off_main<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| CommandError::internal(e.to_string()))?
}

/// Puts the selection on the clipboard as PNG and closes the overlay (⌘C, Return).
#[tauri::command]
pub async fn output_copy(app: AppHandle, selection: Selection) -> CommandResult<OutputDone> {
    off_main(move || crate::output::copy(&app, selection)).await
}

/// Saves the selection as a new file in the folder of Settings › Output, named by its template
/// and in its format, and closes the overlay (⌘S). An existing file is never replaced.
#[tauri::command]
pub async fn output_save(app: AppHandle, selection: Selection) -> CommandResult<OutputDone> {
    off_main(move || crate::output::save(&app, selection)).await
}

/// Saves the selection to a path (Save as…, ⇧⌘S). Without a path the overlay closes first and
/// the system's save dialog asks for one; a cancelled dialog answers `cancelled`. The format
/// follows the chosen extension (`.png`, `.jpg`, `.jpeg`, `.webp`), and a path that exists gets
/// " (2)" instead of being replaced.
#[tauri::command]
pub async fn output_save_as(app: AppHandle, selection: Selection, path: Option<PathBuf>) -> CommandResult<OutputDone> {
    off_main(move || crate::output::save_as(&app, selection, path)).await
}

/// Shows a saved file in Finder (or the platform's file manager).
#[tauri::command]
pub fn output_reveal(app: AppHandle, path: PathBuf) -> CommandResult<()> {
    if !path.exists() {
        return Err(CommandError::not_found(path.to_string_lossy()));
    }
    app.opener().reveal_item_in_dir(&path).map_err(|e| CommandError::new(ErrorCode::Io, e.to_string()))
}
