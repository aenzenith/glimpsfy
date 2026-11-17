//! The linux backend (milestone 8), to be built on the xdg-desktop-portal Screenshot and ScreenCast portals. Every method
//! returns `CaptureError::NotImplemented` until then, so the app compiles and runs on linux
//! without capturing anything.

use crate::backend::{CaptureBackend, CaptureOptions, PermissionState, WindowCaptureOptions};
use crate::display::{Display, DisplayId, WindowId, WindowInfo, WindowListOptions};
use crate::error::{CaptureError, Result};
use crate::frame::Frame;
use crate::geometry::LogicalRect;
use crate::stream::{CaptureStream, StreamConfig, StreamSink};

#[derive(Default)]
pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        LinuxBackend
    }
}

impl CaptureBackend for LinuxBackend {
    fn displays(&self) -> Result<Vec<Display>> {
        Err(CaptureError::NotImplemented("display listing on linux"))
    }

    fn windows(&self, _options: WindowListOptions) -> Result<Vec<WindowInfo>> {
        Err(CaptureError::NotImplemented("window listing on linux"))
    }

    fn capture_displays(&self, _displays: &[DisplayId], _options: &CaptureOptions) -> Result<Vec<(DisplayId, Frame)>> {
        Err(CaptureError::NotImplemented("display capture on linux"))
    }

    fn capture_window(&self, _window: WindowId, _options: &WindowCaptureOptions) -> Result<Frame> {
        Err(CaptureError::NotImplemented("window capture on linux"))
    }

    fn capture_rect(&self, _rect: LogicalRect, _options: &CaptureOptions) -> Result<Frame> {
        Err(CaptureError::NotImplemented("rectangle capture on linux"))
    }

    fn permission(&self) -> PermissionState {
        PermissionState::NotRequired
    }

    fn request_permission(&self) -> PermissionState {
        PermissionState::NotRequired
    }

    fn open_stream(&self, _config: StreamConfig, _sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
        Err(CaptureError::NotImplemented("recording streams on linux"))
    }
}
