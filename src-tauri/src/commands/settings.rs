//! Settings commands. Owner: settings (milestone 1). The store is `settings.rs`.
//!
//! A save applies what a setting changes outside the file at once: the global shortcuts, the
//! login item and the menu bar icon. Every window hears `settings://changed` and follows.

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use super::error::{CommandError, CommandResult};
use crate::output::{file_stem_titled, Naming};
use crate::settings::{ImageFormat, Settings, ShortcutCleared};
use crate::shortcuts::ShortcutProblem;
use crate::{events, shortcuts, AppState};

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> Settings {
    state.settings.get()
}

/// The saved settings and what the save could not do as asked.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSaved {
    pub settings: Settings,
    /// Shortcuts the system refused to register (taken by another app, or not a key).
    pub shortcut_problems: Vec<ShortcutProblem>,
    /// Actions that lost their keys because another action already held them.
    pub cleared_shortcuts: Vec<ShortcutCleared>,
}

/// Saves the whole settings object, applies what changed and tells every page with
/// `settings://changed`. Turning on start at login can fail (the login item is refused); the
/// setting then stays off and the command fails with `io`. `upload.confirmedProviders` is never
/// taken from the payload (`SettingsStore::save_from_page`).
#[tauri::command]
pub fn settings_set(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> CommandResult<SettingsSaved> {
    save(&app, &state, settings)
}

/// Puts every setting back to its default. The first-run mark stays, so the welcome window does
/// not come back, and so do the upload consents, which only Forget confirmation takes back.
#[tauri::command]
pub fn settings_reset(app: AppHandle, state: State<'_, AppState>) -> CommandResult<SettingsSaved> {
    let mut defaults = Settings::default();
    defaults.general.first_run_done = state.settings.get().general.first_run_done;
    save(&app, &state, defaults)
}

/// Forgets the first-upload confirmation of a provider (every destination it was confirmed
/// for), so its next upload asks again. Settings › Upload › Forget confirmation calls it.
#[tauri::command]
pub fn settings_revoke_consent(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
) -> CommandResult<Settings> {
    let saved = state.settings.revoke_consent(&provider_id)?;
    let _ = app.emit(events::SETTINGS_CHANGED, saved.clone());
    Ok(saved)
}

fn save(app: &AppHandle, state: &AppState, settings: Settings) -> CommandResult<SettingsSaved> {
    let previous = state.settings.get();
    let (mut saved, notes) = state.settings.save_from_page(settings)?;

    let mut login_error = None;
    if saved.general.start_at_login != previous.general.start_at_login {
        if let Err(error) = login_item::set(app, saved.general.start_at_login) {
            // The file must not claim a login item the system refused.
            let mut back = saved.clone();
            back.general.start_at_login = previous.general.start_at_login;
            saved = state.settings.save(back)?.0;
            login_error = Some(error);
        }
    }
    if saved.general.show_menu_bar_icon != previous.general.show_menu_bar_icon {
        crate::tray::set_visible(app, saved.general.show_menu_bar_icon);
    }
    if saved.agents.enabled != previous.agents.enabled {
        crate::agents::apply(app, saved.agents.enabled);
    }
    let shortcut_problems = shortcuts::apply(app, &saved.shortcuts);
    let _ = app.emit(events::SETTINGS_CHANGED, saved.clone());
    if let Some(error) = login_error {
        return Err(CommandError::new(
            super::error::ErrorCode::Io,
            format!("the login item could not be changed: {error}"),
        ));
    }
    Ok(SettingsSaved { settings: saved, shortcut_problems, cleared_shortcuts: notes.cleared_shortcuts })
}

/// The default shortcuts of this platform, for Settings › Shortcuts › Restore defaults, which
/// saves them through `settings_set` like any other change.
#[tauri::command]
pub fn settings_default_shortcuts() -> crate::settings::Shortcuts {
    crate::settings::Shortcuts::default()
}

/// While the shortcut recorder listens, Glimpsfy's global shortcuts are off, so a key that one
/// of them holds reaches the page instead of starting a capture. `paused: false` registers the
/// saved shortcuts again and returns what the system refused.
#[tauri::command]
pub fn settings_pause_shortcuts(app: AppHandle, state: State<'_, AppState>, paused: bool) -> Vec<ShortcutProblem> {
    if paused {
        shortcuts::apply(&app, &crate::settings::Shortcuts::none());
        Vec::new()
    } else {
        shortcuts::apply(&app, &state.settings.get().shortcuts)
    }
}

// ------------------------------------------------------------------------- file name preview

/// The tokens a file name template knows (`output::file_stem_titled`).
const TOKENS: [&str; 7] = ["date", "time", "app", "title", "counter", "width", "height"];

/// Why a template cannot name a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TemplateProblem {
    /// Nothing but spaces.
    Empty,
    /// A character no file name may hold, such as `/` or `:`.
    Character { character: String },
    /// A `{token}` Glimpsfy does not know, with the nearest one it does.
    UnknownToken { token: String, suggestion: Option<String> },
}

/// What a template names a capture, computed by the same function that names saved files, so
/// the preview never disagrees with the result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamePreview {
    /// The file name with its extension, or `None` while the template has a problem.
    pub file_name: Option<String>,
    pub problem: Option<TemplateProblem>,
}

/// Fills `template` with the current date and time, an example app ("Safari") and window title
/// ("Glimpsfy"), and a 1280 × 720 capture, and adds the extension of `format`.
#[tauri::command]
pub fn settings_preview_name(template: String, format: ImageFormat) -> NamePreview {
    preview_name(&template, format, chrono::Local::now())
}

fn preview_name(template: &str, format: ImageFormat, now: chrono::DateTime<chrono::Local>) -> NamePreview {
    if let Some(problem) = template_problem(template) {
        return NamePreview { file_name: None, problem: Some(problem) };
    }
    let (date, time) = Naming::clock(now);
    let naming = Naming { date, time, app: Some("Safari"), width: 1280, height: 720 };
    NamePreview {
        file_name: Some(format!("{}.{}", file_stem_titled(template, &naming, Some("Glimpsfy"), 1), format.extension())),
        problem: None,
    }
}

fn template_problem(template: &str) -> Option<TemplateProblem> {
    if template.trim().is_empty() {
        return Some(TemplateProblem::Empty);
    }
    if let Some(c) = template.chars().find(|c| matches!(c, '/' | '\\' | ':') || c.is_control()) {
        return Some(TemplateProblem::Character { character: c.to_string() });
    }
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        let token = &after[..close];
        if !TOKENS.contains(&token) {
            let suggestion = TOKENS.iter().find(|known| distance(known, token) <= 2).map(|s| s.to_string());
            return Some(TemplateProblem::UnknownToken { token: token.to_string(), suggestion });
        }
        rest = &after[close + 1..];
    }
    None
}

/// The edit distance between two short words.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (row[j + 1] + 1).min(row[j] + 1).min(previous + usize::from(ca != cb));
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

// ------------------------------------------------------------------------------- folders

/// What a chosen folder allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderStatus {
    pub exists: bool,
    /// Whether Glimpsfy may write in it; `false` while it does not exist.
    pub writable: bool,
}

/// Checks a save or recordings folder. A missing folder is made at the next save, so it is a
/// warning; a folder Glimpsfy may not write in makes every save fail.
#[tauri::command]
pub async fn settings_folder_status(path: std::path::PathBuf) -> FolderStatus {
    tauri::async_runtime::spawn_blocking(move || folder_status(&path))
        .await
        .unwrap_or(FolderStatus { exists: false, writable: false })
}

fn folder_status(path: &std::path::Path) -> FolderStatus {
    if !path.is_dir() {
        return FolderStatus { exists: false, writable: false };
    }
    // Permissions bits do not tell the whole story (ACLs, read-only volumes, sandboxes), so the
    // check writes and removes an empty file, as a save would.
    let probe = path.join(format!(".glimpsfy-write-check-{}", std::process::id()));
    let writable = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    if writable {
        let _ = std::fs::remove_file(&probe);
    }
    FolderStatus { exists: true, writable }
}

// ---------------------------------------------------------------------------------- login item

/// Start at login. On macOS Glimpsfy writes its launch agent itself, so that the agent names the
/// app's bundle (`AssociatedBundleIdentifiers`): the autostart plugin leaves that list empty, and
/// System Settings › Login Items then lists the agent under the signing certificate's owner with
/// no icon. The agent is the plugin's own file, so one written before is rewritten in place.
/// Elsewhere the plugin's entry is used as it is.
pub(crate) mod login_item {
    use tauri::AppHandle;

    pub fn set(app: &AppHandle, on: bool) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        let result = {
            let agent = agent(app)?;
            if on {
                agent.enable()
            } else {
                agent.disable()
            }
        };
        #[cfg(not(target_os = "macos"))]
        let result = crate::desktop::set_login_item(app, on);
        result.map_err(|error| error.to_string())
    }

    /// The agent as the plugin writes it, plus the bundle it belongs to. Its program is the
    /// executable inside the bundle with links followed, because System Settings matches the
    /// executable's Team ID against the bundle's.
    #[cfg(target_os = "macos")]
    fn agent(app: &AppHandle) -> Result<auto_launch::AutoLaunch, String> {
        let exe = std::env::current_exe().and_then(|path| path.canonicalize()).map_err(|error| error.to_string())?;
        auto_launch::AutoLaunchBuilder::new()
            .set_app_name(&app.package_info().name)
            .set_app_path(&exe.display().to_string())
            .set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
            .set_bundle_identifiers(&[app.config().identifier.as_str()])
            .build()
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_problems_name_the_nearest_token() {
        assert_eq!(template_problem("  "), Some(TemplateProblem::Empty));
        assert_eq!(template_problem("a/b"), Some(TemplateProblem::Character { character: "/".into() }));
        assert_eq!(
            template_problem("Glimpsfy {dat} at {time}"),
            Some(TemplateProblem::UnknownToken { token: "dat".into(), suggestion: Some("date".into()) })
        );
        assert_eq!(template_problem("{title} {date} at {time} {width}×{height}"), None);
    }
}
