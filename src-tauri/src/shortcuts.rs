//! Global shortcuts from Settings › Shortcuts to capture modes.
//!
//! Only actions that work in this build are registered, so Glimpsfy never takes a key from other
//! apps for an action that would do nothing. A key that cannot be registered (taken by another
//! app, or not a key at all) is reported with `shortcut://failed` and skipped.
//!
//! Esc is registered too, but only while a delayed capture counts down, so it can cancel the
//! countdown from any app (`session/delay.rs`).
//!
//! Two sets live only for a while, so they take nothing from other apps the rest of the time:
//! Draw on screen and Spotlight while a take records or pauses (`bind_take`), and the keys of
//! drawing mode (P H A R, 1 2 3, ⌘Z, ⌫ and Esc) while it is on (`bind_ink`). They reach Glimpsfy
//! while another app stays in front with the keyboard, which is how drawing mode never takes the
//! focus (`recording/present.rs`).

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Emitter, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::agents::ApprovalKey;
use crate::commands::capture::CaptureMode;
use crate::commands::windows::WindowKind;
use crate::recording::present::{InkKey, INK_KEYS};
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
    MeasureScreen,
    DrawOnScreen,
    Spotlight,
    PickColor,
    /// Starts an interval series with the plan of Settings, or stops the one that runs.
    IntervalCapture,
    /// Selects an area and goes straight into the scrolling capture (macOS).
    ScrollCapture,
}

/// Every action, for the command line.
const ACTIONS: [Action; 19] = [
    Action::CaptureArea,
    Action::CaptureWindow,
    Action::CaptureDisplay,
    Action::CaptureAllDisplays,
    Action::CaptureAreaDelayed,
    Action::RepeatLastArea,
    Action::CopyText,
    Action::RecordScreen,
    Action::RecordDisplay,
    Action::PauseRecording,
    Action::MuteMicrophone,
    Action::DiscardRecording,
    Action::OpenHistory,
    Action::MeasureScreen,
    Action::DrawOnScreen,
    Action::Spotlight,
    Action::PickColor,
    Action::IntervalCapture,
    Action::ScrollCapture,
];

impl Action {
    /// The action's name on the command line: its name in kebab-case (`capture-area`,
    /// `record-screen`, `open-history`).
    fn cli_name(self) -> &'static str {
        match self {
            Action::CaptureArea => "capture-area",
            Action::CaptureWindow => "capture-window",
            Action::CaptureDisplay => "capture-display",
            Action::CaptureAllDisplays => "capture-all-displays",
            Action::CaptureAreaDelayed => "capture-area-delayed",
            Action::RepeatLastArea => "repeat-last-area",
            Action::CopyText => "copy-text",
            Action::RecordScreen => "record-screen",
            Action::RecordDisplay => "record-display",
            Action::PauseRecording => "pause-recording",
            Action::MuteMicrophone => "mute-microphone",
            Action::DiscardRecording => "discard-recording",
            Action::OpenHistory => "open-history",
            Action::MeasureScreen => "measure-screen",
            Action::PickColor => "pick-color",
            Action::DrawOnScreen => "draw-on-screen",
            Action::Spotlight => "spotlight",
            Action::IntervalCapture => "interval-capture",
            Action::ScrollCapture => "scroll-capture",
        }
    }

    /// Whether the action does something in this build.
    fn works(self) -> bool {
        self.mode().is_some()
            || matches!(self, Action::OpenHistory | Action::IntervalCapture)
            || crate::recording::handles(self)
    }

    /// Whether the action's key is registered only while a take records or pauses (`bind_take`)
    /// rather than for good (`apply`).
    fn take_only(self) -> bool {
        matches!(self, Action::DrawOnScreen | Action::Spotlight)
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
            Action::MeasureScreen => CaptureMode::Ruler,
            Action::PickColor => CaptureMode::PickColor,
            // The scrolling capture streams through ScreenCaptureKit and posts scroll events
            // through Core Graphics, which only the macOS build has.
            Action::ScrollCapture if cfg!(target_os = "macos") => CaptureMode::ScrollCapture,
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

// The plugin registers and unregisters on the main thread and blocks its caller until it has,
// and the handler below runs on the main thread and locks BOUND and ESCAPE on every press. A
// thread that held either lock across a plugin call would wait for a main thread that waits for
// it, so neither is ever held across one.
static BOUND: Mutex<Vec<(Shortcut, Action)>> = Mutex::new(Vec::new());
/// Esc, while a delayed capture counts down.
static ESCAPE: Mutex<Option<Shortcut>> = Mutex::new(None);
/// Whether a countdown wants Esc; `settle_escape` brings `ESCAPE` in line with it.
static WANT_ESCAPE: AtomicBool = AtomicBool::new(false);
/// The keys of the take's actions as `apply` was last given them; while the shortcut recorder
/// listens that is none, so the recorder hears them during a take too.
static TAKE_KEYS: Mutex<Vec<(String, Action)>> = Mutex::new(Vec::new());
/// The take's keys registered now, and whether a take wants them.
static TAKE: Mutex<Vec<(Shortcut, Action)>> = Mutex::new(Vec::new());
static WANT_TAKE: AtomicBool = AtomicBool::new(false);
/// Drawing mode's keys registered now, and whether drawing mode wants them.
static INK: Mutex<Vec<(Shortcut, InkKey)>> = Mutex::new(Vec::new());
static WANT_INK: AtomicBool = AtomicBool::new(false);
/// The approval card's keys registered now, and whether a request waits for them.
static APPROVAL: Mutex<Vec<(Shortcut, ApprovalKey)>> = Mutex::new(Vec::new());
static WANT_APPROVAL: AtomicBool = AtomicBool::new(false);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

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
            let ink = lock(&INK).iter().find(|(s, _)| s == shortcut).map(|(_, k)| *k);
            if let Some(key) = ink {
                crate::recording::present::ink_key(app, key);
                return;
            }
            let approval = lock(&APPROVAL).iter().find(|(s, _)| s == shortcut).map(|(_, k)| *k);
            if let Some(key) = approval {
                crate::agents::key(app, key);
                return;
            }
            let find =
                |list: &Mutex<Vec<(Shortcut, Action)>>| lock(list).iter().find(|(s, _)| s == shortcut).map(|(_, a)| *a);
            if let Some(action) = find(&TAKE).or_else(|| find(&BOUND)) {
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
        (Action::IntervalCapture, None) => {
            // Starting reads the window list and makes a folder, which the main thread (where
            // shortcuts arrive) must not wait for.
            let app = app.clone();
            std::thread::spawn(move || session::interval::toggle(&app));
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

/// The action a launch names with `--action <name>` or `--action=<name>`, where the name is the
/// action's in kebab-case. This is how a system shortcut starts a capture where Glimpsfy gets no
/// global shortcuts of its own (Wayland): the desktop's keyboard settings run
/// `glimpsfy --action capture-area`. An unknown name is logged and does nothing.
pub fn action_from_arguments(arguments: &[String]) -> Option<Action> {
    let mut rest = arguments.iter().skip(1);
    let name = loop {
        let argument = rest.next()?;
        if argument == "--action" {
            break rest.next()?.as_str();
        }
        if let Some(name) = argument.strip_prefix("--action=") {
            break name;
        }
    };
    let action = ACTIONS.into_iter().find(|action| action.cli_name() == name);
    if action.is_none() {
        trace::log(format!("[glimpsfy] --action {name}: no such action"));
    }
    action
}

/// How long the first launch's action waits for the windows that were just made to load.
const LAUNCH_ACTION_AFTER: std::time::Duration = std::time::Duration::from_millis(1200);

/// Runs the action this launch named on its command line, once the app is up. A launch while
/// Glimpsfy already runs hands its arguments to the running one instead (`lib.rs`).
pub fn run_launch_action(app: &AppHandle) {
    let arguments: Vec<String> = std::env::args().collect();
    let Some(action) = action_from_arguments(&arguments) else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(LAUNCH_ACTION_AFTER);
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || run(&handle, action, Instant::now()));
    });
}

/// The keys registered now, with their actions, for the dev driver.
#[cfg(debug_assertions)]
pub(crate) fn bound() -> Vec<(String, Action)> {
    BOUND.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(s, a)| (s.into_string(), *a)).collect()
}

/// Every action with the keys Settings › Shortcuts gives it.
fn assigned(shortcuts: &Shortcuts) -> [(Action, &Option<String>); 19] {
    [
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
        (Action::MeasureScreen, &shortcuts.measure_screen),
        (Action::PickColor, &shortcuts.pick_color),
        (Action::DrawOnScreen, &shortcuts.draw_on_screen),
        (Action::Spotlight, &shortcuts.spotlight),
        (Action::IntervalCapture, &shortcuts.interval_capture),
        (Action::ScrollCapture, &shortcuts.scroll_capture),
    ]
}

/// On a Wayland session, where the plugin's keys reach Glimpsfy only while one of its own
/// windows has the focus, every working action is bound through the GlobalShortcuts portal as
/// well, where the desktop has it (`desktop::bind_portal_shortcuts`): named in the language of
/// Settings › General, with the keys of Settings › Shortcuts as the desktop's suggestion. A press
/// runs the action as the plugin's would. Without the portal nothing changes, and
/// `glimpsfy --action <name>` stays the way to bind a key. Once, at start.
#[cfg(target_os = "linux")]
pub fn bind_portal(app: &AppHandle) {
    use tauri::Manager;
    if crate::desktop::describe().capabilities.global_shortcuts {
        return;
    }
    let settings = app.state::<crate::AppState>().settings.get();
    let language = settings.general.language;
    let wanted = assigned(&settings.shortcuts)
        .into_iter()
        .filter(|(action, _)| action.works())
        .map(|(action, accelerator)| {
            let name =
                serde_json::to_value(action).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            crate::desktop::PortalShortcut {
                id: action.cli_name().to_string(),
                description: crate::i18n::text(language, &format!("settings.shortcuts.action.{name}"), &[]),
                trigger: accelerator.as_deref().and_then(crate::desktop::portal_trigger),
            }
        })
        .collect();
    let app = app.clone();
    crate::desktop::bind_portal_shortcuts(wanted, move |id| {
        let Some(action) = ACTIONS.into_iter().find(|action| action.cli_name() == id) else {
            return;
        };
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || run(&handle, action, Instant::now()));
    });
}

/// Registers the working actions' keys, replacing whatever was registered before.
pub fn apply(app: &AppHandle, shortcuts: &Shortcuts) -> Vec<ShortcutProblem> {
    let manager = app.global_shortcut();
    let previous = std::mem::take(&mut *BOUND.lock().unwrap_or_else(|p| p.into_inner()));
    for (shortcut, _) in previous {
        let _ = manager.unregister(shortcut);
    }
    let mut bound = Vec::new();

    let wanted = assigned(shortcuts);

    let mut problems = Vec::new();
    let mut take_keys = Vec::new();
    for (action, accelerator) in wanted {
        let Some(accelerator) = accelerator.as_deref().filter(|a| !a.is_empty()) else {
            continue;
        };
        if !action.works() {
            continue;
        }
        if action.take_only() {
            take_keys.push((accelerator.to_string(), action));
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
    // Added to rather than replaced, so a key registered here is always one a later apply
    // unregisters.
    BOUND.lock().unwrap_or_else(|p| p.into_inner()).extend(bound);
    for problem in &problems {
        trace::log(format!(
            "[glimpsfy] shortcut {} for {:?}: {}",
            problem.accelerator, problem.action, problem.message
        ));
        let _ = app.emit(events::SHORTCUT_FAILED, problem.clone());
    }
    // A take that runs now registers its keys again, with the new ones.
    *lock(&TAKE_KEYS) = take_keys;
    settle(app, &TAKE, take_wanted, take_refused);
    problems
}

/// Registers the take's keys (Draw on screen, Spotlight) when a take goes live.
pub fn bind_take(app: &AppHandle) {
    WANT_TAKE.store(true, Ordering::SeqCst);
    settle(app, &TAKE, take_wanted, take_refused);
}

/// Gives the take's keys back to the other apps when the take ends.
pub fn unbind_take(app: &AppHandle) {
    WANT_TAKE.store(false, Ordering::SeqCst);
    settle(app, &TAKE, take_wanted, take_refused);
}

/// Registers drawing mode's keys while it is on.
pub fn bind_ink(app: &AppHandle) {
    WANT_INK.store(true, Ordering::SeqCst);
    settle(app, &INK, ink_wanted, ink_refused);
}

pub fn unbind_ink(app: &AppHandle) {
    WANT_INK.store(false, Ordering::SeqCst);
    settle(app, &INK, ink_wanted, ink_refused);
}

/// Registers the approval card's keys (⌥⇧⌘Y N T ← →) while a request waits; the card never takes
/// the keyboard, so they reach Glimpsfy as global keys and are given back when the queue empties.
pub fn bind_approval(app: &AppHandle) {
    if !WANT_APPROVAL.swap(true, Ordering::SeqCst) {
        settle(app, &APPROVAL, approval_wanted, approval_refused);
    }
}

pub fn unbind_approval(app: &AppHandle) {
    if WANT_APPROVAL.swap(false, Ordering::SeqCst) {
        settle(app, &APPROVAL, approval_wanted, approval_refused);
    }
}

/// The approval keys registered now; a key another app or one of the user's own shortcuts holds
/// is missing, and the card shows no key cap for it.
pub fn approval_keys() -> Vec<ApprovalKey> {
    lock(&APPROVAL).iter().map(|(_, key)| *key).collect()
}

fn approval_wanted() -> Vec<(String, ApprovalKey)> {
    if WANT_APPROVAL.load(Ordering::SeqCst) {
        crate::agents::APPROVAL_KEYS.iter().map(|(accelerator, key)| (accelerator.to_string(), *key)).collect()
    } else {
        Vec::new()
    }
}

/// A held approval key costs only its key cap on the card; the buttons still answer.
fn approval_refused(_: &AppHandle, accelerator: &str, key: ApprovalKey, message: String) {
    trace::log(format!("[glimpsfy] approval key {accelerator} ({key:?}): {message}"));
}

fn take_wanted() -> Vec<(String, Action)> {
    if WANT_TAKE.load(Ordering::SeqCst) {
        lock(&TAKE_KEYS).clone()
    } else {
        Vec::new()
    }
}

/// A take's key another app holds costs only that key, and Settings › Shortcuts says so.
fn take_refused(app: &AppHandle, accelerator: &str, action: Action, message: String) {
    trace::log(format!("[glimpsfy] shortcut {accelerator} for {action:?}: {message}"));
    let problem = ShortcutProblem { action, accelerator: accelerator.to_string(), message };
    let _ = app.emit(events::SHORTCUT_FAILED, problem);
}

fn ink_wanted() -> Vec<(String, InkKey)> {
    if WANT_INK.load(Ordering::SeqCst) {
        INK_KEYS.iter().map(|(accelerator, key)| (accelerator.to_string(), *key)).collect()
    } else {
        Vec::new()
    }
}

/// Esc is the one drawing key another holder may have (a delayed capture's countdown, which
/// cancels with it); drawing mode then ends with ✓ or ⌥⇧⌘D instead.
fn ink_refused(_: &AppHandle, accelerator: &str, key: InkKey, message: String) {
    trace::log(format!("[glimpsfy] drawing key {accelerator} ({key:?}): {message}"));
}

/// Replaces what `slot` holds with the keys `wanted` reads at that moment, on the main thread
/// without waiting, as `settle_escape` does and for the same reason. Each task reads the latest
/// wish, so the keys end up as the last call asked even when a bind and an unbind cross.
fn settle<T: Copy + Send + 'static>(
    app: &AppHandle,
    slot: &'static Mutex<Vec<(Shortcut, T)>>,
    wanted: fn() -> Vec<(String, T)>,
    refused: fn(&AppHandle, &str, T, String),
) {
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        let manager = handle.global_shortcut();
        let previous = std::mem::take(&mut *lock(slot));
        for (shortcut, _) in previous {
            let _ = manager.unregister(shortcut);
        }
        let mut held = Vec::new();
        for (accelerator, item) in wanted() {
            let registered = Shortcut::from_str(&accelerator)
                .map_err(|error| error.to_string())
                .and_then(|shortcut| manager.register(shortcut).map(|()| shortcut).map_err(|error| error.to_string()));
            match registered {
                Ok(shortcut) => held.push((shortcut, item)),
                Err(message) => refused(&handle, &accelerator, item, message),
            }
        }
        lock(slot).extend(held);
    });
    if let Err(error) = queued {
        trace::log(format!("[glimpsfy] temporary shortcuts: {error}"));
    }
}

/// Registers Esc for a countdown; a key Glimpsfy cannot take only costs the shortcut.
pub fn bind_escape(app: &AppHandle) {
    WANT_ESCAPE.store(true, Ordering::SeqCst);
    settle_escape(app);
}

/// Gives Esc back to the other apps.
pub fn unbind_escape(app: &AppHandle) {
    WANT_ESCAPE.store(false, Ordering::SeqCst);
    settle_escape(app);
}

/// Registers or unregisters Esc on the main thread, without waiting for it. The countdown's
/// thread binds and unbinds while a key press can be running the handler there, and waiting
/// would freeze both. The main thread runs the tasks one after another and each reads the latest
/// wish, so Esc ends up as the last call asked even when a bind and an unbind cross.
fn settle_escape(app: &AppHandle) {
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        let wanted = WANT_ESCAPE.load(Ordering::SeqCst);
        let held = *ESCAPE.lock().unwrap_or_else(|p| p.into_inner());
        match (wanted, held) {
            (true, None) => match Shortcut::from_str("Escape") {
                Ok(shortcut) => match handle.global_shortcut().register(shortcut) {
                    Ok(()) => *ESCAPE.lock().unwrap_or_else(|p| p.into_inner()) = Some(shortcut),
                    Err(error) => trace::log(format!("[glimpsfy] Esc for the countdown: {error}")),
                },
                Err(error) => trace::log(format!("[glimpsfy] Esc for the countdown: {error}")),
            },
            (false, Some(shortcut)) => {
                *ESCAPE.lock().unwrap_or_else(|p| p.into_inner()) = None;
                let _ = handle.global_shortcut().unregister(shortcut);
            }
            _ => {}
        }
    });
    if let Err(error) = queued {
        trace::log(format!("[glimpsfy] Esc for the countdown: {error}"));
    }
}
