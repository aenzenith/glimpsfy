//! Screen recording: what to record, the recorder's states, the take's timeline and audio, the
//! MP4 writer, and MP4 and GIF export.
//!
//! - [`state`] is the recorder's state machine: pure, every transition validated.
//! - [`session`] runs one take on top of it: it opens the capture stream (`glimpsfy-capture`),
//!   maps every buffer onto the file's timeline ([`timeline`]: pauses leave no gap), places and
//!   mixes the audio ([`audio`]) and hands everything to a [`session::TakeWriter`]. Its platform
//!   parts (the stream, the writer, the Trash, the clock) are traits, so the policy can be tested
//!   without a screen.
//! - [`files`] names files, never replaces one and checks free space; [`export`] writes MP4 and
//!   GIF files from a finished take ([`gif`] is the pure-Rust GIF encoder).
//! - [`platform`] holds the system's side: `AVAssetWriter`, `AVAssetReader`, the devices, the
//!   microphone fallback and the Trash on macOS. Other platforms answer `NotImplemented`.
//!
//! # Threads
//!
//! Starting, stopping and exporting block (the stream starts, the file is finished, frames are
//! encoded), so callers run them on a worker thread. Video and audio buffers arrive on the
//! capture backend's threads, stamped from one host clock, so audio and video stay in step in
//! long recordings.

use std::path::PathBuf;

use glimpsfy_capture::{CaptureError, MicrophoneSource, PhysicalSize, StreamTarget};
use serde::{Deserialize, Serialize};

pub mod audio;
pub mod devices;
pub mod export;
pub mod files;
pub mod gif;
pub mod platform;
pub mod session;
pub mod state;
pub mod timeline;

pub type Result<T, E = RecordError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
    /// A recording is already running, and only one runs at a time.
    #[error("a recording is already running")]
    Busy,
    /// The call needs a running or paused recording, and there is none.
    #[error("no recording is running")]
    NotRecording,
    /// The call does not fit the recorder's state (resume while recording, a discard that was
    /// not asked first).
    #[error("the recorder cannot {action} while it is {state}")]
    InvalidState { action: &'static str, state: &'static str },
    #[error("the {0:?} permission is not granted")]
    PermissionDenied(Permission),
    /// The microphone or camera in the configuration is no longer connected.
    #[error("the device {0} is not connected")]
    DeviceNotFound(String),
    #[error("the recording configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("there is no recording {0:?}")]
    NotFound(RecordingId),
    #[error("the recording was cancelled")]
    Cancelled,
    /// The disk has too little free space to start or to go on.
    #[error("the disk is almost full ({free_bytes} bytes free at {path})")]
    DiskFull { path: PathBuf, free_bytes: u64 },
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// The system's encoder or the GIF encoder failed; the text is its own reason.
    #[error("encoding failed: {0}")]
    Encoder(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The system permissions a recording can need.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    ScreenRecording,
    Microphone,
    Camera,
}

/// A recording's identity, which the history entry of the recording keeps.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecordingId(pub String);

impl RecordingId {
    /// A new id from the time and a counter, so two takes never share one.
    pub fn new() -> RecordingId {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        RecordingId(format!("rec-{ms:x}-{}", NEXT.fetch_add(1, Ordering::SeqCst)))
    }

    /// Whether `text` could be an id this crate made (letters, digits and dashes), so a URL can
    /// never smuggle a path through it.
    pub fn is_well_formed(text: &str) -> bool {
        !text.is_empty() && text.len() <= 64 && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
}

impl Default for RecordingId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VideoCodec {
    /// Plays everywhere.
    #[default]
    H264,
    /// About half the size of H.264, and a few older apps cannot play it.
    Hevc,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    /// Keeps the target's pixel size.
    #[default]
    Native,
    /// Halves the width and the height, which keeps long recordings small.
    Half,
}

/// The audio a recording carries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioConfig {
    /// Records what the system plays.
    pub system: bool,
    /// Records this microphone, or none when `None`.
    pub microphone: Option<MicrophoneSource>,
    /// Writes each source to a track of its own and adds a mixed track as well, so the file
    /// still plays everywhere.
    pub separate_tracks: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RingColor {
    #[default]
    Yellow,
    Red,
    Blue,
    White,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RingSize {
    #[default]
    Small,
    Large,
}

/// Draws a ring around each mouse click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClickHighlight {
    pub enabled: bool,
    pub color: RingColor,
    pub size: RingSize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverlaySize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeystrokePosition {
    #[default]
    BottomCenter,
    BottomLeft,
    BottomRight,
    TopCenter,
}

/// The bar that shows pressed keys on the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeystrokeOverlay {
    pub enabled: bool,
    pub position: KeystrokePosition,
    pub size: OverlaySize,
    /// Shows only keys pressed with ⌘, ⌥ or ⌃, so typed text and passwords stay out of the video.
    pub only_shortcuts: bool,
}

impl Default for KeystrokeOverlay {
    fn default() -> Self {
        KeystrokeOverlay {
            enabled: false,
            position: KeystrokePosition::default(),
            size: OverlaySize::default(),
            only_shortcuts: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WebcamShape {
    #[default]
    Circle,
    RoundedSquare,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Corner {
    #[default]
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

/// Draws the webcam bubble into the recording.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebcamOverlay {
    pub enabled: bool,
    /// Names the camera by its system identifier; `None` picks the default camera.
    pub device_id: Option<String>,
    pub shape: WebcamShape,
    pub size: OverlaySize,
    pub corner: Corner,
}

/// Everything one recording needs. The core builds it from Settings › Recording and the choices
/// of the setup bar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingConfig {
    pub target: StreamTarget,
    /// Sets the frames per second, which are 30 or 60.
    pub fps: u32,
    pub resolution: Resolution,
    pub codec: VideoCodec,
    pub shows_cursor: bool,
    pub audio: AudioConfig,
    pub clicks: ClickHighlight,
    pub keystrokes: KeystrokeOverlay,
    pub webcam: WebcamOverlay,
    /// The countdown before the recording starts, in seconds; 0 starts at once.
    pub countdown_seconds: u32,
    /// The folder the MP4 is written to (Settings › Recording › Recordings folder).
    pub output_folder: PathBuf,
}

impl RecordingConfig {
    /// Refuses what no recording can do: a frame rate other than 30 or 60, a countdown longer
    /// than ten seconds, an empty area, or no folder.
    pub fn validate(&self) -> Result<()> {
        if self.fps != 30 && self.fps != 60 {
            return Err(RecordError::InvalidConfig(format!("the frame rate must be 30 or 60, not {}", self.fps)));
        }
        if self.countdown_seconds > 10 {
            return Err(RecordError::InvalidConfig("the countdown is longer than ten seconds".into()));
        }
        if let StreamTarget::Area { rect, .. } = &self.target {
            if !(rect.width >= 2.0 && rect.height >= 2.0) {
                return Err(RecordError::InvalidConfig("the area is empty".into()));
            }
        }
        if self.output_folder.as_os_str().is_empty() {
            return Err(RecordError::InvalidConfig("no recordings folder is set".into()));
        }
        Ok(())
    }
}

/// Where the recorder is. It is also the payload of the `record://state` event.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RecordingState {
    #[default]
    Idle,
    /// The countdown runs, and nothing is recorded yet.
    Countdown { id: RecordingId, remaining_seconds: u32, total_seconds: u32 },
    /// `microphone` says whether the take records a microphone at all.
    Recording { id: RecordingId, elapsed_ms: u64, microphone: bool, microphone_muted: bool },
    /// Paused time is left out of the file, so `elapsed_ms` stops while paused. With
    /// `discard_asked`, the control bar asks whether to discard the take.
    Paused { id: RecordingId, elapsed_ms: u64, microphone: bool, microphone_muted: bool, discard_asked: bool },
    /// The stopping step: the file is being finished (or moved to the Trash).
    Finishing { id: RecordingId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    Video,
    SystemAudio,
    Microphone,
    /// Mixes system audio and the microphone, and is added when the sources have their own tracks.
    MixedAudio,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    pub kind: TrackKind,
    /// The track's codec as the container names it, such as `avc1`, `hvc1` or `aac`.
    pub codec: String,
}

/// Why a take stopped on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// The disk ran low, so the take was finished while there was room to finish it.
    DiskFull,
}

/// Describes a finished recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingResult {
    pub id: RecordingId,
    pub path: PathBuf,
    /// Gives the length of the video, without paused time.
    pub duration_ms: u64,
    pub size: PhysicalSize,
    pub file_bytes: u64,
    pub tracks: Vec<TrackInfo>,
    pub codec: VideoCodec,
    pub fps: u32,
    pub created_at_ms: i64,
    pub stopped_because: Option<StopReason>,
}

/// How a take ended: the payload of `record://finished`, sent once per take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RecordingOutcome {
    /// The take is complete on disk.
    Saved { result: RecordingResult },
    /// The user confirmed the discard: the file is in the Trash (`trashed`), or nothing had been
    /// written yet.
    Discarded { id: RecordingId, trashed: bool },
    /// The take failed; what was written stays on disk as `partial`. `code` is one of the app's
    /// error codes (`capture`, `permission_denied`, `not_found`, `io`, `internal`), which the UI
    /// translates.
    Failed { id: RecordingId, code: String, message: String, partial: Option<RecordingResult> },
    /// The countdown was cancelled before anything was recorded.
    Cancelled { id: RecordingId },
}

/// The part of a recording to export. `end_ms` is exclusive, and both are measured from the start
/// of the video.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrimRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mp4Quality {
    #[default]
    High,
    Medium,
    Low,
}

/// The file an export writes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ExportFormat {
    Mp4 {
        resolution: Resolution,
        quality: Mp4Quality,
    },
    Gif {
        /// Sets the frames per second, from 1 to 50.
        fps: u32,
        /// Sets the width in pixels and keeps the aspect ratio; `None` keeps the recording's width.
        width: Option<u32>,
        /// Plays the GIF in a loop instead of once.
        loop_forever: bool,
    },
}

/// The audio sources an MP4 export keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportAudio {
    pub microphone: bool,
    pub system: bool,
}

impl Default for ExportAudio {
    fn default() -> Self {
        ExportAudio { microphone: true, system: true }
    }
}

/// What an export does with its file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportPurpose {
    /// Writes it into the recordings folder (or the destination).
    #[default]
    Save,
    /// Writes it to the cache and puts the file on the clipboard.
    Copy,
    /// Writes it to the cache for an upload.
    Upload,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// Names the part to keep; `None` keeps the whole recording.
    pub trim: Option<TrimRange>,
    /// Says which folder to write the file into; `None` uses the recordings folder. An existing
    /// file is never overwritten.
    pub destination: Option<PathBuf>,
    #[serde(default)]
    pub audio: ExportAudio,
    #[serde(default)]
    pub purpose: ExportPurpose,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub recording_id: RecordingId,
    pub path: PathBuf,
    pub duration_ms: u64,
    pub size: PhysicalSize,
    pub file_bytes: u64,
    /// False when the MP4 kept the original video stream (a pass-through trim).
    pub reencoded: bool,
    /// The part that was really exported: a pass-through trim starts at a keyframe.
    pub trim: TrimRange,
}

/// How a microphone or a camera is connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceTransport {
    BuiltIn,
    Usb,
    Bluetooth,
    Virtual,
    Other,
}

/// A microphone or a camera as the system lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// The system's identifier (`AVCaptureDevice.uniqueID`), stable across launches, which a
    /// configuration stores.
    pub id: String,
    /// The name the system shows, such as "MacBook Pro Microphone".
    pub name: String,
    pub is_default: bool,
    pub transport: DeviceTransport,
}

/// The state of a device permission, in the words of `glimpsfy_capture::PermissionState`.
pub use glimpsfy_capture::PermissionState;

/// The inputs a recording can use on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingDevices {
    pub microphones: Vec<Device>,
    pub cameras: Vec<Device>,
    /// Whether the system can record its own audio output here.
    pub system_audio: bool,
    pub microphone_permission: PermissionState,
    pub camera_permission: PermissionState,
}

/// What a recorded file holds, read back from the file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub duration_ms: u64,
    pub size: PhysicalSize,
    /// The nominal frame rate of the video track.
    pub fps: f64,
    pub video_codec: String,
    /// The audio tracks in file order, named by the title the writer gave each.
    pub audio: Vec<TrackKind>,
    pub file_bytes: u64,
}
