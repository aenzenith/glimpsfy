//! The image on the clipboard for Open clipboard in editor and Pin clipboard to screen (A1): what
//! counts as one, and the two actions that their shortcuts and the menu bar menu run. Owner: app
//! shell. The reads themselves live in `macos.rs` and `desktop/`; ⌘V in the editor goes through
//! `editor_open_clipboard` (`commands/render.rs`).

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

use crate::commands::error::CommandResult;
use crate::windows::{Toast, ToastKind};
use crate::{i18n, pins, AppState};

/// An image the clipboard holds.
pub enum Pasted {
    /// An image file the clipboard names, as a file manager's Copy leaves it. The editor opens it
    /// by its path, so it keeps its name, its format and its folder.
    File(PathBuf),
    /// Image data: a PNG, a JPEG, or a PNG made from another format with its colour profile and
    /// resolution kept.
    Image(Vec<u8>),
}

/// The extensions of the formats the editor's web view decodes (`editor_open`).
const IMAGE_EXTENSIONS: [&str; 10] = ["png", "jpg", "jpeg", "heic", "heif", "webp", "tif", "tiff", "gif", "bmp"];

/// What the clipboard holds as an image. Call it off the main thread: Linux's answer comes
/// through the main thread, which must not be the one waiting for it.
pub fn read() -> Option<Pasted> {
    #[cfg(target_os = "macos")]
    return crate::macos::read_image();
    #[cfg(not(target_os = "macos"))]
    crate::desktop::read_image()
}

/// The first image file among the files a clipboard names. `None` when none of them is an image
/// (a PDF, a folder, a text file), because the picture a file manager puts beside them is their
/// icon, which is no image the user copied.
pub(crate) fn first_image(files: Vec<PathBuf>) -> Option<Pasted> {
    files.into_iter().find(|path| is_image_file(path)).map(Pasted::File)
}

fn is_image_file(path: &Path) -> bool {
    let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    extension.is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str())) && path.is_file()
}

/// The name of image data from the clipboard, in the language of Settings › General.
pub fn pasted_name(app: &AppHandle) -> String {
    let language = app.state::<AppState>().settings.get().general.language;
    i18n::text(language, "clipboard.name", &[])
}

/// Open clipboard in editor, from its shortcut or the menu. Without an image the amber toast says
/// so; the editor's own ⌘V says it in the editor's hint instead.
pub fn open_in_editor(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match crate::commands::render::editor_open_clipboard(app.clone()).await {
            Ok(Some(_)) => {}
            Ok(None) => empty(&app),
            Err(error) => crate::windows::toast_failure(&app, None, &error),
        }
    });
}

/// Pin clipboard to screen, from its shortcut or the menu. The pin lands centred on the screen
/// under the pointer, at the pixels per point of the image's resolution or else of that screen.
pub fn pin(app: &AppHandle) {
    let app = app.clone();
    // Reading files and making the window block, and the screen's scale waits for the main
    // thread, where shortcuts arrive.
    tauri::async_runtime::spawn_blocking(move || match read() {
        None => empty(&app),
        Some(pasted) => {
            if let Err(error) = pin_pasted(&app, pasted) {
                crate::windows::toast_failure(&app, None, &error);
            }
        }
    });
}

fn pin_pasted(app: &AppHandle, pasted: Pasted) -> CommandResult<pins::PinInfo> {
    let (bytes, name) = match pasted {
        Pasted::File(path) => {
            let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (std::fs::read(&path)?, name)
        }
        Pasted::Image(bytes) => (bytes, pasted_name(app)),
    };
    let bytes = pinnable(bytes);
    // 0 lets the pin take the scale of the screen under the pointer.
    let scale = crate::commands::render::png_facts(&bytes).and_then(|(_, _, scale)| scale).unwrap_or(0.0);
    pins::open_bytes(app, bytes, name, None, scale, None)
}

/// A pin shows PNG and JPEG only, so on macOS any other format ImageIO reads (HEIC, TIFF, WebP)
/// becomes a PNG that keeps its colour profile and resolution. Elsewhere such a file reaches
/// `pins::open_bytes` as it is, which refuses it with its own message.
fn pinnable(bytes: Vec<u8>) -> Vec<u8> {
    #[cfg(target_os = "macos")]
    if pins::image_size(&bytes).is_none() {
        if let Some(png) = crate::macos::png_of(&bytes) {
            return png;
        }
    }
    bytes
}

fn empty(app: &AppHandle) {
    crate::windows::toast(app, None, Toast::new(ToastKind::ClipboardEmpty));
}
