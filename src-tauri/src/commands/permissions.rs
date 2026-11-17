//! Permission commands. Owner: app shell (first run and permissions, milestone 1). The checks are
//! `permissions.rs`.

use glimpsfy_capture::PermissionState;
use tauri::AppHandle;

use crate::permissions::{self, Permission, Permissions};

/// The state of every permission, without prompting. When screen recording reads as missing, a
/// fresh process checks whether it was granted meanwhile (`restartNeeded`), so this runs off the
/// main thread.
#[tauri::command]
pub async fn permissions_status(app: AppHandle) -> Permissions {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || permissions::status_fresh(&handle))
        .await
        .unwrap_or_else(|_| permissions::status(&app))
}

/// Asks the system for one permission; the first request may show the system prompt.
#[tauri::command]
pub fn permissions_request(app: AppHandle, permission: Permission) -> PermissionState {
    permissions::request(&app, permission)
}

/// Opens the pane of System Settings where the permission is granted.
#[tauri::command]
pub fn permissions_open_settings(app: AppHandle, permission: Permission) {
    permissions::open_settings(&app, permission);
}
