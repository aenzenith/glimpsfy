//! Event names, the one place the core spells them. The UI's copy is `src/lib/events.ts`; a name
//! changes in both files or in neither.
//!
//! A name is `area://what-happened`. Events that a crate's area owns alone (upload, history,
//! record) are declared in that area's command file, next to the payload they carry.

/// To one overlay window: a capture is frozen and this window's frame is ready to fetch.
/// Payload: `commands::capture::OverlayFrame`.
pub const CAPTURE_STARTED: &str = "capture://started";
/// To every overlay window: the capture ended (copied, saved, cancelled or failed).
/// Payload: `commands::capture::CaptureClosed`.
pub const CAPTURE_CLOSED: &str = "capture://closed";
/// From an overlay window to the others and the core: the user started a selection on this
/// display, so the other displays drop theirs and the keyboard stays here
/// (`windows::follow_pointer`). Payload: `{ captureId, displayId }`.
pub const OVERLAY_SELECTION_STARTED: &str = "overlay://selection-started";
/// From an overlay window to the others and the core: this display's selection is gone, so the
/// keyboard follows the pointer again. Payload: `{ captureId, displayId }`.
pub const OVERLAY_SELECTION_ENDED: &str = "overlay://selection-ended";

/// The image of a capture reached the clipboard. Payload: `commands::output::OutputDone`.
pub const OUTPUT_COPIED: &str = "output://copied";
/// The image of a capture was written to a file. Payload: `commands::output::OutputDone`.
pub const OUTPUT_SAVED: &str = "output://saved";
/// An output failed. Payload: `commands::output::OutputFailed`.
pub const OUTPUT_FAILED: &str = "output://failed";

/// To the toast window: show a toast. Payload: `windows::Toast`.
pub const TOAST_SHOW: &str = "toast://show";

/// A permission's state changed or was checked again. Payload: `permissions::Permissions`.
pub const PERMISSIONS_CHANGED: &str = "permissions://changed";
/// The settings were saved. Payload: `settings::Settings`.
pub const SETTINGS_CHANGED: &str = "settings://changed";
/// A display was connected, removed, moved or rescaled. Payload: `Vec<Display>`.
pub const DISPLAYS_CHANGED: &str = "displays://changed";
/// A global shortcut could not be registered (taken by another app, or not a valid key).
/// Payload: `shortcuts::ShortcutProblem`.
pub const SHORTCUT_FAILED: &str = "shortcut://failed";
