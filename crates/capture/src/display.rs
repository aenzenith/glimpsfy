//! Displays and windows as the capture backends describe them.

use serde::{Deserialize, Serialize};

use crate::frame::ColorSpace;
use crate::geometry::{LogicalRect, PhysicalSize, ScaleFactor};

/// A display, stable for as long as it stays connected. On macOS it is the `CGDirectDisplayID`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisplayId(pub u32);

/// A window of any app, stable while the window exists. On macOS it is the `CGWindowID`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowId(pub u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Display {
    pub id: DisplayId,
    /// A name for people, such as "Built-in Display" or the monitor's model.
    pub name: String,
    /// Where the display sits in the global desktop space, in points (see `geometry`).
    pub frame: LogicalRect,
    /// Physical pixels per point on this display.
    pub scale: ScaleFactor,
    /// The size of a full-resolution frame of this display.
    pub pixel_size: PhysicalSize,
    pub is_primary: bool,
    pub is_builtin: bool,
    /// The colour space a capture of this display is requested in and tagged with: Display P3
    /// when the panel is wide-gamut, sRGB otherwise.
    pub color_space: ColorSpace,
    /// Refresh rate in hertz, when the system reports one.
    pub refresh_rate: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: WindowId,
    /// The window's title. macOS reports it only while the screen recording permission is granted.
    pub title: Option<String>,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
    /// The window's frame in the global desktop space, in points.
    pub bounds: LogicalRect,
    /// The system's window layer: 0 for ordinary windows, higher for panels, menus and the menu bar.
    pub layer: i32,
    /// Stacking order at the moment of the listing: 0 is the frontmost window.
    pub z_index: u32,
    pub alpha: f64,
    pub is_on_screen: bool,
    /// The window's corner radius in points, when the platform reports one.
    pub corner_radius: Option<f64>,
    /// The window keeps itself out of screen captures (macOS `NSWindowSharingNone`, Windows
    /// display affinity), so a frame shows what lies behind it instead.
    #[serde(default)]
    pub capture_blocked: bool,
}

/// Which windows a listing returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowListOptions {
    /// Only windows that are on screen now, which is what the overlay's snapping needs.
    pub on_screen_only: bool,
    /// Leaves out the desktop picture and the desktop icons.
    pub exclude_desktop: bool,
    /// Leaves out Glimpsfy's own windows (the overlay, toasts, pins).
    pub exclude_own: bool,
}

impl Default for WindowListOptions {
    fn default() -> Self {
        WindowListOptions { on_screen_only: true, exclude_desktop: true, exclude_own: true }
    }
}
