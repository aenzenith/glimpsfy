//! Send to app and Copy path commands. Owner: output (A2). The work is `send.rs`; these run it off
//! the main thread, except the list of targets, which asks AppKit for the apps' icons there.
//!
//! Copy path puts a capture's file path on the clipboard as text, quoted for a shell, everywhere.
//! Send to app puts the capture on the clipboard the way the target takes it (`SendPaste`),
//! brings the app forward, starting it when it is not running, and presses ⌘V in it; it never
//! presses Return. Send to app is macOS only for now (`Capabilities::send_to_app`).

use glimpsfy_history::EntryId;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use super::capture::{CaptureId, Selection};
use super::error::{CommandError, CommandResult};
use crate::settings::SendPaste;

/// Event names of the send area and their payloads. The TypeScript copy is in
/// `src/lib/api/send.ts`.
pub mod events {
    use serde::Serialize;

    use super::SendPhase;

    /// A send moved on: the target is opening, the paste happened, or it could not. Payload:
    /// [`SendState`]. The toast of the send follows it.
    pub const STATE: &str = "send://state";

    #[derive(Clone, Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SendState {
        pub send_id: u64,
        pub target_name: String,
        pub phase: SendPhase,
    }
}

/// How far a send got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SendPhase {
    /// The app was not running and is starting; the paste waits for its window, 10 seconds at
    /// most.
    Opening,
    /// ⌘V was pressed in the app. Return was not: sending the message is the user's.
    Pasted,
    /// The app is in front and the clipboard holds the capture, but Glimpsfy may not press keys
    /// (Accessibility), so the user presses ⌘V.
    NoPermission,
    /// The app showed no window within 10 seconds; nothing was pasted, and the clipboard keeps
    /// the capture.
    TimedOut,
    /// The user stopped the wait for the app (`send_cancel`).
    Cancelled,
    /// The app could not be brought forward or started.
    Failed,
}

/// What is sent: an open capture's selection (the overlay), a history entry (the popover, the
/// history), or a capture a toast reports, which the core remembers for a while.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SendSource {
    Selection { selection: Selection },
    Entry { entry_id: EntryId },
    Capture { capture_id: CaptureId },
}

/// What a `Sent` toast carries.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentToast {
    pub send_id: u64,
    /// The target's bundle identifier, for Try again.
    pub target_id: String,
    pub target_name: String,
    pub phase: SendPhase,
    /// What Try again sends: the capture or the entry the send went out from.
    pub retry: Option<SendSource>,
    /// The entry the send went out from, whose thumbnail the toast shows.
    pub entry_id: Option<EntryId>,
}

/// One target of the send list, with what the system says about it now.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendTargetInfo {
    pub bundle_id: String,
    /// The installed app's own name, or the name it had when it was added.
    pub name: String,
    pub paste: SendPaste,
    /// The app is on this Mac; a missing one shows dimmed and cannot be picked.
    pub installed: bool,
    /// The app runs now; one that does not is started first.
    pub running: bool,
    /// The app's icon as a PNG data URL, 64 points.
    pub icon: Option<String>,
}

/// What `send_list_targets` answers.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendTargets {
    /// The targets of Settings › Output, in their order.
    pub targets: Vec<SendTargetInfo>,
    /// The target sent to last since Glimpsfy started; the list opens on it.
    pub last: Option<String>,
    /// Glimpsfy may press ⌘V (Accessibility); without it a send stops after bringing the app
    /// forward.
    pub can_paste: bool,
}

/// The app `send_pick_app` picked.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedApp {
    pub bundle_id: String,
    pub name: String,
    pub icon: Option<String>,
    /// What the app should get: a path for a terminal, which cannot paste an image, an image
    /// otherwise.
    pub paste: SendPaste,
}

/// What a send or a Copy path answers.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sent {
    /// The send, whose progress arrives as `send://state`; `None` for Copy path.
    pub send_id: Option<u64>,
    /// The copied path, unquoted; `None` for a send.
    pub path: Option<String>,
    /// The copied path points into the inbox, where the file is deleted after a day.
    pub temporary: bool,
}

async fn off_main<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| CommandError::internal(e.to_string()))?
}

/// The targets of the send list, in the order of Settings › Output, with their icons and whether
/// each is installed and running. Runs on the main thread, where AppKit draws the icons.
#[tauri::command]
pub fn send_list_targets(app: AppHandle) -> SendTargets {
    crate::send::list_targets(&app)
}

/// Lets the user pick an app (`.app`) and answers its name, bundle identifier and icon, or `null`
/// when they cancel. The page adds it to Settings › Output itself. `not_implemented` outside macOS.
#[tauri::command]
pub async fn send_pick_app(app: AppHandle) -> CommandResult<Option<PickedApp>> {
    off_main(move || crate::send::pick_app(&app)).await
}

/// Puts the path of the source's file on the clipboard as text, quoted for a shell when it needs
/// quotes, and shows a toast. A selection closes its capture; while the history is off its file is
/// written into the inbox, where it is deleted after a day. `invalid_argument` for a text copy,
/// which has no file, and `not_found` for a file that is gone.
#[tauri::command]
pub async fn send_copy_path(app: AppHandle, source: SendSource) -> CommandResult<Sent> {
    off_main(move || crate::send::copy_path(&app, source)).await
}

/// Sends the source to the target of Settings › Output whose bundle identifier is `target_id`:
/// the clipboard gets the image, the path or the file, and the app comes forward and gets ⌘V. It
/// answers once the clipboard is ready; the rest arrives as `send://state`. `not_found` for a
/// target that is not in the settings or not on this Mac (a selection then stays open), and
/// `not_implemented` outside macOS.
#[tauri::command]
pub async fn send_to_app(app: AppHandle, source: SendSource, target_id: String) -> CommandResult<Sent> {
    off_main(move || crate::send::send(&app, source, &target_id)).await
}

/// Stops waiting for the app of a send that is still opening; nothing is pasted.
#[tauri::command]
pub fn send_cancel(send_id: u64) {
    crate::send::cancel(send_id);
}
