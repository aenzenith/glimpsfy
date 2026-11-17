//! The delayed capture: a one-shot delay for the next capture (the menu's Delay › 3, 5 or 10
//! seconds, or `capture_set_delay`), and the countdown that runs before "Capture area after a
//! delay" and before any capture while a delay is set.
//!
//! The delay is used by the next capture and turns itself off. While a countdown runs, the menu
//! bar icon shows the seconds left, Esc (registered globally for the countdown only) and the
//! menu's "Cancel delayed capture" stop it, every window hears `capture://countdown`, and other
//! capture triggers are ignored.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::commands::capture::{events::COUNTDOWN, CaptureMode, DelayState};
use crate::commands::error::{CommandError, CommandResult};
use crate::{shortcuts, trace, tray, windows};

/// The delays the menu offers; 0 is off.
pub const CHOICES: [u32; 4] = [0, 3, 5, 10];

/// The one-shot delay of the next capture, in seconds.
static PENDING: AtomicU32 = AtomicU32::new(0);
/// The countdown that runs now.
static RUNNING: Mutex<Option<Countdown>> = Mutex::new(None);
/// Each countdown's number; a cancelled one finds the number changed and stops.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
struct Countdown {
    generation: u64,
    mode: CaptureMode,
    remaining: u32,
}

fn running() -> Option<Countdown> {
    *RUNNING.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn is_running() -> bool {
    running().is_some()
}

/// The delay that is set and the countdown that runs.
pub fn state(_app: &AppHandle) -> DelayState {
    let run = running();
    DelayState {
        pending_seconds: PENDING.load(Ordering::SeqCst),
        remaining_seconds: run.map(|r| r.remaining),
        mode: run.map(|r| r.mode),
    }
}

/// The delay that is set, for the menu's check marks.
pub fn pending() -> u32 {
    PENDING.load(Ordering::SeqCst)
}

/// Sets the one-shot delay of the next capture.
pub fn set_pending(app: &AppHandle, seconds: u32) -> CommandResult<DelayState> {
    if !CHOICES.contains(&seconds) {
        return Err(CommandError::invalid("a delay is 0 (off), 3, 5 or 10 seconds"));
    }
    PENDING.store(seconds, Ordering::SeqCst);
    announce(app, true);
    Ok(state(app))
}

/// Takes the one-shot delay for the capture that starts now, turning it off.
pub fn take_pending() -> u32 {
    PENDING.swap(0, Ordering::SeqCst)
}

/// Counts `seconds` down and then starts `mode`.
pub fn begin(app: &AppHandle, mode: CaptureMode, seconds: u32) -> CommandResult<()> {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = Some(Countdown { generation, mode, remaining: seconds });
    shortcuts::bind_escape(app);
    announce(app, true);

    let handle = app.clone();
    let spawned = thread::Builder::new().name("glimpsfy-delay".into()).spawn(move || {
        let app = handle;
        for left in (0..seconds).rev() {
            thread::sleep(Duration::from_secs(1));
            {
                let mut run = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
                match run.as_mut() {
                    Some(countdown) if countdown.generation == generation => countdown.remaining = left,
                    _ => return,
                }
                if left == 0 {
                    *run = None;
                }
            }
            announce(&app, left == 0);
        }
        shortcuts::unbind_escape(&app);
        // The icon's title is gone before the frame is taken.
        thread::sleep(Duration::from_millis(60));
        if let Err(error) = super::start_now(&app, mode, Instant::now()) {
            windows::toast_failure(&app, None, &error);
        }
    });
    if let Err(error) = spawned {
        *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = None;
        shortcuts::unbind_escape(app);
        announce(app, true);
        return Err(CommandError::internal(error.to_string()));
    }
    Ok(())
}

/// Stops the running countdown without capturing. Answers whether one was running.
pub fn cancel(app: &AppHandle) -> bool {
    let was = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).take().is_some();
    if was {
        trace::log("[glimpsfy] delayed capture cancelled");
        shortcuts::unbind_escape(app);
        announce(app, true);
    }
    was
}

/// Tells the pages, the icon's title and (when `menu`) the menu.
fn announce(app: &AppHandle, menu: bool) {
    let state = state(app);
    let _ = app.emit(COUNTDOWN, state);
    tray::show_countdown(app, state.remaining_seconds);
    if menu {
        tray::refresh(app);
    }
}
