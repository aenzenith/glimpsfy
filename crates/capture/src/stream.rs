//! The streaming side of capture, which the recorder (`glimpsfy-record`) consumes.
//!
//! A stream delivers video frames and audio buffers to a [`StreamSink`] on the backend's own
//! threads. Every buffer carries a [`HostTime`] from one clock, so the recorder can line up video,
//! system audio and the microphone without drift.

use std::any::Any;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::display::{DisplayId, WindowId};
use crate::error::{CaptureError, Result};
use crate::frame::{ColorSpace, Frame};
use crate::geometry::{LogicalRect, PhysicalSize};

/// Nanoseconds on the system's host clock (`mach_absolute_time` converted on macOS, the QPC
/// clock on Windows, `CLOCK_MONOTONIC` on Linux). Only differences between two values mean
/// anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostTime(pub u64);

/// What a stream records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StreamTarget {
    Display {
        display: DisplayId,
    },
    Window {
        window: WindowId,
    },
    /// A rectangle of one display, display-local and logical.
    Area {
        display: DisplayId,
        rect: LogicalRect,
    },
}

/// A microphone as the system names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneSource {
    /// The system's identifier (`AVCaptureDevice.uniqueID` on macOS, the endpoint id on Windows,
    /// the PulseAudio source name on Linux); `None` is the default input.
    pub device_id: Option<String>,
}

/// The pixel layout of a stream's video frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamPixelFormat {
    /// 8-bit blue, green, red, alpha, as a still capture has it.
    #[default]
    Bgra,
    /// 8-bit 4:2:0 luma and chroma planes in video range with the BT.709 matrix, which a video
    /// encoder takes without converting.
    Nv12,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamConfig {
    pub target: StreamTarget,
    /// Frames per second, 30 or 60 in the app.
    pub fps: u32,
    /// The size of the delivered frames; `None` keeps the native pixel size divided by
    /// `size_divisor`.
    pub output_size: Option<PhysicalSize>,
    /// Divides the native pixel size (2 halves it); rounded to even sizes for `Nv12`, which video
    /// encoders need, and kept exact for `Bgra`.
    #[serde(default = "one")]
    pub size_divisor: u32,
    pub shows_cursor: bool,
    pub capture_system_audio: bool,
    pub microphone: Option<MicrophoneSource>,
    /// The app's windows the stream must not show, such as the recording control bar. The
    /// macOS backend leaves out every window of the app except `shown_windows`, so a window the
    /// list forgot is left out as well.
    pub excluded_windows: Vec<WindowId>,
    /// The app's windows the stream shows, such as the webcam bubble and the click ring.
    #[serde(default)]
    pub shown_windows: Vec<WindowId>,
    pub color_space: ColorSpace,
    #[serde(default)]
    pub pixel_format: StreamPixelFormat,
    /// How many frames the system may queue before it drops one.
    pub queue_depth: u32,
    /// Leaves the desktop icons, notification banners and widgets out of a display or area
    /// stream (Settings › Recording › Hide desktop icons and notifications). A window stream
    /// shows its window alone either way.
    #[serde(default)]
    pub clean_desktop: bool,
}

fn one() -> u32 {
    1
}

/// The window filter a stream is built from, worked out before any system call so it can be
/// checked: which windows the stream shows, which of the app's windows it leaves out, and which
/// part of the display it crops to.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterPlan {
    pub display: Option<DisplayId>,
    /// The window a window recording follows; `None` records the display or the area.
    pub window: Option<WindowId>,
    /// Leaves out every window of this app, except `shown_windows`.
    pub exclude_own_app: bool,
    /// The app's windows the stream leaves out, listed one by one for the system calls that
    /// take a list of windows.
    pub excluded_windows: Vec<WindowId>,
    /// The app's windows the stream shows.
    pub shown_windows: Vec<WindowId>,
    /// The part of the display to record, display-local and logical; `None` is all of it.
    pub source_rect: Option<LogicalRect>,
    /// Leaves the desktop's icons, banners and widgets out (`desktop_exceptions`).
    pub clean_desktop: bool,
}

/// The bundle of Finder, whose window at the desktop icon level holds the desktop's icons.
pub const FINDER: &str = "com.apple.finder";
/// The bundle of Notification Center, which draws the notification banners and the desktop's
/// widgets.
pub const NOTIFICATION_CENTER: &str = "com.apple.notificationcenterui";

/// A window as the system lists it, for working out a clean desktop.
#[derive(Clone, Debug, PartialEq)]
pub struct ListedWindow {
    pub id: WindowId,
    /// The owning app's bundle identifier, when it has one.
    pub bundle_id: Option<String>,
    /// The window level: 0 for ordinary windows, far below 0 for the desktop's own windows.
    pub layer: i32,
}

/// The apps a filter leaves out on top of this app, and the windows that show anyway.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopExceptions {
    /// Bundle identifiers whose every window is left out.
    pub excluded_bundles: Vec<&'static str>,
    /// Windows that show although their app is left out: this app's shown windows and, on a
    /// clean desktop, Finder's ordinary windows.
    pub excepted_windows: Vec<WindowId>,
}

impl FilterPlan {
    /// The plan of a still capture, the same for each display it takes; a still has no shown
    /// windows of this app.
    pub fn still(exclude_own_app: bool, clean_desktop: bool) -> FilterPlan {
        FilterPlan {
            display: None,
            window: None,
            exclude_own_app,
            excluded_windows: Vec::new(),
            shown_windows: Vec::new(),
            source_rect: None,
            clean_desktop,
        }
    }

    /// What a display filter leaves out besides this app, given the windows the system lists.
    ///
    /// A clean desktop leaves out Finder and Notification Center as apps, because a filter can
    /// leave out apps or windows but not both. Finder's windows at level 0 and above (its
    /// browser windows, Get Info, the copy progress) are taken back as exceptions, so only the
    /// icon window below them drops; Notification Center stays out as a whole, so a banner that
    /// appears later is left out too. Without a clean desktop, or for a window recording, the
    /// plan is the one a filter had before.
    pub fn desktop_exceptions(&self, windows: &[ListedWindow]) -> DesktopExceptions {
        let mut excepted = self.shown_windows.clone();
        if !self.clean_desktop || self.window.is_some() {
            return DesktopExceptions { excluded_bundles: Vec::new(), excepted_windows: excepted };
        }
        for window in windows {
            if window.layer >= 0 && window.bundle_id.as_deref() == Some(FINDER) && !excepted.contains(&window.id) {
                excepted.push(window.id);
            }
        }
        DesktopExceptions { excluded_bundles: vec![FINDER, NOTIFICATION_CENTER], excepted_windows: excepted }
    }

    /// The plan for `config`. A window recording takes the display that shows the window
    /// (`window_display`) and crops to the window's frame (`window_frame`, display-local), so
    /// the click ring and the webcam bubble are recorded over it as well.
    pub fn new(config: &StreamConfig, window_display: Option<(DisplayId, LogicalRect)>) -> FilterPlan {
        let shown: Vec<WindowId> = config.shown_windows.clone();
        let excluded: Vec<WindowId> = config.excluded_windows.iter().copied().filter(|w| !shown.contains(w)).collect();
        match &config.target {
            StreamTarget::Display { display } => FilterPlan {
                display: Some(*display),
                window: None,
                exclude_own_app: true,
                excluded_windows: excluded,
                shown_windows: shown,
                source_rect: None,
                clean_desktop: config.clean_desktop,
            },
            StreamTarget::Area { display, rect } => FilterPlan {
                display: Some(*display),
                window: None,
                exclude_own_app: true,
                excluded_windows: excluded,
                shown_windows: shown,
                source_rect: Some(*rect),
                clean_desktop: config.clean_desktop,
            },
            StreamTarget::Window { window } => FilterPlan {
                display: window_display.map(|(d, _)| d),
                window: Some(*window),
                exclude_own_app: true,
                excluded_windows: excluded,
                shown_windows: shown,
                source_rect: window_display.map(|(_, r)| r),
                // The window shows alone, with no desktop behind it.
                clean_desktop: false,
            },
        }
    }

    /// Whether the stream would show `window`.
    pub fn shows(&self, window: WindowId, own: bool) -> bool {
        if self.window == Some(window) || self.shown_windows.contains(&window) {
            return true;
        }
        if own {
            return false;
        }
        // Another app's window shows unless the recording follows a single window.
        self.window.is_none()
    }
}

/// A `CVPixelBuffer` handed from the macOS stream to the macOS writer inside a [`NativeBuffer`],
/// retained, so the frame reaches the encoder without a copy.
#[cfg(target_os = "macos")]
pub struct MacPixelBuffer(pub objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>);

// SAFETY: a CVPixelBuffer is reference counted with atomic counts and the stream never writes to
// it after delivering it; the writer only reads it.
#[cfg(target_os = "macos")]
unsafe impl Send for MacPixelBuffer {}

/// A platform's own video buffer, passed through untouched so an encoder of the same platform
/// can take it without a copy: a retained `CMSampleBuffer` on macOS, an `ID3D11Texture2D` on
/// Windows, a PipeWire buffer on Linux. Code of another platform never looks inside.
pub struct NativeBuffer(pub Box<dyn Any + Send>);

impl std::fmt::Debug for NativeBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeBuffer")
    }
}

#[derive(Debug)]
pub enum VideoBuffer {
    /// Pixels in memory, as a still capture has them.
    Cpu(Arc<Frame>),
    Native(NativeBuffer),
}

#[derive(Debug)]
pub struct VideoFrame {
    pub timestamp: HostTime,
    pub size: PhysicalSize,
    /// The layout of a native buffer; a `Cpu` buffer carries its own format.
    pub format: StreamPixelFormat,
    pub color_space: ColorSpace,
    pub buffer: VideoBuffer,
}

/// Which input an audio buffer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioSource {
    System,
    Microphone,
}

/// Interleaved 32-bit float samples.
#[derive(Debug)]
pub struct AudioBuffer {
    pub timestamp: HostTime,
    pub source: AudioSource,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// Receives what a stream produces. The methods are called on the backend's threads, possibly
/// on several at once, so an implementation hands the buffers on quickly and never blocks.
pub trait StreamSink: Send + Sync {
    fn video(&self, frame: VideoFrame);
    fn audio(&self, buffer: AudioBuffer);
    /// The stream ended: on `stop`, or with the reason it failed (a display was unplugged, the
    /// permission was withdrawn).
    fn stopped(&self, error: Option<CaptureError>);
}

/// A running or stopped stream. Dropping it stops it.
pub trait CaptureStream: Send {
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    /// Changes the configuration of a running stream, for example a muted microphone.
    fn update(&mut self, config: StreamConfig) -> Result<()>;
}

/// Now on the host clock that stamps every buffer of a stream.
pub fn host_now() -> HostTime {
    #[cfg(target_os = "macos")]
    return crate::platform::macos_stream::host_now();
    #[cfg(target_os = "windows")]
    return crate::platform::windows::host_now();
    #[cfg(target_os = "linux")]
    return crate::platform::linux::host_now();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        HostTime(START.get_or_init(Instant::now).elapsed().as_nanos() as u64)
    }
}

/// The pixels of a stream's video frame in memory, for code that reads them (the scrolling
/// capture's stitcher) instead of handing them to an encoder: a `Cpu` buffer is cloned, and the
/// pixel buffer of a macOS `Bgra` stream is copied out. The frame's colour info carries no ICC
/// profile and its scale is 1; the caller knows both.
pub fn cpu_frame(frame: &VideoFrame) -> Result<Frame> {
    match &frame.buffer {
        VideoBuffer::Cpu(pixels) => Ok((**pixels).clone()),
        VideoBuffer::Native(_) => {
            #[cfg(target_os = "macos")]
            return crate::platform::macos_stream::cpu_frame(frame);
            #[cfg(not(target_os = "macos"))]
            Err(CaptureError::NotImplemented("reading a native video buffer on this platform"))
        }
    }
}

/// Opens a microphone on its own, for a level meter or for recording beside a stream that
/// cannot capture the microphone itself (macOS 14). Only `audio` and `stopped` of the sink are
/// called. The stream is created stopped.
pub fn open_microphone(source: MicrophoneSource, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
    #[cfg(target_os = "macos")]
    return crate::platform::macos_stream::open_microphone(source, sink);
    #[cfg(target_os = "windows")]
    return crate::platform::windows::open_microphone(source, sink);
    #[cfg(target_os = "linux")]
    return crate::platform::linux::open_microphone(source, sink);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (source, sink);
        Err(CaptureError::NotImplemented("the microphone on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bars, the countdown and the bubble's controls stay out of every kind of recording,
    /// while the webcam bubble and the click ring stay in.
    #[test]
    fn own_windows_stay_out_of_every_target() {
        let bars = [WindowId(901), WindowId(902), WindowId(903)];
        let shown = [WindowId(911), WindowId(912)];
        let rect = LogicalRect { x: 10.0, y: 20.0, width: 300.0, height: 200.0 };
        let targets = [
            StreamTarget::Display { display: DisplayId(1) },
            StreamTarget::Area { display: DisplayId(1), rect },
            StreamTarget::Window { window: WindowId(42) },
        ];
        for target in targets {
            let config = StreamConfig {
                target: target.clone(),
                fps: 30,
                output_size: None,
                size_divisor: 1,
                shows_cursor: true,
                capture_system_audio: false,
                microphone: None,
                excluded_windows: bars.iter().chain(&shown).copied().collect(),
                shown_windows: shown.to_vec(),
                color_space: ColorSpace::Srgb,
                pixel_format: StreamPixelFormat::Nv12,
                queue_depth: 6,
                clean_desktop: false,
            };
            let plan = FilterPlan::new(&config, Some((DisplayId(1), rect)));
            assert!(plan.exclude_own_app, "{target:?}");
            for bar in bars {
                assert!(plan.excluded_windows.contains(&bar), "{target:?} leaves {bar:?} in");
                assert!(!plan.shows(bar, true), "{target:?} shows {bar:?}");
            }
            for window in shown {
                assert!(!plan.excluded_windows.contains(&window), "{target:?} leaves {window:?} out");
                assert!(plan.shows(window, true), "{target:?} hides {window:?}");
            }
        }
    }

    /// A clean desktop drops Finder's icon window and everything of Notification Center, and
    /// nothing else: every Finder window at level 0 or above and the app's shown windows stay in,
    /// since a window wrongly left out is missing from a take that cannot be recorded again.
    /// With the switch off, and for a window recording, the filter stays as it was.
    #[test]
    fn clean_desktop_leaves_out_only_desktop_icons_and_notifications() {
        let listed = |id: u32, bundle: Option<&str>, layer: i32| ListedWindow {
            id: WindowId(id),
            bundle_id: bundle.map(str::to_string),
            layer,
        };
        let icons = listed(1, Some(FINDER), -2_147_483_603);
        let browser = listed(2, Some(FINDER), 0);
        let copying = listed(3, Some(FINDER), 3);
        let banner = listed(4, Some(NOTIFICATION_CENTER), 23);
        let widget = listed(5, Some(NOTIFICATION_CENTER), -2_147_483_603);
        let safari = listed(6, Some("com.apple.Safari"), 0);
        let unnamed = listed(7, None, 0);
        let wallpaper = listed(8, Some("com.apple.dock"), -2_147_483_624);
        let windows = [icons, browser.clone(), copying.clone(), banner, widget, safari, unnamed, wallpaper];
        let shown = [WindowId(911), WindowId(912)];
        let rect = LogicalRect { x: 10.0, y: 20.0, width: 300.0, height: 200.0 };
        let config = |target: StreamTarget, clean_desktop: bool| StreamConfig {
            target,
            fps: 30,
            output_size: None,
            size_divisor: 1,
            shows_cursor: true,
            capture_system_audio: true,
            microphone: None,
            excluded_windows: vec![WindowId(901)],
            shown_windows: shown.to_vec(),
            color_space: ColorSpace::Srgb,
            pixel_format: StreamPixelFormat::Nv12,
            queue_depth: 6,
            clean_desktop,
        };
        let unchanged = DesktopExceptions { excluded_bundles: Vec::new(), excepted_windows: shown.to_vec() };

        let screens =
            [StreamTarget::Display { display: DisplayId(1) }, StreamTarget::Area { display: DisplayId(1), rect }];
        for target in screens {
            let plan = FilterPlan::new(&config(target.clone(), true), None);
            let clean = plan.desktop_exceptions(&windows);
            assert_eq!(clean.excluded_bundles, vec![FINDER, NOTIFICATION_CENTER], "{target:?}");
            let mut excepted = clean.excepted_windows.clone();
            excepted.sort_by_key(|w| w.0);
            assert_eq!(excepted, vec![browser.id, copying.id, shown[0], shown[1]], "{target:?}");

            let off = FilterPlan::new(&config(target.clone(), false), None);
            assert_eq!(off.desktop_exceptions(&windows), unchanged, "{target:?} changes with the switch off");
        }

        let window = StreamTarget::Window { window: WindowId(42) };
        let plan = FilterPlan::new(&config(window, true), Some((DisplayId(1), rect)));
        assert_eq!(plan.desktop_exceptions(&windows), unchanged, "a window recording changes");

        let still = FilterPlan::still(true, true).desktop_exceptions(&windows);
        assert_eq!(still.excluded_bundles, vec![FINDER, NOTIFICATION_CENTER]);
        assert_eq!(still.excepted_windows, vec![browser.id, copying.id]);
        let still_off = FilterPlan::still(true, false).desktop_exceptions(&windows);
        assert_eq!(still_off, DesktopExceptions::default());
    }
}
