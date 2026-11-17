//! The app's own folders (config, data, cache) and the default user folders, which a dev run
//! moves under `GLIMPSFY_HOME` so it never reads or writes the user's real ones.

use std::path::PathBuf;

use tauri::{AppHandle, Manager, Runtime};

use crate::dev;

/// The folder a dev run keeps everything in, when `GLIMPSFY_HOME` says so (debug builds only).
pub fn dev_home() -> Option<&'static PathBuf> {
    dev::options().home.as_ref()
}

/// `settings.json`'s folder.
pub fn config_dir<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<PathBuf> {
    match dev_home() {
        Some(home) => Ok(home.join("config")),
        None => app.path().app_config_dir(),
    }
}

/// The history, the inbox and the OCR models.
pub fn data_dir<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<PathBuf> {
    match dev_home() {
        Some(home) => Ok(home.join("data")),
        None => app.path().app_data_dir(),
    }
}

/// Pins, staged renders, posters and exports.
pub fn cache_dir<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<PathBuf> {
    match dev_home() {
        Some(home) => Ok(home.join("cache")),
        None => app.path().app_cache_dir(),
    }
}

/// The default save folder: Pictures › Glimpsfy.
pub fn default_pictures() -> PathBuf {
    match dev_home() {
        Some(home) => home.join("Pictures").join("Glimpsfy"),
        None => dirs::picture_dir().or_else(dirs::home_dir).unwrap_or_default().join("Glimpsfy"),
    }
}

/// The default recordings folder: Movies › Glimpsfy.
pub fn default_movies() -> PathBuf {
    match dev_home() {
        Some(home) => home.join("Movies").join("Glimpsfy"),
        None => dirs::video_dir().or_else(dirs::home_dir).unwrap_or_default().join("Glimpsfy"),
    }
}

/// The keychain service prefix a dev run uses instead of the real one, so its secrets stay apart.
pub fn keychain_prefix() -> Option<String> {
    dev_home().map(|_| format!("{}.dev", glimpsfy_upload::secrets::SERVICE_PREFIX))
}
