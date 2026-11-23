//! Switches for measuring the capture path in dev builds, read once from the environment.
//! Release builds ignore them all.
//!
//! - `GLIMPSFY_FRAME_FORMAT=rgba|bmp|png` picks how frames travel to the overlay (`frames.rs`).
//! - `GLIMPSFY_SYNTHETIC=1` replaces ScreenCaptureKit with generated frames of each display's real
//!   size, so the overlay path can be measured without the screen recording permission.
//! - `GLIMPSFY_SIMULATE_5K=1` makes every frame 5120 × 2880, the size of a 5K display, whatever
//!   the real display is (synthetic frames only).
//! - `GLIMPSFY_AUTOCAPTURE=N` runs the area capture N times after start, each through the same
//!   entry the shortcut uses, closes each overlay once it has painted, prints the statistics and
//!   quits. `GLIMPSFY_AUTOCAPTURE_GAP_MS` sets the pause between runs (default 600).
//! - `GLIMPSFY_HOME=<folder>` moves the settings, the history, the caches, the default save and
//!   recordings folders and the keychain service under that folder (`paths.rs`), so a dev run
//!   never touches the user's real data.
//! - `GLIMPSFY_FORCE_HDR=1` sends every display down the HDR capture path on macOS 15 and later,
//!   so its timing and its SDR output can be checked without HDR content on screen. The capture
//!   crate reads it itself (`platform/macos_hdr.rs`), because nothing here reaches the backend.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::frames::FrameFormat;

#[derive(Clone, Debug)]
pub struct DevOptions {
    pub frame_format: Option<FrameFormat>,
    pub synthetic: bool,
    pub simulate_5k: bool,
    pub autocapture: u32,
    pub autocapture_gap_ms: u64,
    /// `GLIMPSFY_HOME`: the folder a dev run keeps all of its data in.
    pub home: Option<PathBuf>,
}

pub fn options() -> &'static DevOptions {
    static OPTIONS: OnceLock<DevOptions> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let var = |name: &str| if cfg!(debug_assertions) { std::env::var(name).ok() } else { None };
        let on = |name: &str| var(name).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        DevOptions {
            frame_format: var("GLIMPSFY_FRAME_FORMAT").and_then(|v| FrameFormat::parse(&v)),
            synthetic: on("GLIMPSFY_SYNTHETIC"),
            simulate_5k: on("GLIMPSFY_SIMULATE_5K"),
            autocapture: var("GLIMPSFY_AUTOCAPTURE").and_then(|v| v.parse().ok()).unwrap_or(0),
            autocapture_gap_ms: var("GLIMPSFY_AUTOCAPTURE_GAP_MS").and_then(|v| v.parse().ok()).unwrap_or(600),
            home: var("GLIMPSFY_HOME").filter(|v| !v.trim().is_empty()).map(PathBuf::from),
        }
    })
}
