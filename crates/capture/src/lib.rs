//! # glimpsfy-capture
//!
//! The capture contract of Glimpsfy: what a display, a window and a frame are, how logical and
//! physical coordinates relate, and the [`CaptureBackend`] trait each platform implements for
//! stills and recording streams.
//!
//! - [`geometry`] holds the two coordinate spaces and the crop maths between them, and
//!   [`desktop`] maps between them where displays share no logical space (Windows).
//! - [`display`] describes displays and windows.
//! - [`frame`] is a pixel buffer with its format, colour space and scale, and its outputs;
//!   [`encode`] writes it as PNG, JPEG or WebP and [`resample`] scales it to 1×.
//! - [`window`] finishes a window capture (opaque body, square corners, border, background
//!   fill) the same way on every platform.
//! - [`backend`] is the trait for stills and permissions; [`stream`] is the recording side.
//! - [`stitch`] joins the frames of a scrolling area into one tall image (scrolling capture).
//! - [`new_backend`] returns the backend of the platform the app was built for. macOS uses
//!   ScreenCaptureKit; Windows uses GDI for stills and Windows.Graphics.Capture for windows and
//!   streams; Linux uses X11 (also under XWayland) and the desktop portals on Wayland.

pub mod backend;
pub mod desktop;
pub mod display;
pub mod encode;
pub mod error;
pub mod frame;
pub mod geometry;
pub mod resample;
pub mod stitch;
pub mod stream;
pub mod window;

mod platform;

pub use backend::{CaptureBackend, CaptureOptions, CapturePath, CaptureReport, PermissionState, WindowCaptureOptions};
pub use display::{Display, DisplayId, WindowId, WindowInfo, WindowListOptions};
pub use encode::ImageEncoding;
pub use error::{CaptureError, Result};
pub use frame::{ColorInfo, ColorSpace, Frame, PixelFormat, PngEffort};
pub use geometry::{
    selection_to_frame, LogicalPoint, LogicalRect, LogicalSize, PhysicalPoint, PhysicalRect, PhysicalSize, ScaleFactor,
};
pub use platform::new_backend;
// What the Linux app shell and the recorder ask the backend besides the trait: the session kind,
// where to keep the portal's restore tokens, and the sound server's sources.
#[cfg(target_os = "linux")]
pub use platform::linux::{
    diagnostics as linux_diagnostics, linux_session, microphones as linux_microphones,
    set_state_dir as linux_set_state_dir, system_audio_available as linux_system_audio_available, LinuxSession,
};
pub use stream::{
    AudioBuffer, AudioSource, CaptureStream, HostTime, MicrophoneSource, NativeBuffer, StreamConfig, StreamSink,
    StreamTarget, VideoBuffer, VideoFrame,
};
// The recording side of the stream contract (screen recording).
#[cfg(target_os = "macos")]
pub use stream::MacPixelBuffer;
pub use stream::{cpu_frame, host_now, open_microphone, FilterPlan, StreamPixelFormat};
pub use window::{finish_window, BodyHint};
