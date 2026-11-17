//! Recording commands: the setup surface, one take from the countdown to the file, the devices
//! and their meters, the webcam bubble, the canvas, and the trim window's exports. Owner:
//! recording (milestones 5 and 6). The state and the windows live in `crate::recording`; these
//! wrappers carry the pages' arguments to it.
//!
//! One take runs at a time; `record_start` fails with `busy` while another one runs. Opening the
//! stream, finishing a file and exporting block, so those run on worker threads.

use glimpsfy_record::{
    ExportOptions, ExportResult, RecordError, RecordingConfig, RecordingDevices, RecordingId, RecordingResult,
    RecordingState,
};
use tauri::AppHandle;

use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::recording::media::{self, RecordingInfo, StartFormat, TrimSource};
use crate::recording::setup::{BarLayout, RecordSetup, SetupRequest, SetupUpdate, Size};
use crate::recording::{self, BubbleChange, BubbleState, CanvasConfig};

impl From<RecordError> for CommandError {
    fn from(error: RecordError) -> Self {
        // A capture failure keeps the capture area's own code.
        let error = match error {
            RecordError::Capture(capture) => return capture.into(),
            other => other,
        };
        let code = match &error {
            RecordError::NotImplemented(_) => ErrorCode::NotImplemented,
            RecordError::Busy => ErrorCode::Busy,
            RecordError::NotRecording | RecordError::NotFound(_) | RecordError::DeviceNotFound(_) => {
                ErrorCode::NotFound
            }
            RecordError::InvalidState { .. } | RecordError::InvalidConfig(_) => ErrorCode::InvalidArgument,
            RecordError::PermissionDenied(_) => ErrorCode::PermissionDenied,
            RecordError::Cancelled => ErrorCode::Cancelled,
            RecordError::Capture(_) => ErrorCode::Capture,
            RecordError::Encoder(_) => ErrorCode::Internal,
            RecordError::DiskFull { .. } => ErrorCode::Io,
            RecordError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            RecordError::Io(_) => ErrorCode::Io,
        };
        CommandError::new(code, error.to_string())
    }
}

/// Event names of the recording area and their payloads. `src/lib/api/record.ts` holds the same
/// names.
pub mod events {
    use glimpsfy_record::RecordingId;
    use serde::Serialize;

    /// Four times a second while a take records or pauses. Payload: [`RecordTick`].
    pub const TICK: &str = "record://tick";
    /// The recorder moved to another state. Payload: `glimpsfy_record::RecordingState`.
    pub const STATE: &str = "record://state";
    /// Once per take, after the state returned to idle. Payload: `glimpsfy_record::RecordingOutcome`
    /// with a failure's `code` as one of the UI's `ErrorCode`s.
    pub const FINISHED: &str = "record://finished";
    /// An export moved forward. Payload: `crate::recording::media::ExportProgress`.
    pub const EXPORT_PROGRESS: &str = "record://export-progress";
    /// The setup surface changed or closed. Payload: `RecordSetup` or `null`.
    pub const SETUP: &str = "record://setup";
    /// Live input levels before a take. Payload: `{ microphones: {id: level}, system: level|null }`.
    pub const LEVELS: &str = "record://levels";
    /// The webcam bubble moved, changed or went. Payload: `BubbleState` or `null`.
    pub const BUBBLE: &str = "record://bubble";
    /// To `rec-canvas` only: what it draws. Payload: `CanvasConfig` or `null`.
    pub const CANVAS: &str = "record://canvas";
    /// To `rec-canvas` only: a click in the recorded rectangle. Payload: a global `LogicalPoint`.
    pub const CLICK: &str = "record://click";
    /// To `rec-canvas` only: a key press as caps. Payload: `{ caps: string[] }`.
    pub const KEYS: &str = "record://keys";
    /// To `rec-canvas` only, with the keystroke overlay on: the pointer, global, or `null` when it
    /// left the recorded rectangle.
    pub const POINTER: &str = "record://pointer";
    /// To the trim window only: the recording it shows. Payload: `RecordingInfo`.
    pub const TRIM: &str = "record://trim";

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct RecordTick {
        pub id: RecordingId,
        /// The recorded time without pauses.
        pub elapsed_ms: u64,
        /// The microphone's peak since the last tick, from 0 to 1, when it is recorded and not
        /// muted.
        pub microphone_level: Option<f32>,
        /// The system audio's peak since the last tick, when it is recorded.
        pub system_level: Option<f32>,
        pub file_bytes: u64,
    }
}

// ------------------------------------------------------------------------------ the setup

/// Opens the setup surface (the target and the setup bar), or retargets it when it is open.
#[tauri::command]
pub async fn record_setup(app: AppHandle, request: SetupRequest) -> CommandResult<RecordSetup> {
    recording::open_setup(&app, request)
}

#[tauri::command]
pub fn record_setup_state(app: AppHandle) -> Option<RecordSetup> {
    recording::setup_state(&app)
}

#[tauri::command]
pub async fn record_setup_update(app: AppHandle, update: SetupUpdate) -> CommandResult<RecordSetup> {
    recording::update_setup(&app, update)
}

/// Close (Esc) on the setup bar.
#[tauri::command]
pub fn record_setup_close(app: AppHandle) {
    recording::close_setup(&app);
}

/// The bar page measured its bar and its open popover; the window is placed for them.
#[tauri::command]
pub fn record_bar_fit(app: AppHandle, bar: Size, popover: Option<Size>) -> CommandResult<BarLayout> {
    recording::fit_bar(&app, bar, popover)
}

// ------------------------------------------------------------------------------- the take

/// Starts the countdown and then the recording, and resolves at once with the take's id.
/// Without a configuration it records the open setup with Settings › Recording.
#[tauri::command]
pub async fn record_start(app: AppHandle, config: Option<RecordingConfig>) -> CommandResult<RecordingId> {
    recording::start(app, config).await
}

/// The recorder's state, for a page that opens or reloads during a take.
#[tauri::command]
pub fn record_state(app: AppHandle) -> RecordingState {
    recording::recorder_state(&app)
}

#[tauri::command]
pub fn record_countdown_skip(app: AppHandle) -> CommandResult<()> {
    recording::skip_countdown(&app)
}

#[tauri::command]
pub async fn record_countdown_cancel(app: AppHandle) -> CommandResult<()> {
    recording::cancel_countdown(app).await
}

#[tauri::command]
pub fn record_pause(app: AppHandle) -> CommandResult<()> {
    recording::pause(&app)
}

#[tauri::command]
pub fn record_resume(app: AppHandle) -> CommandResult<()> {
    recording::resume(&app)
}

/// Stops the take and resolves when the file is complete.
#[tauri::command]
pub async fn record_stop(app: AppHandle) -> CommandResult<RecordingResult> {
    recording::stop(app).await
}

/// Without `confirmed`, pauses the take and asks in the control bar; with it, moves the take to
/// the Trash. During the countdown it cancels.
#[tauri::command]
pub async fn record_discard(app: AppHandle, confirmed: Option<bool>) -> CommandResult<&'static str> {
    recording::discard(app, confirmed.unwrap_or(false)).await
}

#[tauri::command]
pub fn record_keep(app: AppHandle) -> CommandResult<()> {
    recording::keep(&app)
}

#[tauri::command]
pub fn record_set_microphone_muted(app: AppHandle, muted: bool) -> CommandResult<()> {
    recording::set_muted(&app, muted)
}

// ---------------------------------------------------------------------- devices and meters

/// The microphones and cameras connected now, and whether system audio can be recorded.
#[tauri::command]
pub async fn record_devices() -> CommandResult<RecordingDevices> {
    // Listing devices asks AVFoundation, which can take a moment.
    tauri::async_runtime::spawn_blocking(glimpsfy_record::devices::list)
        .await
        .map_err(|error| CommandError::internal(error.to_string()))
}

/// Meters these microphones with `record://levels`; `null` stops.
#[tauri::command]
pub fn record_meter(app: AppHandle, microphones: Option<Vec<String>>, system: Option<bool>) {
    recording::meter(&app, microphones, system.unwrap_or(false));
}

// ------------------------------------------------------------------- bubble and canvas

#[tauri::command]
pub fn record_bubble_state(app: AppHandle) -> Option<BubbleState> {
    recording::bubble_state(&app)
}

#[tauri::command]
pub fn record_bubble_set(app: AppHandle, change: BubbleChange) {
    recording::set_bubble(&app, change);
}

#[tauri::command]
pub fn record_canvas_state(app: AppHandle) -> Option<CanvasConfig> {
    recording::canvas_state(&app)
}

// ------------------------------------------------------------------- trim and export

/// Opens the trim window for a finished take or a history entry.
#[tauri::command]
pub async fn record_open_trim(
    app: AppHandle,
    source: TrimSource,
    format: Option<StartFormat>,
) -> CommandResult<RecordingInfo> {
    // A history entry is read from disk first.
    tauri::async_runtime::spawn_blocking(move || media::open_trim(&app, source, format.unwrap_or_default()))
        .await
        .map_err(|error| CommandError::internal(error.to_string()))?
}

#[tauri::command]
pub fn record_trim_current(app: AppHandle) -> Option<RecordingInfo> {
    media::current(&app)
}

#[tauri::command]
pub fn record_info(app: AppHandle, recording_id: RecordingId) -> CommandResult<RecordingInfo> {
    media::recording_info(&app, &recording_id)
}

/// Writes an MP4 or a GIF from a recording, with progress as `record://export-progress`.
#[tauri::command]
pub async fn record_export(
    app: AppHandle,
    recording_id: RecordingId,
    options: ExportOptions,
) -> CommandResult<ExportResult> {
    media::export(app, recording_id, options).await
}

#[tauri::command]
pub fn record_export_cancel(app: AppHandle, recording_id: RecordingId) {
    media::cancel_export(&app, &recording_id);
}

/// Moves a finished take to the Trash (the trim window's Discard, after its own question).
#[tauri::command]
pub async fn record_trash(app: AppHandle, recording_id: RecordingId) -> CommandResult<()> {
    tauri::async_runtime::spawn_blocking(move || media::trash(&app, &recording_id))
        .await
        .map_err(|error| CommandError::internal(error.to_string()))?
}
