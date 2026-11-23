//! Colour picker commands: what the picker remembers, and copying a colour or a contrast line.
//! Owner: the colour picker (D29). The page reads the frame and computes every value itself
//! (`src/lib/color.ts`); the core keeps the recent colours (`colors.rs`), writes the clipboard
//! and shows the toast.

use glimpsfy_capture::DisplayId;
use tauri::AppHandle;

use super::error::{CommandError, CommandResult};
use crate::colors::{self, ColorFormat, ColorState};
use crate::trace;
use crate::windows::{self, Toast, ToastKind};

/// The longest text a copy takes: a contrast line in the longest language fits many times over.
const MAX_TEXT: usize = 512;

/// The format a click copies in and the recent colours, newest first.
#[tauri::command]
pub fn color_state(app: AppHandle) -> ColorState {
    colors::state(&app)
}

/// Puts `text` on the clipboard and shows the toast with `swatch`. With a `format` the text is one
/// colour, and `swatch` (its CSS colour) goes first in the recent colours; without one it is a
/// contrast line, which the recent colours do not keep. A recent colour that cannot be written
/// costs only the strip, so the copy still succeeds.
#[tauri::command]
pub fn color_copy(
    app: AppHandle,
    text: String,
    swatch: Option<String>,
    format: Option<ColorFormat>,
    display_id: Option<DisplayId>,
) -> CommandResult<ColorState> {
    if text.trim().is_empty() || text.len() > MAX_TEXT {
        return Err(CommandError::invalid("a colour copy needs a short, non-empty text"));
    }
    let swatch = swatch.filter(|s| colors::is_swatch(s));
    crate::output::copy_text(&text)?;
    let state = match (&swatch, format) {
        (Some(swatch), Some(_)) => colors::remember(&app, swatch).unwrap_or_else(|error| {
            trace::log(format!("[glimpsfy] colors.json: {error}"));
            colors::state(&app)
        }),
        _ => colors::state(&app),
    };
    let mut toast = Toast::new(ToastKind::ColorCopied);
    toast.text = Some(text);
    toast.format = format.map(ColorFormat::label);
    toast.swatch = swatch;
    windows::toast(&app, display_id, toast);
    Ok(state)
}

/// Remembers the format a click copies in (⌥ in the picker), for the next time it opens.
#[tauri::command]
pub fn color_set_format(app: AppHandle, format: ColorFormat) -> CommandResult<()> {
    colors::set_format(&app, format).map(|_| ()).map_err(CommandError::from)
}
