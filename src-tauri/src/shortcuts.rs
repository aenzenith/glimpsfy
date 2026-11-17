//! Global shortcuts from Settings › Shortcuts to capture modes.
//!
//! Only actions that work in this build are registered, so Glimpsfy never takes a key from other
//! apps for an action that would do nothing. A key that cannot be registered (taken by another
//! app, or not a key at all) is reported with `shortcut://failed` and skipped.
//!
//! Esc is registered too, but only while a delayed capture counts down, so it can cancel the
//! countdown from any app (`session/delay.rs`).

use std::str::FromStr;
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Emitter, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::commands::capture::CaptureMode;
use crate::commands::windows::WindowKind;
use crate::settings::Shortcuts;
use crate::{events, session, trace, windows};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    CaptureArea,
    CaptureWindow,
    CaptureDisplay,
    CaptureAllDisplays,
    CaptureAreaDelayed,
    RepeatLastArea,
    CopyText,
    RecordScreen,
    RecordDisplay,
    PauseRecording,
    MuteMicrophone,
    DiscardRecording,
    OpenHistory,
}

impl Action {
    /// Whether the action does something in this build.
    fn works(self) -> bool {
        self.mode().is_some() || self == Action::OpenHistory || crate::recording::handles(self)
    }

    /// The capture mode the action starts.
    fn mode(self) -> Option<CaptureMode> {
        Some(match self {
            Action::CaptureArea => CaptureMode::Area,
            Action::CaptureWindow => CaptureMode::Window,
            Action::CaptureDisplay => CaptureMode::Display,
            Action::CaptureAllDisplays => CaptureMode::AllDisplays,
            Action::CaptureAreaDelayed => CaptureMode::AreaDelayed,
            Action::RepeatLastArea => CaptureMode::RepeatLastArea,
            Action::CopyText => CaptureMode::CopyText,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutProblem {
    pub action: Action,
    pub accelerator: String,
    pub message: String,
}

static BOUND: Mutex<Vec<(Shortcut, Action)>> = Mutex::new(Vec::new());
/// Esc, while a delayed capture counts down.
static ESCAPE: Mutex<Option<Shortcut>> = Mutex::new(None);

pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            // The latency budget starts here.
            let at = Instant::now();
            let escape = ESCAPE.lock().unwrap_or_else(|p| p.into_inner()).as_ref() == Some(shortcut);
            if escape {
                session::delay::cancel(app);
                return;
            }
            let action =
                BOUND.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(s, _)| s == shortcut).map(|(_, a)| *a);
            if let Some(action) = action {
                run(app, action, at);
            }
        })
        .build()
}

pub(crate) fn run(app: &AppHandle, action: Action, at: Instant) {
    let result = match (action, action.mode()) {
        (_, Some(mode)) => session::start(app, mode, at),
        (Action::OpenHistory, None) => {
            windows::open(app, WindowKind::History);
            Ok(())
        }
        (recording, None) if crate::recording::handles(recording) => {
            crate::recording::shortcut(app, recording);
            Ok(())
        }
        (other, None) => {
            trace::log(format!("[glimpsfy] shortcut for {other:?} is not implemented yet"));
            Ok(())
        }
    };
    if let Err(error) = result {
        trace::log(format!("[glimpsfy] {action:?}: {error}"));
    }
}

/// The keys registered now, with their actions, for the dev driver.
#[cfg(debug_assertions)]
pub(crate) fn bound() -> Vec<(String, Action)> {
    BOUND.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(s, a)| (s.into_string(), *a)).collect()
}

/// Registers the working actions' keys, replacing whatever was registered before.
pub fn apply(app: &AppHandle, shortcuts: &Shortcuts) -> Vec<ShortcutProblem> {
    let manager = app.global_shortcut();
    let mut bound = BOUND.lock().unwrap_or_else(|p| p.into_inner());
    for (shortcut, _) in bound.drain(..) {
        let _ = manager.unregister(shortcut);
    }

    let wanted = [
        (Action::CaptureArea, &shortcuts.capture_area),
        (Action::CaptureWindow, &shortcuts.capture_window),
        (Action::CaptureDisplay, &shortcuts.capture_display),
        (Action::CaptureAllDisplays, &shortcuts.capture_all_displays),
        (Action::CaptureAreaDelayed, &shortcuts.capture_area_delayed),
        (Action::RepeatLastArea, &shortcuts.repeat_last_area),
        (Action::CopyText, &shortcuts.copy_text),
        (Action::RecordScreen, &shortcuts.record_screen),
        (Action::RecordDisplay, &shortcuts.record_display),
        (Action::PauseRecording, &shortcuts.pause_recording),
        (Action::MuteMicrophone, &shortcuts.mute_microphone),
        (Action::DiscardRecording, &shortcuts.discard_recording),
        (Action::OpenHistory, &shortcuts.open_history),
    ];

    let mut problems = Vec::new();
    for (action, accelerator) in wanted {
        let Some(accelerator) = accelerator.as_deref().filter(|a| !a.is_empty()) else {
            continue;
        };
        if !action.works() {
            continue;
        }
        let problem = |message: String| ShortcutProblem { action, accelerator: accelerator.to_string(), message };
        match Shortcut::from_str(accelerator) {
            Ok(shortcut) => match manager.register(shortcut) {
                Ok(()) => bound.push((shortcut, action)),
                Err(error) => problems.push(problem(error.to_string())),
            },
            Err(error) => problems.push(problem(error.to_string())),
        }
    }
    for problem in &problems {
        trace::log(format!(
            "[glimpsfy] shortcut {} for {:?}: {}",
            problem.accelerator, problem.action, problem.message
        ));
        let _ = app.emit(events::SHORTCUT_FAILED, problem.clone());
    }
    problems
}

/// Registers Esc for a countdown; a key Glimpsfy cannot take only costs the shortcut.
pub fn bind_escape(app: &AppHandle) {
    let mut escape = ESCAPE.lock().unwrap_or_else(|p| p.into_inner());
    if escape.is_some() {
        return;
    }
    match Shortcut::from_str("Escape") {
        Ok(shortcut) => match app.global_shortcut().register(shortcut) {
            Ok(()) => *escape = Some(shortcut),
            Err(error) => trace::log(format!("[glimpsfy] Esc for the countdown: {error}")),
        },
        Err(error) => trace::log(format!("[glimpsfy] Esc for the countdown: {error}")),
    }
}

/// Gives Esc back to the other apps.
pub fn unbind_escape(app: &AppHandle) {
    if let Some(shortcut) = ESCAPE.lock().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = app.global_shortcut().unregister(shortcut);
    }
}
