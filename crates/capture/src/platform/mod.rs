//! One backend per platform. Each module compiles only on its own platform, and every platform
//! offers the same `new_backend`.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
/// The recording stream (screen recording owns it; the screenshot side is `macos`).
#[cfg(target_os = "macos")]
pub mod macos_stream;
#[cfg(target_os = "windows")]
pub mod windows;

use std::sync::Arc;

use crate::backend::CaptureBackend;
use crate::frame::ColorSpace;

/// The backend of the platform the app was built for.
pub fn new_backend() -> Arc<dyn CaptureBackend> {
    #[cfg(target_os = "macos")]
    return Arc::new(macos::MacBackend::new());
    #[cfg(target_os = "windows")]
    return Arc::new(windows::WindowsBackend::new());
    #[cfg(target_os = "linux")]
    return Arc::new(linux::LinuxBackend::new());
}

/// The ICC profile the platform gives for `space`. sRGB needs none (PNG tags it with its own
/// chunk, and untagged JPEG and WebP mean sRGB); Display P3 comes from Core Graphics on macOS and
/// does not occur elsewhere yet.
pub fn icc_profile(space: ColorSpace) -> Option<Arc<[u8]>> {
    match space {
        ColorSpace::Srgb => None,
        #[cfg(target_os = "macos")]
        ColorSpace::DisplayP3 => macos::display_p3_profile(),
        #[cfg(not(target_os = "macos"))]
        ColorSpace::DisplayP3 => None,
    }
}
