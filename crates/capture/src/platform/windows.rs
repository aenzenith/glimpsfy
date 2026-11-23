//! Windows backend (Windows 10 1903 and later, Windows 11): GDI for stills of displays,
//! Windows.Graphics.Capture for single windows and for streams, WASAPI for audio.
//!
//! - **Coordinates.** Monitors of different scales share no logical space on Windows, so the
//!   global logical space is defined per display (`crate::desktop`): a display's frame starts at
//!   its top-left corner in virtual-screen pixels and is its pixel size divided by its scale.
//!   Window bounds are expressed through the display that holds most of the window. A
//!   `DisplayId` is the `HMONITOR` and a `WindowId` is the `HWND`.
//! - **DPI.** Every entry point makes its thread per-monitor DPI aware for its duration
//!   (`DpiScope`), so the backend reads physical pixels whatever the process declared.
//! - **Stills** (`capture_displays`) are one `BitBlt` of the rectangle that spans the displays
//!   asked for, split per display afterwards, so every display shows the same moment. GDI works
//!   in virtual machines and over Remote Desktop and draws no capture border. The app keeps its
//!   own windows out by marking them `WDA_EXCLUDEFROMCAPTURE`; the backend hides nothing.
//! - **A window** (`capture_window`) is one frame of a Windows.Graphics.Capture session, which
//!   shows it whole under other windows and with real alpha around Windows 11's rounded
//!   corners. When that is missing, fails or takes too long, `PrintWindow` draws it, and when
//!   that fails too, the window's rectangle is cropped from the screen. The system's drop shadow
//!   is in none of these, so `include_shadow` does nothing on Windows.
//! - **Streams** are in `stream`, audio in `audio`; `HostTime` is the QPC clock.
//! - **Threads.** COM and WinRT are only used on threads the backend creates and puts into the
//!   multithreaded apartment; nothing is assumed about the caller's thread, and nothing runs on
//!   or waits for the UI thread.
//! - No permission exists for any of this: `permission` is `NotRequired`.

mod audio;
mod gdi;
mod monitors;
mod stream;
mod sys;
mod wgc;
mod window_list;

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::backend::{
    CaptureBackend, CaptureOptions, CapturePath, CaptureReport, PermissionState, WindowCaptureOptions,
};
use crate::display::{Display, DisplayId, WindowId, WindowInfo, WindowListOptions};
use crate::error::{CaptureError, Result};
use crate::frame::{ColorInfo, Frame, PixelFormat};
use crate::geometry::{PhysicalRect, PhysicalSize};
use crate::platform::windows_convert as convert;
use crate::stream::{CaptureStream, StreamConfig, StreamSink};
use crate::window::{finish_window, BodyHint};

pub(crate) use stream::open_microphone;
pub(crate) use sys::host_now;
use sys::DpiScope;

/// How long the first frame of a window may take to arrive from Windows.Graphics.Capture. It
/// comes within a few frames of the display; a window that draws nothing (minimised a moment
/// ago, suspended) sends none, and `PrintWindow` is tried instead.
const WINDOW_FRAME_WAIT: Duration = Duration::from_millis(1_500);
/// How long the whole Windows.Graphics.Capture attempt may take, device and session included.
const WINDOW_CAPTURE_LIMIT: Duration = Duration::from_secs(4);
/// How long window captures go without Windows.Graphics.Capture after it failed to answer.
const SESSION_PAUSE: Duration = Duration::from_secs(60);
/// How long `prepare` waits for the graphics device to be made.
const WARM_LIMIT: Duration = Duration::from_secs(10);

thread_local! {
    /// The report of the last still capture made on this thread, until `take_report` takes it.
    static REPORT: RefCell<Option<CaptureReport>> = const { RefCell::new(None) };
}

fn keep_report(report: CaptureReport) {
    REPORT.with(|slot| *slot.borrow_mut() = Some(report));
}

pub struct WindowsBackend {
    /// App names by executable path (see `window_list::NameCache`).
    names: window_list::NameCache,
}

impl WindowsBackend {
    pub fn new() -> Self {
        WindowsBackend { names: std::sync::Mutex::new(HashMap::new()) }
    }

    fn displays_until(
        &self,
        displays: &[DisplayId],
        options: &CaptureOptions,
        report: &mut CaptureReport,
    ) -> Result<Vec<(DisplayId, Frame)>> {
        let _dpi = DpiScope::per_monitor();
        if displays.is_empty() {
            return Ok(Vec::new());
        }
        let monitors = monitors::layout()?;
        let mut wanted = Vec::with_capacity(displays.len());
        for id in displays {
            wanted.push(
                monitors.iter().find(|monitor| monitor.display.id == *id).ok_or(CaptureError::DisplayNotFound(*id))?,
            );
        }
        // One rectangle around every display asked for, taken in one copy.
        let around = wanted.iter().skip(1).fold(wanted[0].rect, |all, monitor| RECT {
            left: all.left.min(monitor.rect.left),
            top: all.top.min(monitor.rect.top),
            right: all.right.max(monitor.rect.right),
            bottom: all.bottom.max(monitor.rect.bottom),
        });
        let requested = Instant::now();
        let pixels = gdi::screen(around, options.shows_cursor)?;
        let arrived = requested.elapsed();
        for (_, frame_arrived) in &mut report.frames {
            *frame_arrived = Some(arrived);
        }

        let stride = (around.right - around.left) as usize * 4;
        let single = wanted.len() == 1;
        let mut pixels = pixels;
        let mut frames = Vec::with_capacity(wanted.len());
        for monitor in wanted {
            let size = monitor.display.pixel_size;
            let data = if single {
                // The copy is this display's frame as it is.
                std::mem::take(&mut pixels)
            } else {
                convert::copy_region(
                    &pixels,
                    stride,
                    (monitor.rect.left - around.left) as u32,
                    (monitor.rect.top - around.top) as u32,
                    size.width,
                    size.height,
                )
                .ok_or_else(|| CaptureError::Backend("a display lies outside the copy of the screen".into()))?
            };
            let frame = Frame::new(
                size,
                size.width as usize * 4,
                PixelFormat::Bgra8,
                ColorInfo::srgb(),
                monitor.display.scale,
                data,
            )?;
            frames.push((monitor.display.id, frame));
        }
        Ok(frames)
    }

    fn window_until(
        &self,
        window: WindowId,
        options: &WindowCaptureOptions,
        report: &mut CaptureReport,
    ) -> Result<Frame> {
        let _dpi = DpiScope::per_monitor();
        let handle = sys::hwnd(window);
        if !window_list::exists(handle) {
            return Err(CaptureError::WindowNotFound(window));
        }
        // What the user sees of the window, and what the compositor draws of it; they differ
        // for a maximised window, whose frame reaches past the monitor's edges.
        let visible = window_list::visible_frame(handle).ok_or(CaptureError::WindowNotFound(window))?;
        let drawn = window_list::frame_bounds(handle).unwrap_or(visible);
        let area = sys::physical(visible).ok_or(CaptureError::WindowNotFound(window))?;
        let displays: Vec<Display> = monitors::layout()?.into_iter().map(|monitor| monitor.display).collect();
        // The window's scale is that of the display that shows most of it.
        let display = crate::desktop::display_of_physical_rect(&displays, area);
        let scale = display.map(|display| display.scale).unwrap_or_default();
        if let Some(display) = display {
            report.frames.push((display.id, None));
        }
        let corner = window_list::corner_radius(handle) * scale.0;
        let requested = Instant::now();

        let mut attempts: Vec<String> = Vec::new();
        let mut image: Option<(Vec<u8>, PhysicalSize)> = None;

        // 1. One frame from Windows.Graphics.Capture, asked for on a thread of the backend's own.
        if window_list::is_minimised(handle) {
            attempts.push("the window is minimised".into());
        } else if session_stills_paused() {
            attempts.push("Windows.Graphics.Capture did not answer a moment ago".into());
        } else {
            let (raw, cursor, pointer_over) = (handle.0 as isize, options.shows_cursor, cursor_over(visible));
            let captured = sys::on_worker("window-capture", WINDOW_CAPTURE_LIMIT, move || {
                if !wgc::supported() {
                    return Err(CaptureError::Backend("Windows.Graphics.Capture is not available".into()));
                }
                // Before Windows 10 2004 a session always draws the pointer, so a capture without
                // it leaves this step out while the pointer is over the window.
                if !cursor && pointer_over && !wgc::can_hide_cursor() {
                    return Err(CaptureError::Backend(
                        "this Windows cannot leave the pointer out of a capture session".into(),
                    ));
                }
                wgc::window_once(raw, cursor, WINDOW_FRAME_WAIT)
            });
            match captured {
                Ok((pixels, size)) => {
                    // Only the visible part of the frame is kept (all of it, unless the window
                    // is maximised).
                    let origin = frame_origin(size, drawn, window_list::outer_rect(handle));
                    let (left, top) =
                        ((visible.left - origin.left).max(0) as u32, (visible.top - origin.top).max(0) as u32);
                    let kept = PhysicalSize::new(
                        area.width.min(size.width.saturating_sub(left)),
                        area.height.min(size.height.saturating_sub(top)),
                    );
                    let cut = if kept == size {
                        Some(pixels)
                    } else {
                        convert::copy_region(&pixels, size.width as usize * 4, left, top, kept.width, kept.height)
                    };
                    match cut {
                        Some(mut pixels) => {
                            // Two pixels more than the radius, so the blended edge of the arc
                            // keeps its alpha.
                            let kept_alpha = if corner > 0.0 { corner.ceil() as u32 + 2 } else { 0 };
                            convert::finish_window_pixels(&mut pixels, kept.width, kept.height, kept_alpha);
                            image = Some((pixels, kept));
                        }
                        None => attempts.push("the captured frame does not hold the window".into()),
                    }
                }
                Err(error) => {
                    if matches!(error, CaptureError::Timeout(ms) if ms == WINDOW_CAPTURE_LIMIT.as_millis() as u64) {
                        pause_session_stills();
                    }
                    attempts.push(error.to_string());
                }
            }
        }

        // 2. The window draws itself. 3. Its rectangle is cropped from the screen, where other
        //    windows may cover it. Neither knows alpha, so the corners are cut afterwards.
        if image.is_none() {
            let fallback = gdi::printed_window(handle, visible, options.shows_cursor).or_else(|error| {
                attempts.push(error.to_string());
                let on_screen = sys::intersect(visible, virtual_screen(&displays))
                    .ok_or_else(|| CaptureError::Backend("the window lies outside the screen".into()))?;
                gdi::screen(on_screen, options.shows_cursor).map(|pixels| (pixels, on_screen))
            });
            match fallback {
                Ok((mut pixels, rect)) => {
                    let size = sys::physical(rect).map(|rect| rect.size()).unwrap_or_default();
                    // A crop that the screen's edge cut has lost corners; it stays square.
                    if rect == visible {
                        convert::round_corners(&mut pixels, size.width, size.height, corner);
                    }
                    image = Some((pixels, size));
                }
                Err(error) => attempts.push(error.to_string()),
            }
        }

        let Some((pixels, size)) = image else {
            if !window_list::exists(handle) {
                return Err(CaptureError::WindowNotFound(window));
            }
            return Err(CaptureError::Backend(format!("the window could not be captured: {}", attempts.join("; "))));
        };
        if let Some((_, arrived)) = report.frames.last_mut() {
            *arrived = Some(requested.elapsed());
        }
        let frame = Frame::new(size, size.width as usize * 4, PixelFormat::Bgra8, ColorInfo::srgb(), scale, pixels)?;
        // The image is the window without a shadow, and DWM names its corners' radius.
        Ok(finish_window(frame, BodyHint { size: None, radius: Some(corner), fill: None }, options))
    }
}

/// Until when window captures leave Windows.Graphics.Capture out, after it failed to answer at
/// all. Without this, every capture would wait the full limit again before it falls back.
static SESSION_STILLS_PAUSED: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

fn session_stills_paused() -> bool {
    let paused = SESSION_STILLS_PAUSED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    paused.is_some_and(|until| Instant::now() < until)
}

fn pause_session_stills() {
    *SESSION_STILLS_PAUSED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(Instant::now() + SESSION_PAUSE);
}

/// The rectangle of the virtual screen whose top-left pixel is the first pixel of a window's
/// frame from Windows.Graphics.Capture. The frame covers the window as the compositor draws it
/// (`drawn`); should a build hand out the whole window rectangle instead (`outer`, with the
/// invisible borders a window is resized by), its size tells, and the frame is read from there.
fn frame_origin(size: PhysicalSize, drawn: RECT, outer: Option<RECT>) -> RECT {
    let matches = |rect: RECT| {
        (rect.right - rect.left).abs_diff(size.width as i32) <= 1
            && (rect.bottom - rect.top).abs_diff(size.height as i32) <= 1
    };
    match outer {
        Some(outer) if !matches(drawn) && matches(outer) => outer,
        _ => drawn,
    }
}

/// The rectangle around every display, in virtual-screen pixels.
fn virtual_screen(displays: &[Display]) -> RECT {
    let mut all = RECT::default();
    for (index, display) in displays.iter().enumerate() {
        let PhysicalRect { x, y, width, height } = crate::desktop::physical_frame(display);
        let rect = RECT { left: x, top: y, right: x + width as i32, bottom: y + height as i32 };
        all = if index == 0 {
            rect
        } else {
            RECT {
                left: all.left.min(rect.left),
                top: all.top.min(rect.top),
                right: all.right.max(rect.right),
                bottom: all.bottom.max(rect.bottom),
            }
        };
    }
    all
}

/// Whether the pointer is over `rect` (virtual-screen pixels) now.
fn cursor_over(rect: RECT) -> bool {
    let mut point = Default::default();
    // SAFETY: the call writes one `POINT`.
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return false;
    }
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for WindowsBackend {
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(monitors::list()?.into_iter().map(|monitor| monitor.display).collect())
    }

    fn windows(&self, options: WindowListOptions) -> Result<Vec<WindowInfo>> {
        let _dpi = DpiScope::per_monitor();
        let displays: Vec<Display> = monitors::layout()?.into_iter().map(|monitor| monitor.display).collect();
        window_list::list(options, &displays, &self.names)
    }

    fn capture_displays(&self, displays: &[DisplayId], options: &CaptureOptions) -> Result<Vec<(DisplayId, Frame)>> {
        let started = Instant::now();
        let mut report = CaptureReport {
            // One copy of a rectangle of the composited screen, which is what `Rect` names.
            path: CapturePath::Rect,
            rect: None,
            content: None,
            frames: displays.iter().map(|id| (*id, None)).collect(),
            total: Duration::ZERO,
        };
        let result = self.displays_until(displays, options, &mut report);
        report.total = started.elapsed();
        keep_report(report);
        result
    }

    fn capture_window(&self, window: WindowId, options: &WindowCaptureOptions) -> Result<Frame> {
        let started = Instant::now();
        let mut report = CaptureReport {
            path: CapturePath::Window,
            rect: None,
            content: None,
            frames: Vec::new(),
            total: Duration::ZERO,
        };
        let result = self.window_until(window, options, &mut report);
        report.total = started.elapsed();
        keep_report(report);
        result
    }

    fn permission(&self) -> PermissionState {
        PermissionState::NotRequired
    }

    fn request_permission(&self) -> PermissionState {
        PermissionState::NotRequired
    }

    fn prepare(&self) {
        // The first window capture would otherwise pay for the graphics device and for loading
        // Windows.Graphics.Capture.
        let _ = sys::on_worker("prepare", WARM_LIMIT, || {
            wgc::warm();
            Ok(())
        });
    }

    fn take_report(&self) -> Option<CaptureReport> {
        REPORT.with(|slot| slot.borrow_mut().take())
    }

    fn open_stream(&self, config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
        stream::open(config, sink)
    }
}
