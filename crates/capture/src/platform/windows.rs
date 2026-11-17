//! The windows backend (milestone 7), to be built on Windows.Graphics.Capture. Every method
//! returns `CaptureError::NotImplemented` until then, so the app compiles and runs on windows
//! without capturing anything.

use crate::backend::{CaptureBackend, CaptureOptions, PermissionState, WindowCaptureOptions};
use crate::display::{Display, DisplayId, WindowId, WindowInfo, WindowListOptions};
use crate::error::{CaptureError, Result};
use crate::frame::Frame;
use crate::geometry::LogicalRect;
use crate::stream::{CaptureStream, StreamConfig, StreamSink};

#[derive(Default)]
pub struct WindowsBackend;

impl WindowsBackend {
    pub fn new() -> Self {
        WindowsBackend
    }
}

impl CaptureBackend for WindowsBackend {
    fn displays(&self) -> Result<Vec<Display>> {
        Err(CaptureError::NotImplemented("display listing on windows"))
    }

    fn windows(&self, _options: WindowListOptions) -> Result<Vec<WindowInfo>> {
        Err(CaptureError::NotImplemented("window listing on windows"))
    }

    fn capture_displays(&self, _displays: &[DisplayId], _options: &CaptureOptions) -> Result<Vec<(DisplayId, Frame)>> {
        Err(CaptureError::NotImplemented("display capture on windows"))
    }

    fn capture_window(&self, _window: WindowId, _options: &WindowCaptureOptions) -> Result<Frame> {
        Err(CaptureError::NotImplemented("window capture on windows"))
    }

    fn capture_rect(&self, _rect: LogicalRect, _options: &CaptureOptions) -> Result<Frame> {
        Err(CaptureError::NotImplemented("rectangle capture on windows"))
    }

    fn permission(&self) -> PermissionState {
        PermissionState::NotRequired
    }

    fn request_permission(&self) -> PermissionState {
        PermissionState::NotRequired
    }

    fn open_stream(&self, _config: StreamConfig, _sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
        Err(CaptureError::NotImplemented("recording streams on windows"))
    }
}
