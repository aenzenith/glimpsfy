//! The trait every platform implements for stills, plus the permission check.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::display::{Display, DisplayId, WindowId, WindowInfo, WindowListOptions};
use crate::error::{CaptureError, Result};
use crate::frame::Frame;
use crate::geometry::{selection_to_frame, LogicalRect};
use crate::stream::{CaptureStream, StreamConfig, StreamSink};

/// The state of a system permission as the app can observe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionState {
    Granted,
    /// Refused, or not asked yet on a platform that cannot tell the two apart (macOS reports
    /// only granted or not for screen recording).
    Denied,
    NotDetermined,
    /// The platform has no such permission (Windows and X11 for screen capture).
    NotRequired,
}

/// Options shared by every still capture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureOptions {
    /// Draws the mouse pointer into the frame (Settings › General › Include the mouse pointer).
    pub shows_cursor: bool,
    /// Leaves Glimpsfy's own windows (toasts, pins) out of the frame.
    pub exclude_own_windows: bool,
    /// Leaves the desktop icons, notification banners and widgets out of the frame (Settings ›
    /// Output › Hide desktop icons and notifications); only macOS can, the others ignore it.
    #[serde(default)]
    pub clean_desktop: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        CaptureOptions { shows_cursor: false, exclude_own_windows: true, clean_desktop: false }
    }
}

/// Options of a single-window capture (Settings › Output › Window captures).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowCaptureOptions {
    pub shows_cursor: bool,
    /// Keeps the system's drop shadow around the window, with transparent pixels around it.
    pub include_shadow: bool,
    /// Keeps the rounded corners transparent; when off, the corners are filled with `background`
    /// (white when there is none).
    pub keep_rounded_corners: bool,
    /// Fills every transparent pixel (the corners and the shadow's surroundings) with this colour,
    /// as `[r, g, b]`; `None` keeps the transparency.
    #[serde(default)]
    pub background: Option<[u8; 3]>,
    /// Fills the translucent parts of the window's body with the system's window colour, so they
    /// come out neither dark nor see-through; the shadow and the corners keep their alpha.
    #[serde(default)]
    pub opaque_body: bool,
    /// Draws a thin line along the window's edge and its rounded corners, inside the window.
    #[serde(default)]
    pub border: bool,
}

impl Default for WindowCaptureOptions {
    fn default() -> Self {
        WindowCaptureOptions {
            shows_cursor: false,
            include_shadow: true,
            keep_rounded_corners: true,
            background: None,
            opaque_body: false,
            border: false,
        }
    }
}

/// The way a still capture took its frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePath {
    /// Rectangles of the composited screen (macOS 15.2 and later, nothing of Glimpsfy showing).
    Rect,
    /// A display filter that leaves Glimpsfy's own windows out.
    Filtered,
    /// The rectangles were refused, so the displays were taken with the filter after all.
    RectThenFiltered,
    /// One window on its own.
    Window,
}

impl CapturePath {
    pub fn as_str(self) -> &'static str {
        match self {
            CapturePath::Rect => "rect",
            CapturePath::Filtered => "filtered",
            CapturePath::RectThenFiltered => "rect refused, then filtered",
            CapturePath::Window => "window",
        }
    }
}

/// How one still capture went, for the log of slow and failed captures: which path it took and
/// how long the system took to answer each request.
#[derive(Clone, Debug)]
pub struct CaptureReport {
    pub path: CapturePath,
    /// The time spent on refused rectangles before the filtered capture.
    pub rect: Option<Duration>,
    /// The time spent fetching the system's list of displays, windows and apps, when it was not
    /// cached.
    pub content: Option<Duration>,
    /// When each display's frame arrived after its request, in the order asked; `None` when it
    /// never did. A window capture names the display that shows most of the window.
    pub frames: Vec<(DisplayId, Option<Duration>)>,
    /// From the call to its return.
    pub total: Duration,
}

/// One platform's way of reading the screen.
///
/// Every method may block for tens of milliseconds while the system answers. Call them from a
/// worker thread, never from the UI or main thread: on macOS, ScreenCaptureKit answers on its
/// own queues, and a blocked main thread would also freeze the overlay windows.
pub trait CaptureBackend: Send + Sync {
    /// Every connected display, the primary one first.
    fn displays(&self) -> Result<Vec<Display>>;

    /// Windows in stacking order, the frontmost first, with titles and owners.
    fn windows(&self, options: WindowListOptions) -> Result<Vec<WindowInfo>>;

    /// A full-resolution frame of one display.
    fn capture_display(&self, display: DisplayId, options: &CaptureOptions) -> Result<Frame> {
        let mut frames = self.capture_displays(&[display], options)?;
        frames.pop().map(|(_, frame)| frame).ok_or(CaptureError::DisplayNotFound(display))
    }

    /// Frames of several displays taken together, so a multi-display overlay freezes one moment.
    /// The result keeps the order of `displays`.
    fn capture_displays(&self, displays: &[DisplayId], options: &CaptureOptions) -> Result<Vec<(DisplayId, Frame)>>;

    /// One window on its own, also when other windows cover it or it reaches past the screen's
    /// edge, with real alpha around its rounded corners (and its shadow, when asked).
    fn capture_window(&self, window: WindowId, options: &WindowCaptureOptions) -> Result<Frame>;

    /// A rectangle of the global desktop space, taken from the one display that holds most of it
    /// and clipped to that display (a frame never mixes two displays' scales or colour spaces).
    /// The pixels are exactly the ones an area selection of the same rectangle crops, because both
    /// go through `selection_to_frame`.
    fn capture_rect(&self, rect: LogicalRect, options: &CaptureOptions) -> Result<Frame> {
        let displays = self.displays()?;
        let display = displays
            .iter()
            .filter_map(|d| d.frame.intersection(&rect).map(|common| (d, common.width * common.height)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(d, _)| d.clone())
            .ok_or_else(|| CaptureError::InvalidArgument("the rectangle lies on no display".into()))?;
        let frame = self.capture_display(display.id, options)?;
        let local = rect.relative_to(display.frame.origin());
        let crop = selection_to_frame(local, display.frame.size(), frame.size)
            .ok_or_else(|| CaptureError::InvalidArgument("the rectangle is empty".into()))?;
        frame.crop(crop)
    }

    /// Whether the app may read the screen now. Never shows a system prompt.
    fn permission(&self) -> PermissionState;

    /// Asks the system for the permission. The first call may show the system prompt; on macOS
    /// later calls return at once, and the user grants the permission in System Settings.
    fn request_permission(&self) -> PermissionState;

    /// Forgets anything cached about displays and windows, after the display layout changed.
    fn invalidate(&self) {}

    /// Loads what the first capture needs, so it is as fast as the later ones. Does nothing
    /// while the permission is missing, so it never shows a system prompt.
    fn prepare(&self) {}

    /// The report of the last `capture_displays` or `capture_window` call made on the calling
    /// thread, which this call takes; `None` on platforms that keep none.
    fn take_report(&self) -> Option<CaptureReport> {
        None
    }

    /// A stream of frames and audio for recording. The stream is created stopped.
    fn open_stream(&self, config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>>;
}
