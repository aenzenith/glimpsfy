//! # glimpsfy-capture
//!
//! The capture contract of Glimpsfy: what a display, a window and a frame are, how logical and
//! physical coordinates relate, and the [`CaptureBackend`] trait each platform implements for
//! stills and recording streams.
//!
//! - [`geometry`] holds the two coordinate spaces and the crop maths between them.
//! - [`display`] describes displays and windows.
//! - [`frame`] is a pixel buffer with its format, colour space and scale, and its outputs;
//!   [`encode`] writes it as PNG, JPEG or WebP and [`resample`] scales it to 1×.
//! - [`window`] finishes a window capture (square corners, background fill) the same way on
//!   every platform.
//! - [`backend`] is the trait for stills and permissions; [`stream`] is the recording side.
//! - [`new_backend`] returns the backend of the platform the app was built for. macOS uses
//!   ScreenCaptureKit; the Windows and Linux backends compile and return
//!   [`CaptureError::NotImplemented`] until their milestones.

pub mod backend;
pub mod display;
pub mod encode;
pub mod error;
pub mod frame;
pub mod geometry;
pub mod resample;
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
pub use stream::{
    AudioBuffer, AudioSource, CaptureStream, HostTime, MicrophoneSource, NativeBuffer, StreamConfig, StreamSink,
    StreamTarget, VideoBuffer, VideoFrame,
};
// The recording side of the stream contract (screen recording).
#[cfg(target_os = "macos")]
pub use stream::MacPixelBuffer;
pub use stream::{host_now, open_microphone, FilterPlan, StreamPixelFormat};
pub use window::finish_window;
