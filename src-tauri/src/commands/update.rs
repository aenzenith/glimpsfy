//! Update commands. Owner: app shell. The updater itself is `update.rs`; its state reaches every
//! window as `update://changed`, so these commands only read it or start something.

use tauri::{AppHandle, State};

use super::error::{CommandError, CommandResult, ErrorCode};
use crate::update::{Update, UpdateView};

/// The update's state now, for a page that opens after the last `update://changed`.
#[tauri::command]
pub fn update_state(update: State<'_, Update>) -> UpdateView {
    update.view()
}

/// Asks for a new version now (Settings' "Check now"). The answer arrives as events. Fails with
/// `offline` while Settings › General keeps the app offline, and nothing changes then.
#[tauri::command]
pub fn update_check(app: AppHandle, update: State<'_, Update>) -> CommandResult<()> {
    Ok(update.check(&app)?)
}

/// Installs the downloaded update and restarts. It returns only when the install failed.
#[tauri::command]
pub fn update_install(app: AppHandle, update: State<'_, Update>) -> CommandResult<()> {
    update.install(&app).map_err(|message| CommandError::new(ErrorCode::Internal, message))
}
