//! macOS backend: ScreenCaptureKit for pixels, Core Graphics for the display and window lists.
//!
//! - Stills come from `SCScreenshotManager` (macOS 14+), one request per display, all in flight
//!   together, each answered on ScreenCaptureKit's own queue.
//! - On macOS 15.2 and later, while none of Glimpsfy's windows shows and the pointer is not
//!   wanted, the displays are taken as rectangles of the composited screen (`capture_rects`),
//!   which is two to three times faster than the filtered capture that leaves windows out.
//! - Every capture is requested in an explicit colour space (Display P3 for wide-gamut panels,
//!   sRGB otherwise) and in 32-bit BGRA, so the frame is tagged without guessing. A display that
//!   shows HDR content is the exception: it is asked for half floats in its own profile, which
//!   `macos_hdr` tone maps into the same kind of frame.
//! - A capture has one deadline for all of its requests (`DEADLINE`). A slow answer is waited
//!   for instead of being asked for again on the other path, because every app's requests wait
//!   in the same ScreenCaptureKit queue; only a refused rectangle falls back to the filter.
//! - `SCShareableContent` is fetched once and kept until the display layout changes
//!   (`invalidate`), because asking for it costs tens of milliseconds on every capture. It is
//!   fetched without holding the cache's lock, and `prepare` waits longer for it than a capture.
//! - The window list comes from `CGWindowListCopyWindowInfo`, which is fast and returns windows
//!   front to back, the order snapping needs. Titles are there only while the screen recording
//!   permission is granted; bundle ids come from the cached ScreenCaptureKit content. macOS
//!   exposes no window corner radius, so `corner_radius` stays `None`.
//! - A window is captured on its own with a desktop-independent-window filter, which draws it
//!   whole when other windows cover it or it reaches past the screen's edge, with real alpha.
//! - Display names come from `NSScreen.localizedName`, which only the main thread may read; a
//!   worker thread asks the main queue once per display layout and keeps the answer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{sel, AllocAnyThread, ClassType, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSColor, NSColorSpace, NSScreen};
use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CGRect};
use objc2_core_graphics::{
    kCGColorSpaceDisplayP3, kCGColorSpaceSRGB, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowIsOnscreen,
    kCGWindowLayer, kCGWindowName, kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID, kCGWindowSharingState,
    CGBitmapInfo, CGColorSpace, CGDataProvider, CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyColorSpace,
    CGDisplayCopyDisplayMode, CGDisplayIsBuiltin, CGDisplayMode, CGGetActiveDisplayList, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGMainDisplayID, CGPreflightScreenCaptureAccess, CGRectMakeWithDictionaryRepresentation,
    CGRequestScreenCaptureAccess, CGWindowListCopyWindowInfo, CGWindowListOption, CGWindowSharingType,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString};
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCDisplay, SCRunningApplication, SCScreenshotManager, SCShareableContent,
    SCStreamConfiguration, SCStreamErrorCode, SCWindow,
};

use crate::backend::{
    CaptureBackend, CaptureOptions, CapturePath, CaptureReport, PermissionState, WindowCaptureOptions,
};
use crate::display::{Display, DisplayId, WindowId, WindowInfo, WindowListOptions};
use crate::error::{CaptureError, Result};
use crate::frame::{ColorInfo, ColorSpace, Frame, PixelFormat};
use crate::geometry::{LogicalRect, PhysicalRect, PhysicalSize, ScaleFactor};
use crate::stream::{CaptureStream, FilterPlan, ListedWindow, StreamConfig, StreamSink, FINDER};
use crate::window::{finish_window, BodyHint};

/// `kCVPixelFormatType_32BGRA`, spelled out to keep Core Video out of the dependencies.
const PIXEL_FORMAT_BGRA: u32 = u32::from_be_bytes(*b"BGRA");

/// The room around a window for its shadow, in points; an active window's shadow reaches about
/// 35 points out and further down.
const SHADOW_MARGIN: f64 = 100.0;

/// How long a still capture may take, from the call to its last frame. All of its requests share
/// this time, so a slow answer is waited for instead of being asked for again: ScreenCaptureKit
/// queues every app's requests in one daemon (`replayd`), and an app that captures windows in
/// bursts (a window switcher's thumbnails) has held captures back for 2 to 17 seconds. A second
/// request would only join the end of the same queue.
const DEADLINE: Duration = Duration::from_secs(10);

/// How long `prepare` waits for the content. Nobody waits for it, and right after a wake the
/// daemon has taken 3.6 seconds; a fetch given up on would leave the cache empty for the next
/// capture to fill.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a worker thread waits for the main thread to read the screens' names. A main thread
/// that is busy (or not running an event loop, as in tests) costs this once and the names fall
/// back to generic ones.
const NAMES_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a window capture waits for the main thread to read the system's window colour; past
/// it the translucent parts are filled with white.
const FILL_TIMEOUT: Duration = Duration::from_millis(250);

thread_local! {
    /// The report of the last still capture made on this thread, until `take_report` takes it.
    static REPORT: RefCell<Option<CaptureReport>> = const { RefCell::new(None) };
}

/// ScreenCaptureKit objects are immutable once made and documented as usable from any queue;
/// objc2 does not mark them `Send`, so this wrapper carries them between threads.
#[derive(Clone)]
struct Shared<T>(T);
// SAFETY: see above; the wrapped objects are only read after they are created.
unsafe impl<T> Send for Shared<T> {}

/// The content ScreenCaptureKit lists, kept until the layout changes.
#[derive(Clone)]
struct Content {
    content: Retained<SCShareableContent>,
    /// Glimpsfy itself, so its own windows can be left out of a frame.
    own_app: Option<Retained<SCRunningApplication>>,
}

/// The cached content and the display layout it belongs to.
#[derive(Default)]
struct ContentCache {
    content: Option<Shared<Content>>,
    /// Counts the layout changes, so content fetched before one is not kept after it.
    layout: u64,
}

/// The displays to capture and Glimpsfy's own app, taken from the cached content.
type Targets = (Vec<Retained<SCDisplay>>, Option<Retained<SCRunningApplication>>);

/// The apps a clean desktop leaves out and the windows of theirs that show anyway.
type Exceptions = (Vec<Retained<SCRunningApplication>>, Vec<Retained<SCWindow>>);

/// The moment a capture (or a background fetch) gives up, shared by all of its requests.
#[derive(Clone, Copy)]
struct Deadline {
    at: Instant,
    limit: Duration,
}

impl Deadline {
    fn after(limit: Duration) -> Deadline {
        Deadline { at: Instant::now() + limit, limit }
    }

    fn timed_out(self) -> CaptureError {
        CaptureError::Timeout(self.limit.as_millis() as u64)
    }

    /// The next answer on `rx`, waited for until the deadline.
    fn wait<T>(self, rx: &mpsc::Receiver<T>) -> Result<T> {
        rx.recv_timeout(self.at.saturating_duration_since(Instant::now())).map_err(|error| match error {
            RecvTimeoutError::Timeout => self.timed_out(),
            RecvTimeoutError::Disconnected => {
                CaptureError::Backend("ScreenCaptureKit dropped a request without answering it".into())
            }
        })
    }
}

/// Keeps `report` for `take_report` on this thread.
fn keep_report(report: CaptureReport) {
    REPORT.with(|slot| *slot.borrow_mut() = Some(report));
}

pub struct MacBackend {
    content: Mutex<ContentCache>,
    /// `NSScreen.localizedName` by display id, kept until the layout changes.
    names: Mutex<HashMap<u32, String>>,
    /// Displays whose rectangle capture did not match their pixel size, kept until the layout
    /// changes, so they go straight to the filtered capture instead of paying for both.
    rect_refused: Mutex<Vec<u32>>,
}

impl MacBackend {
    pub fn new() -> Self {
        MacBackend {
            content: Mutex::new(ContentCache::default()),
            names: Mutex::new(HashMap::new()),
            rect_refused: Mutex::new(Vec::new()),
        }
    }

    /// Whether the displays can be taken with the rectangle capture: it needs macOS 15.2, it
    /// never draws the pointer, and it cannot leave windows out, so it is used only while none
    /// of Glimpsfy's windows shows on screen (the warm overlays are on screen but fully clear)
    /// and never for a clean desktop.
    fn rect_capture_fits(&self, displays: &[DisplayId], options: &CaptureOptions) -> bool {
        if options.shows_cursor || options.clean_desktop || !rect_capture_available() {
            return false;
        }
        // A rectangle comes in 8 bits with the highlights clipped; HDR content needs the filter.
        if displays.iter().any(|d| super::macos_hdr::headroom(d.0).is_some()) {
            return false;
        }
        let refused = self.rect_refused.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if displays.iter().any(|d| refused.contains(&d.0)) {
            return false;
        }
        drop(refused);
        !options.exclude_own_windows || !own_window_showing()
    }

    /// The screens' names, read on the main thread once per display layout.
    fn display_names(&self) -> HashMap<u32, String> {
        let mut names = self.names.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if names.is_empty() {
            if let Some(fresh) = screen_names() {
                *names = fresh;
            }
        }
        names.clone()
    }

    /// The cached content, if any.
    fn cached(&self) -> Option<Shared<Content>> {
        self.content.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).content.clone()
    }

    /// Fetches the content and keeps it, unless the layout changed meanwhile. The lock is not
    /// held while ScreenCaptureKit answers, so a slow answer never holds up the window list.
    fn refresh(&self, deadline: Deadline) -> Result<Shared<Content>> {
        let layout = self.content.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).layout;
        let content = Shared(fetch_content(deadline)?);
        let mut cache = self.content.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.layout == layout {
            cache.content = Some(content.clone());
        }
        Ok(content)
    }

    /// Looks `find` up in the cached content, and in a fresh one when nothing is cached or the
    /// cached one lacks it (a window opened or a display added since). A fetch adds its time to
    /// `fetched`.
    fn find_in_content<T>(
        &self,
        deadline: Deadline,
        fetched: &mut Option<Duration>,
        find: impl Fn(&Content) -> Option<T>,
    ) -> Result<Option<T>> {
        if let Some(Shared(content)) = self.cached() {
            if let Some(found) = find(&content) {
                return Ok(Some(found));
            }
        }
        let started = Instant::now();
        let fresh = self.refresh(deadline);
        *fetched = Some(fetched.unwrap_or_default() + started.elapsed());
        let Shared(content) = fresh?;
        Ok(find(&content))
    }

    /// A ScreenCaptureKit window from the content.
    fn window_for(
        &self,
        id: WindowId,
        deadline: Deadline,
        fetched: &mut Option<Duration>,
    ) -> Result<Shared<Retained<SCWindow>>> {
        self.find_in_content(deadline, fetched, |content| {
            unsafe { content.content.windows() }.iter().find(|w| unsafe { w.windowID() } == id.0).map(Shared)
        })?
        .ok_or(CaptureError::WindowNotFound(id))
    }

    /// Bundle ids by process, from the cached content only (never a fetch, so the window list
    /// stays fast); empty until the content is cached.
    fn bundle_ids(&self) -> HashMap<i32, String> {
        let cache = self.content.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(Shared(content)) = cache.content.as_ref() else {
            return HashMap::new();
        };
        unsafe { content.content.applications() }
            .iter()
            .map(|app| unsafe { (app.processID(), app.bundleIdentifier().to_string()) })
            .filter(|(_, id)| !id.is_empty())
            .collect()
    }

    /// The ScreenCaptureKit displays of `displays` and Glimpsfy's own app, from the content.
    fn content_for(
        &self,
        displays: &[DisplayId],
        deadline: Deadline,
        fetched: &mut Option<Duration>,
    ) -> Result<Shared<Targets>> {
        self.find_in_content(deadline, fetched, |content| {
            let all = unsafe { content.content.displays() };
            let found: Vec<Retained<SCDisplay>> =
                displays.iter().filter_map(|id| all.iter().find(|d| unsafe { d.displayID() } == id.0)).collect();
            (found.len() == displays.len()).then(|| Shared((found, content.own_app.clone())))
        })?
        .ok_or_else(|| CaptureError::DisplayNotFound(displays.first().copied().unwrap_or(DisplayId(0))))
    }

    /// What a still capture leaves out besides Glimpsfy (`FilterPlan::desktop_exceptions`):
    /// nothing unless `options` asks for a clean desktop. The content must list every Finder
    /// window on screen, or a window opened since it was cached would drop with the icons, so a
    /// missing one is fetched first; the check reads the window list, about a millisecond.
    fn desktop_exceptions(
        &self,
        options: &CaptureOptions,
        deadline: Deadline,
        fetched: &mut Option<Duration>,
    ) -> Result<Shared<Exceptions>> {
        if !options.clean_desktop {
            return Ok(Shared((Vec::new(), Vec::new())));
        }
        let content = match self.cached() {
            Some(Shared(content)) if !finder_window_unlisted(&content.content) => content,
            _ => {
                let started = Instant::now();
                let fresh = self.refresh(deadline);
                *fetched = Some(fetched.unwrap_or_default() + started.elapsed());
                fresh?.0
            }
        };
        let plan = FilterPlan::still(options.exclude_own_windows, true);
        let exceptions = plan.desktop_exceptions(&listed_windows(&content.content));
        Ok(Shared((
            apps_by_bundle(&content.content, &exceptions.excluded_bundles),
            windows_by_id(&content.content, &exceptions.excepted_windows),
        )))
    }
}

impl MacBackend {
    /// `capture_displays` until `deadline`, filling in `report` on the way.
    fn displays_until(
        &self,
        displays: &[DisplayId],
        options: &CaptureOptions,
        deadline: Deadline,
        report: &mut CaptureReport,
    ) -> Result<Vec<(DisplayId, Frame)>> {
        if !CGPreflightScreenCaptureAccess() {
            return Err(CaptureError::PermissionDenied);
        }
        let described = self.displays()?;
        if self.rect_capture_fits(displays, options) {
            let started = Instant::now();
            report.path = CapturePath::Rect;
            match capture_rects(displays, &described, deadline, &mut report.frames) {
                Ok(frames) => return Ok(frames),
                // The filtered capture would wait in the same queue, behind the requests just
                // made, so a late answer ends the capture instead of starting another one.
                Err(RectFailure::TimedOut) => return Err(deadline.timed_out()),
                Err(RectFailure::Refused(display)) => {
                    if let Some(display) = display {
                        self.rect_refused.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(display.0);
                    }
                    report.path = CapturePath::RectThenFiltered;
                    report.rect = Some(started.elapsed());
                    report.frames.iter_mut().for_each(|(_, arrived)| *arrived = None);
                }
            }
        }
        // First, so a fetch it needs leaves the fresh content in the cache for the displays.
        let Shared((desktop_apps, desktop_windows)) =
            self.desktop_exceptions(options, deadline, &mut report.content)?;
        let Shared((sc_displays, own_app)) = self.content_for(displays, deadline, &mut report.content)?;

        let requested = Instant::now();
        let (tx, rx) = mpsc::channel::<(usize, Result<(Frame, Option<f32>)>)>();
        for (index, (id, sc_display)) in displays.iter().zip(sc_displays.iter()).enumerate() {
            let info = described.iter().find(|d| d.id == *id).ok_or(CaptureError::DisplayNotFound(*id))?;
            let mut excluded: Vec<&SCRunningApplication> =
                if options.exclude_own_windows { own_app.iter().map(|a| &**a).collect() } else { Vec::new() };
            excluded.extend(desktop_apps.iter().map(|a| &**a));
            let excluded = NSArray::from_slice(&excluded);
            let excepted: Vec<&SCWindow> = desktop_windows.iter().map(|w| &**w).collect();
            let excepted = NSArray::from_slice(&excepted);
            // SAFETY: a fresh filter from a display of the current content.
            let filter = unsafe {
                SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                    SCContentFilter::alloc(),
                    sc_display,
                    &excluded,
                    &excepted,
                )
            };
            let headroom = super::macos_hdr::headroom(id.0);
            let config = match headroom {
                Some(_) => super::macos_hdr::hdr_configuration(info.pixel_size, options.shows_cursor),
                None => stream_configuration(info.pixel_size, info.color_space, options.shows_cursor),
            };
            let tx = tx.clone();
            let headroom = headroom.unwrap_or(1.0);
            screenshot(&filter, &config, info.color_space, info.scale, false, headroom, move |result| {
                let _ = tx.send((index, result));
            });
        }
        drop(tx);

        let mut frames: Vec<Option<Frame>> = displays.iter().map(|_| None).collect();
        for _ in 0..displays.len() {
            let (index, frame) = deadline.wait(&rx)?;
            report.frames[index].1 = Some(requested.elapsed());
            let (frame, tone_mapped) = frame?;
            if let Some(headroom) = tone_mapped {
                report.tone_mapped = Some(report.tone_mapped.map_or(headroom, |kept| kept.max(headroom)));
            }
            frames[index] = Some(frame);
        }
        displays
            .iter()
            .zip(frames)
            .map(|(id, frame)| frame.map(|f| (*id, f)).ok_or(CaptureError::DisplayNotFound(*id)))
            .collect()
    }

    /// `capture_window` until `deadline`, filling in `report` on the way.
    fn window_until(
        &self,
        window: WindowId,
        options: &WindowCaptureOptions,
        deadline: Deadline,
        report: &mut CaptureReport,
    ) -> Result<Frame> {
        if !CGPreflightScreenCaptureAccess() {
            return Err(CaptureError::PermissionDenied);
        }
        let Shared(sc_window) = self.window_for(window, deadline, &mut report.content)?;
        let described = self.displays()?;
        let bounds = unsafe { sc_window.frame() };
        let bounds = LogicalRect::new(bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height);
        // The window's colour space and scale are those of the display that shows most of it.
        let display = described
            .iter()
            .filter_map(|d| d.frame.intersection(&bounds).map(|common| (d, common.width * common.height)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(d, _)| d)
            .or(described.first())
            .ok_or(CaptureError::WindowNotFound(window))?;
        report.frames.push((display.id, None));

        // SAFETY: a fresh filter from a window of the current content.
        let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &sc_window) };
        // The filter knows the window's size and pixel scale (macOS 14), without its shadow.
        let (rect, pixel_scale) = unsafe { (filter.contentRect(), filter.pointPixelScale() as f64) };
        let scale = if pixel_scale > 0.0 { pixel_scale } else { display.scale.0 };
        // ScreenCaptureKit draws the shadow too and shrinks window and shadow together into the
        // image's size, so an image of the window's own size holds a window about 9 % too small
        // and blurred. The image gets room for the shadow instead, the window stays at its own
        // pixels, and the empty margin is cut away afterwards.
        let margin = if options.include_shadow { SHADOW_MARGIN } else { 0.0 };
        let size = PhysicalSize::new(
            ((rect.size.width + 2.0 * margin) * scale).round().max(1.0) as u32,
            ((rect.size.height + 2.0 * margin) * scale).round().max(1.0) as u32,
        );
        let headroom = super::macos_hdr::headroom(display.id.0);
        let config = match headroom {
            Some(_) => super::macos_hdr::hdr_configuration(size, options.shows_cursor),
            None => stream_configuration(size, display.color_space, options.shows_cursor),
        };
        // SAFETY: plain property setters on a configuration this function owns; the one added
        // after macOS 14.0 is guarded.
        unsafe {
            config.setIgnoreShadowsSingleWindow(!options.include_shadow);
            // Not the system's own opaque backing: it is white and fills the corners and the
            // shadow too, where `finish_window` fills the body alone.
            config.setShouldBeOpaque(false);
            config.setIgnoreGlobalClipSingleWindow(true);
            config.setPreservesAspectRatio(true);
            if config.respondsToSelector(sel!(setIncludeChildWindows:)) {
                config.setIncludeChildWindows(true);
            }
        }

        let requested = Instant::now();
        let (tx, rx) = mpsc::sync_channel::<Result<(Frame, Option<f32>)>>(1);
        let headroom = headroom.unwrap_or(1.0);
        screenshot(&filter, &config, display.color_space, ScaleFactor(scale), true, headroom, move |result| {
            let _ = tx.send(result);
        });
        let (frame, tone_mapped) = deadline.wait(&rx)??;
        report.tone_mapped = tone_mapped;
        if let Some((_, arrived)) = report.frames.last_mut() {
            *arrived = Some(requested.elapsed());
        }
        let frame = if margin > 0.0 { crop_to_visible(frame)? } else { frame };
        // With the shadow the image holds more than the window, whose size the filter gave;
        // without it the image is the window.
        let body = (margin > 0.0).then(|| {
            PhysicalSize::new(
                (rect.size.width * scale).round().max(1.0) as u32,
                (rect.size.height * scale).round().max(1.0) as u32,
            )
        });
        let fill = if options.opaque_body { window_fill(display.color_space) } else { None };
        Ok(finish_window(frame, BodyHint { size: body, radius: None, fill }, options))
    }
}

impl Default for MacBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for MacBackend {
    fn displays(&self) -> Result<Vec<Display>> {
        let mut ids = [0 as CGDirectDisplayID; 32];
        let mut count = 0u32;
        // SAFETY: the buffer holds 32 ids and the count is written by the call.
        let error = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
        if error.0 != 0 {
            return Err(CaptureError::Backend(format!("CGGetActiveDisplayList failed with {}", error.0)));
        }
        let main = CGMainDisplayID();
        let names = self.display_names();
        let mut displays: Vec<Display> =
            ids[..count as usize].iter().map(|&id| describe_display(id, id == main, names.get(&id).cloned())).collect();
        displays.sort_by_key(|d| !d.is_primary);
        Ok(displays)
    }

    fn windows(&self, options: WindowListOptions) -> Result<Vec<WindowInfo>> {
        let mut flags = CGWindowListOption::empty();
        if options.on_screen_only {
            flags |= CGWindowListOption::OptionOnScreenOnly;
        }
        if options.exclude_desktop {
            flags |= CGWindowListOption::ExcludeDesktopElements;
        }
        let Some(list) = CGWindowListCopyWindowInfo(flags, kCGNullWindowID) else {
            return Ok(Vec::new());
        };
        // SAFETY: a CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
        let list: &NSArray<NSDictionary<NSString, AnyObject>> =
            unsafe { &*(CFRetained::as_ptr(&list).as_ptr().cast()) };
        let own = std::process::id() as i32;
        let bundle_ids = self.bundle_ids();
        let mut windows = Vec::new();
        for dict in list.iter() {
            let pid = number(&dict, unsafe { kCGWindowOwnerPID }).map(|n| n.as_i32()).unwrap_or(0);
            if options.exclude_own && pid == own {
                continue;
            }
            let Some(id) = number(&dict, unsafe { kCGWindowNumber }).map(|n| n.as_u32()) else { continue };
            let bounds = rect(&dict, unsafe { kCGWindowBounds }).unwrap_or_default();
            windows.push(WindowInfo {
                id: WindowId(id),
                title: string(&dict, unsafe { kCGWindowName }).filter(|t| !t.is_empty()),
                app_name: string(&dict, unsafe { kCGWindowOwnerName }).unwrap_or_default(),
                bundle_id: bundle_ids.get(&pid).cloned(),
                pid,
                bounds: LogicalRect::new(bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height),
                layer: number(&dict, unsafe { kCGWindowLayer }).map(|n| n.as_i32()).unwrap_or(0),
                z_index: windows.len() as u32,
                alpha: number(&dict, unsafe { kCGWindowAlpha }).map(|n| n.as_f64()).unwrap_or(1.0),
                is_on_screen: number(&dict, unsafe { kCGWindowIsOnscreen }).is_some_and(|n| n.as_bool()),
                corner_radius: None,
                capture_blocked: number(&dict, unsafe { kCGWindowSharingState })
                    .is_some_and(|n| n.as_u32() == CGWindowSharingType::None.0),
            });
        }
        Ok(windows)
    }

    fn capture_displays(&self, displays: &[DisplayId], options: &CaptureOptions) -> Result<Vec<(DisplayId, Frame)>> {
        let started = Instant::now();
        let mut report = CaptureReport {
            path: CapturePath::Filtered,
            rect: None,
            content: None,
            frames: displays.iter().map(|id| (*id, None)).collect(),
            total: Duration::ZERO,
            tone_mapped: None,
        };
        let result = self.displays_until(displays, options, Deadline::after(DEADLINE), &mut report);
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
            tone_mapped: None,
        };
        let result = self.window_until(window, options, Deadline::after(DEADLINE), &mut report);
        report.total = started.elapsed();
        keep_report(report);
        result
    }

    fn permission(&self) -> PermissionState {
        if CGPreflightScreenCaptureAccess() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }

    fn request_permission(&self) -> PermissionState {
        if CGRequestScreenCaptureAccess() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }

    fn invalidate(&self) {
        {
            let mut cache = self.content.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.content = None;
            cache.layout += 1;
        }
        self.names.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
        self.rect_refused.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }

    fn prepare(&self) {
        super::macos_hdr::prepare();
        // The names need no permission; reading them now keeps the main-thread hop out of the
        // first capture.
        let displays = self.displays().unwrap_or_default();
        if CGPreflightScreenCaptureAccess() {
            let ids: Vec<DisplayId> = displays.iter().map(|d| d.id).collect();
            let _ = self.content_for(&ids, Deadline::after(PREPARE_TIMEOUT), &mut None);
        }
    }

    fn take_report(&self) -> Option<CaptureReport> {
        REPORT.with(|slot| slot.borrow_mut().take())
    }

    fn open_stream(&self, config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
        super::macos_stream::open(config, sink)
    }
}

fn describe_display(id: CGDirectDisplayID, is_primary: bool, name: Option<String>) -> Display {
    let bounds = CGDisplayBounds(id);
    let mode: Option<CFRetained<CGDisplayMode>> = CGDisplayCopyDisplayMode(id);
    let (pixel_width, pixel_height, refresh) = match &mode {
        Some(mode) => (
            CGDisplayMode::pixel_width(Some(mode)) as u32,
            CGDisplayMode::pixel_height(Some(mode)) as u32,
            CGDisplayMode::refresh_rate(Some(mode)),
        ),
        None => (bounds.size.width as u32, bounds.size.height as u32, 0.0),
    };
    let wide = CGDisplayCopyColorSpace(id).is_wide_gamut_rgb();
    let is_builtin = CGDisplayIsBuiltin(id);
    Display {
        id: DisplayId(id),
        name: name.filter(|n| !n.is_empty()).unwrap_or_else(|| {
            if is_builtin {
                "Built-in Display".into()
            } else {
                format!("Display {id}")
            }
        }),
        frame: LogicalRect::new(bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height),
        scale: ScaleFactor(if bounds.size.width > 0.0 { pixel_width as f64 / bounds.size.width } else { 1.0 }),
        pixel_size: PhysicalSize::new(pixel_width, pixel_height),
        is_primary,
        is_builtin,
        color_space: if wide { ColorSpace::DisplayP3 } else { ColorSpace::Srgb },
        refresh_rate: (refresh > 0.0).then_some(refresh),
    }
}

fn stream_configuration(size: PhysicalSize, space: ColorSpace, shows_cursor: bool) -> Retained<SCStreamConfiguration> {
    // SAFETY: plain property setters on a configuration this function owns.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(size.width as usize);
        config.setHeight(size.height as usize);
        config.setPixelFormat(PIXEL_FORMAT_BGRA);
        config.setColorSpaceName(color_space_name(space));
        config.setShowsCursor(shows_cursor);
        config.setCaptureResolution(SCCaptureResolutionType::Best);
        config.setScalesToFit(false);
        config
    }
}

fn color_space_name(space: ColorSpace) -> &'static CFString {
    // SAFETY: Core Graphics' constant strings live for the whole process.
    unsafe {
        match space {
            ColorSpace::Srgb => kCGColorSpaceSRGB,
            ColorSpace::DisplayP3 => kCGColorSpaceDisplayP3,
        }
    }
}

/// The names AppKit gives the screens ("Built-in Retina Display", "LG HDR 4K"), by display id.
/// `None` when the main thread did not answer in time.
fn screen_names() -> Option<HashMap<u32, String>> {
    fn read(mtm: MainThreadMarker) -> HashMap<u32, String> {
        let key = NSString::from_str("NSScreenNumber");
        NSScreen::screens(mtm)
            .iter()
            .filter_map(|screen| {
                let number = screen.deviceDescription().objectForKey(&key)?.downcast::<NSNumber>().ok()?;
                Some((number.unsignedIntValue(), screen.localizedName().to_string()))
            })
            .collect()
    }
    if let Some(mtm) = MainThreadMarker::new() {
        return Some(read(mtm));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    DispatchQueue::main().exec_async(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            let _ = tx.send(read(mtm));
        }
    });
    rx.recv_timeout(NAMES_TIMEOUT).ok()
}

/// `NSColor.windowBackgroundColor` in the system's appearance (light or dark) and in `space`,
/// as `[r, g, b]`: the colour a window shows where nothing is drawn, which fills its translucent
/// parts. Only the main thread may resolve it; `None` when the main thread did not answer in
/// time.
fn window_fill(space: ColorSpace) -> Option<[u8; 3]> {
    fn read(mtm: MainThreadMarker, space: ColorSpace) -> Option<[u8; 3]> {
        let target = match space {
            ColorSpace::Srgb => NSColorSpace::sRGBColorSpace(),
            ColorSpace::DisplayP3 => NSColorSpace::displayP3ColorSpace(),
        };
        let colour = std::cell::Cell::new(None);
        // A dynamic colour takes the value of the appearance that is current while it is read.
        let block = RcBlock::new(|| {
            colour.set(NSColor::windowBackgroundColor().colorUsingColorSpace(&target).map(|c| {
                [c.redComponent(), c.greenComponent(), c.blueComponent()]
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            }));
        });
        NSApplication::sharedApplication(mtm).effectiveAppearance().performAsCurrentDrawingAppearance(&block);
        colour.get()
    }
    if let Some(mtm) = MainThreadMarker::new() {
        return read(mtm, space);
    }
    let (tx, rx) = mpsc::sync_channel(1);
    DispatchQueue::main().exec_async(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            let _ = tx.send(read(mtm, space));
        }
    });
    rx.recv_timeout(FILL_TIMEOUT).ok().flatten()
}

/// Asks ScreenCaptureKit for one still; `done` gets the frame (or the error) and the headroom
/// it was tone mapped from. An 8-bit image is read on ScreenCaptureKit's queue; a half-float one
/// (an HDR request, `macos_hdr`) is tone mapped on a thread of its own, so the queue's next
/// answer does not wait for it. The image decides, not the request, because a Mac that ignores
/// the HDR request answers in 8 bits. `alpha` keeps real transparency (a window); without it the
/// frame is made fully opaque, whatever the alpha bytes held. `headroom` is the display's.
fn screenshot(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
    space: ColorSpace,
    scale: ScaleFactor,
    alpha: bool,
    headroom: f32,
    done: impl Fn(Result<(Frame, Option<f32>)>) + Send + Sync + 'static,
) {
    let done = std::sync::Arc::new(done);
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let Some(image) = NonNull::new(image) else {
            done(Err(sc_error(error)));
            return;
        };
        // SAFETY: ScreenCaptureKit hands over a valid image for the duration of the call.
        let image_ref = unsafe { image.as_ref() };
        if !super::macos_hdr::is_half_float(image_ref) {
            done(frame_from_image(image_ref, space, scale, alpha).map(|frame| (frame, None)));
            return;
        }
        // SAFETY: retained, so the image outlives the call on the thread that tone maps it.
        let image = unsafe { CFRetained::retain(image) };
        let done = done.clone();
        std::thread::spawn(move || done(super::macos_hdr::tone_mapped_frame(&image, space, scale, headroom, alpha)));
    });
    // SAFETY: filter and configuration live until the call returns, and the handler is retained
    // by ScreenCaptureKit until it runs.
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(filter, config, Some(&handler));
    }
}

/// Whether `SCScreenshotManager` has `captureImageInRect:completionHandler:` (macOS 15.2).
fn rect_capture_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        SCScreenshotManager::class().metaclass().responds_to(sel!(captureImageInRect:completionHandler:))
    })
}

/// Every window of `content` with its owner's bundle and its level, for
/// `FilterPlan::desktop_exceptions`.
pub(super) fn listed_windows(content: &SCShareableContent) -> Vec<ListedWindow> {
    // SAFETY: reading the immutable lists and properties of fetched content.
    unsafe { content.windows() }
        .iter()
        .map(|window| unsafe {
            ListedWindow {
                id: WindowId(window.windowID()),
                bundle_id: window.owningApplication().map(|app| app.bundleIdentifier().to_string()),
                layer: window.windowLayer().clamp(i32::MIN as isize, i32::MAX as isize) as i32,
            }
        })
        .collect()
}

/// The running apps of `content` whose bundle is one of `bundles`.
pub(super) fn apps_by_bundle(content: &SCShareableContent, bundles: &[&str]) -> Vec<Retained<SCRunningApplication>> {
    if bundles.is_empty() {
        return Vec::new();
    }
    // SAFETY: reading the immutable lists and properties of fetched content.
    unsafe { content.applications() }
        .iter()
        .filter(|app| bundles.contains(&unsafe { app.bundleIdentifier() }.to_string().as_str()))
        .collect()
}

/// The windows of `content` among `ids`.
pub(super) fn windows_by_id(content: &SCShareableContent, ids: &[WindowId]) -> Vec<Retained<SCWindow>> {
    if ids.is_empty() {
        return Vec::new();
    }
    // SAFETY: reading the immutable lists of fetched content.
    unsafe { content.windows() }.iter().filter(|w| ids.contains(&WindowId(unsafe { w.windowID() }))).collect()
}

/// The processes of Finder in `content`.
pub(super) fn finder_pids(content: &SCShareableContent) -> Vec<i32> {
    // SAFETY: reading the immutable lists and properties of fetched content.
    unsafe { content.applications() }
        .iter()
        .filter(|app| unsafe { app.bundleIdentifier() }.to_string() == FINDER)
        .map(|app| unsafe { app.processID() })
        .collect()
}

/// The windows at level 0 or above that the processes `pids` show on screen, from the window
/// list (about a millisecond): the Finder windows a clean desktop must keep. A fully clear window
/// and one that keeps itself out of captures are skipped: ScreenCaptureKit may never list them,
/// and a window it never lists would fetch the content again on every capture.
pub(super) fn windows_on_screen(pids: &[i32]) -> Vec<WindowId> {
    if pids.is_empty() {
        return Vec::new();
    }
    let flags = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(flags, kCGNullWindowID) else {
        return Vec::new();
    };
    // SAFETY: a CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(CFRetained::as_ptr(&list).as_ptr().cast()) };
    list.iter()
        .filter(|dict| {
            number(dict, unsafe { kCGWindowOwnerPID }).is_some_and(|n| pids.contains(&n.as_i32()))
                && number(dict, unsafe { kCGWindowLayer }).is_some_and(|n| n.as_i32() >= 0)
                && number(dict, unsafe { kCGWindowAlpha }).map_or(1.0, |n| n.as_f64()) > 0.0
                && number(dict, unsafe { kCGWindowSharingState })
                    .is_none_or(|n| n.as_u32() != CGWindowSharingType::None.0)
        })
        .filter_map(|dict| number(&dict, unsafe { kCGWindowNumber }).map(|n| WindowId(n.as_u32())))
        .collect()
}

/// Whether Finder shows a window on screen that `content` does not list (opened, or brought
/// in from another Space, since it was fetched).
fn finder_window_unlisted(content: &SCShareableContent) -> bool {
    let shown = windows_on_screen(&finder_pids(content));
    if shown.is_empty() {
        return false;
    }
    // SAFETY: reading the immutable lists of fetched content.
    let listed: Vec<WindowId> =
        unsafe { content.windows() }.iter().map(|w| WindowId(unsafe { w.windowID() })).collect();
    shown.iter().any(|id| !listed.contains(id))
}

/// Whether one of Glimpsfy's windows is on screen and not fully clear (a toast, a pin, the
/// settings). The window list costs about a millisecond once the process has read it before.
fn own_window_showing() -> bool {
    let flags = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(flags, kCGNullWindowID) else {
        return true;
    };
    // SAFETY: a CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(CFRetained::as_ptr(&list).as_ptr().cast()) };
    let own = std::process::id() as i32;
    list.iter().any(|dict| {
        number(&dict, unsafe { kCGWindowOwnerPID }).is_some_and(|n| n.as_i32() == own)
            && number(&dict, unsafe { kCGWindowAlpha }).map_or(1.0, |n| n.as_f64()) > 0.0
    })
}

/// Why the rectangle capture gave up.
enum RectFailure {
    /// ScreenCaptureKit refused a display; it is named when it will never fit (its image does
    /// not have the display's pixel size), so it is not tried again until the layout changes.
    Refused(Option<DisplayId>),
    /// Not every answer came before the deadline.
    TimedOut,
}

/// Takes each display with `captureImageInRect:`, which composites the screen as it is shown
/// instead of rebuilding it without some windows. ScreenCaptureKit answers the requests one after
/// another either way, but a rectangle takes 15 to 20 ms (35 to 50 ms for the first), where a
/// filtered display takes 60 to 75 ms. The image comes in the display's own colour space and is
/// converted to the frame's explicit one (1 to 4 ms) on a thread of its own, so the frame holds
/// the same values as one from the filtered path. `arrivals` gets each display's answer time.
fn capture_rects(
    displays: &[DisplayId],
    described: &[Display],
    deadline: Deadline,
    arrivals: &mut [(DisplayId, Option<Duration>)],
) -> std::result::Result<Vec<(DisplayId, Frame)>, RectFailure> {
    let requested = Instant::now();
    let (tx, rx) = mpsc::channel::<(usize, std::result::Result<Frame, RectFailure>)>();
    for (index, id) in displays.iter().enumerate() {
        let info = described.iter().find(|d| d.id == *id).ok_or(RectFailure::Refused(None))?.clone();
        let tx = tx.clone();
        let handler = RcBlock::new(move |image: *mut CGImage, _error: *mut NSError| {
            // SAFETY: ScreenCaptureKit hands over a valid image for the call; it is retained so
            // the conversion can run on another thread.
            let Some(image) = NonNull::new(image).map(|image| Shared(unsafe { CFRetained::retain(image) })) else {
                let _ = tx.send((index, Err(RectFailure::Refused(None))));
                return;
            };
            let tx = tx.clone();
            let info = info.clone();
            std::thread::spawn(move || {
                let Shared(image) = image;
                let width = CGImage::width(Some(&image)) as u32;
                let height = CGImage::height(Some(&image)) as u32;
                let result = if width != info.pixel_size.width || height != info.pixel_size.height {
                    Err(RectFailure::Refused(Some(info.id)))
                } else {
                    converted_frame(&image, info.color_space, info.scale).map_err(|_| RectFailure::Refused(None))
                };
                let _ = tx.send((index, result));
            });
        });
        let f = info_frame(described, *id).ok_or(RectFailure::Refused(None))?;
        // SAFETY: the handler is retained by ScreenCaptureKit until it runs.
        unsafe { SCScreenshotManager::captureImageInRect_completionHandler(f, Some(&handler)) };
    }
    drop(tx);

    let mut frames: Vec<Option<Frame>> = displays.iter().map(|_| None).collect();
    for _ in 0..displays.len() {
        let (index, frame) = match deadline.wait(&rx) {
            Ok(answer) => answer,
            Err(CaptureError::Timeout(_)) => return Err(RectFailure::TimedOut),
            Err(_) => return Err(RectFailure::Refused(None)),
        };
        if let Some((_, arrived)) = arrivals.get_mut(index) {
            *arrived = Some(requested.elapsed());
        }
        frames[index] = Some(frame?);
    }
    Ok(displays.iter().zip(frames).filter_map(|(id, frame)| frame.map(|f| (*id, f))).collect())
}

fn info_frame(described: &[Display], id: DisplayId) -> Option<CGRect> {
    let d = described.iter().find(|d| d.id == id)?;
    Some(CGRect::new(
        objc2_core_foundation::CGPoint::new(d.frame.x, d.frame.y),
        objc2_core_foundation::CGSize::new(d.frame.width, d.frame.height),
    ))
}

/// Converts a screen image from the display's colour space into `space`, as 32-bit BGRA with
/// premultiplied (here always opaque) alpha, the layout the filtered capture delivers.
pub(super) fn converted_frame(image: &CGImage, space: ColorSpace, scale: ScaleFactor) -> Result<Frame> {
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    let provider = CGImage::data_provider(Some(image)).ok_or_else(|| CaptureError::Backend("no pixel data".into()))?;
    let data = CGDataProvider::data(Some(&provider)).ok_or_else(|| CaptureError::Backend("no pixel data".into()))?;
    let source_space =
        CGImage::color_space(Some(image)).ok_or_else(|| CaptureError::Backend("no colour space".into()))?;
    let target_space = CGColorSpace::with_name(Some(color_space_name(space)))
        .ok_or_else(|| CaptureError::Backend("no target colour space".into()))?;
    let bgra = CGBitmapInfo(CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0);
    let source = vimage::Format {
        bits_per_component: CGImage::bits_per_component(Some(image)) as u32,
        bits_per_pixel: CGImage::bits_per_pixel(Some(image)) as u32,
        color_space: CFRetained::as_ptr(&source_space).as_ptr(),
        bitmap_info: CGImage::bitmap_info(Some(image)).0,
        version: 0,
        decode: std::ptr::null(),
        rendering_intent: CGImage::rendering_intent(Some(image)).0,
    };
    let target = vimage::Format {
        bits_per_component: 8,
        bits_per_pixel: 32,
        color_space: CFRetained::as_ptr(&target_space).as_ptr(),
        bitmap_info: bgra.0,
        version: 0,
        decode: std::ptr::null(),
        rendering_intent: 0,
    };
    let bytes_per_row = width * 4;
    let mut bytes = vec![0u8; bytes_per_row * height];
    // SAFETY: the source buffer is the image's own bytes, alive while `data` is; the target is
    // a buffer of exactly `height` rows of `bytes_per_row`. vImage reads one and writes the other.
    unsafe {
        let source_bytes = data.as_bytes_unchecked();
        let from = vimage::Buffer {
            data: source_bytes.as_ptr() as *mut std::ffi::c_void,
            height,
            width,
            row_bytes: CGImage::bytes_per_row(Some(image)),
        };
        let to = vimage::Buffer { data: bytes.as_mut_ptr().cast(), height, width, row_bytes: bytes_per_row };
        let mut error = 0isize;
        let converter =
            vimage::vImageConverter_CreateWithCGImageFormat(&source, &target, std::ptr::null(), 0, &mut error);
        if converter.is_null() || error != 0 {
            return Err(CaptureError::Backend(format!("vImage could not make a converter ({error})")));
        }
        let error = vimage::vImageConvert_AnyToAny(converter, &from, &to, std::ptr::null_mut(), 0);
        vimage::vImageConverter_Release(converter);
        if error != 0 {
            return Err(CaptureError::Backend(format!("vImage could not convert the capture ({error})")));
        }
    }
    let color = ColorInfo {
        space,
        icc_profile: match space {
            ColorSpace::DisplayP3 => display_p3_profile(),
            ColorSpace::Srgb => None,
        },
    };
    Frame::new(PhysicalSize::new(width as u32, height as u32), bytes_per_row, PixelFormat::Bgra8, color, scale, bytes)
}

/// The few vImage calls the colour conversion needs (Accelerate has no Rust bindings here).
#[allow(non_snake_case)]
mod vimage {
    use std::ffi::c_void;

    use objc2_core_graphics::CGColorSpace;

    /// `vImage_CGImageFormat`.
    #[repr(C)]
    pub struct Format {
        pub bits_per_component: u32,
        pub bits_per_pixel: u32,
        pub color_space: *const CGColorSpace,
        pub bitmap_info: u32,
        pub version: u32,
        pub decode: *const f64,
        pub rendering_intent: i32,
    }

    /// `vImage_Buffer`.
    #[repr(C)]
    pub struct Buffer {
        pub data: *mut c_void,
        pub height: usize,
        pub width: usize,
        pub row_bytes: usize,
    }

    #[link(name = "Accelerate", kind = "framework")]
    extern "C" {
        pub fn vImageConverter_CreateWithCGImageFormat(
            source: *const Format,
            target: *const Format,
            background: *const f64,
            flags: u32,
            error: *mut isize,
        ) -> *mut c_void;
        pub fn vImageConvert_AnyToAny(
            converter: *mut c_void,
            sources: *const Buffer,
            targets: *const Buffer,
            temp: *mut c_void,
            flags: u32,
        ) -> isize;
        pub fn vImageConverter_Release(converter: *mut c_void);
    }
}

/// The frame cut to the pixels that are not fully transparent (a window and its shadow), or as
/// it is when every pixel is.
fn crop_to_visible(frame: Frame) -> Result<Frame> {
    let (w, h) = (frame.size.width, frame.size.height);
    let row = frame.bytes_per_row;
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..h {
        let line = &frame.data[y as usize * row..y as usize * row + w as usize * 4];
        let mut first = None;
        let mut last = 0;
        for (x, p) in line.chunks_exact(4).enumerate() {
            if p[3] != 0 {
                first.get_or_insert(x as u32);
                last = x as u32;
            }
        }
        if let Some(first) = first {
            left = left.min(first);
            right = right.max(last);
            top = top.min(y);
            bottom = y;
        }
    }
    if left == u32::MAX {
        return Ok(frame);
    }
    frame.crop(PhysicalRect { x: left as i32, y: top as i32, width: right - left + 1, height: bottom - top + 1 })
}

/// The ICC profile of Display P3, read from Core Graphics once.
pub(crate) fn display_p3_profile() -> Option<std::sync::Arc<[u8]>> {
    static PROFILE: OnceLock<Option<std::sync::Arc<[u8]>>> = OnceLock::new();
    PROFILE
        .get_or_init(|| {
            let space = CGColorSpace::with_name(Some(color_space_name(ColorSpace::DisplayP3)))?;
            let data: CFRetained<CFData> = CGColorSpace::icc_data(Some(&space))?;
            Some(data.to_vec().into())
        })
        .clone()
}

/// Copies a ScreenCaptureKit image into a frame. The rows keep their padding; the bytes are
/// copied once, out of the image's own buffer. Premultiplied alpha is divided out (frames carry
/// straight alpha), and an image without alpha, or a screen (`alpha` false), is made opaque.
fn frame_from_image(image: &CGImage, space: ColorSpace, scale: ScaleFactor, alpha: bool) -> Result<Frame> {
    let width = CGImage::width(Some(image)) as u32;
    let height = CGImage::height(Some(image)) as u32;
    let bytes_per_row = CGImage::bytes_per_row(Some(image));
    if CGImage::bits_per_pixel(Some(image)) != 32 {
        return Err(CaptureError::Backend("the capture is not 32 bits per pixel".into()));
    }
    let provider = CGImage::data_provider(Some(image)).ok_or_else(|| CaptureError::Backend("no pixel data".into()))?;
    let data = CGDataProvider::data(Some(&provider)).ok_or_else(|| CaptureError::Backend("no pixel data".into()))?;
    // SAFETY: the bytes are read while `data` is alive and nothing mutates it.
    let mut bytes = unsafe { data.as_bytes_unchecked() }.to_vec();
    let info = CGImage::alpha_info(Some(image));
    let has_alpha =
        !matches!(info, CGImageAlphaInfo::None | CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast);
    let premultiplied = matches!(info, CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::PremultipliedLast);
    // A screen with an alpha channel is opaque already, so its frame skips this pass and the
    // latency it would cost.
    let force_opaque = !has_alpha;
    let unpremultiply = alpha && has_alpha && premultiplied;
    if force_opaque || unpremultiply {
        let row = width as usize * 4;
        for y in 0..height as usize {
            let Some(line) = bytes.get_mut(y * bytes_per_row..y * bytes_per_row + row) else { break };
            for p in line.chunks_exact_mut(4) {
                if force_opaque {
                    p[3] = 255;
                } else if p[3] != 255 {
                    let a = p[3] as u32;
                    for c in &mut p[..3] {
                        *c = (*c as u32 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
                    }
                }
            }
        }
    }
    let color = ColorInfo {
        space,
        icc_profile: match space {
            ColorSpace::DisplayP3 => display_p3_profile(),
            ColorSpace::Srgb => None,
        },
    };
    Frame::new(PhysicalSize::new(width, height), bytes_per_row, PixelFormat::Bgra8, color, scale, bytes)
}

fn fetch_content(deadline: Deadline) -> Result<Content> {
    let (tx, rx) = mpsc::sync_channel::<Result<Shared<Retained<SCShareableContent>>>>(1);
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: the content, when there is one, is valid for the call and retained here.
        let result = match unsafe { Retained::retain(content) } {
            Some(content) => Ok(Shared(content)),
            None => Err(sc_error(error)),
        };
        let _ = tx.send(result);
    });
    // SAFETY: the handler is retained by ScreenCaptureKit until it runs.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            false, true, &handler,
        );
    }
    let Shared(content) = deadline.wait(&rx)??;
    let own = std::process::id() as i32;
    let own_app = unsafe { content.applications() }.iter().find(|app| unsafe { app.processID() } == own);
    Ok(Content { content, own_app })
}

/// ScreenCaptureKit's error, or a generic one when it gave none.
fn sc_error(error: *mut NSError) -> CaptureError {
    // SAFETY: the error, when there is one, is valid for the call.
    let Some(error) = (unsafe { error.as_ref() }) else {
        return CaptureError::Backend("ScreenCaptureKit returned nothing".into());
    };
    if error.code() == SCStreamErrorCode::UserDeclined.0 {
        return CaptureError::PermissionDenied;
    }
    CaptureError::Backend(error.localizedDescription().to_string())
}

fn key(key: &CFString) -> &NSString {
    // SAFETY: CFString is toll-free bridged to NSString.
    unsafe { &*(key as *const CFString).cast::<NSString>() }
}

fn number(dict: &NSDictionary<NSString, AnyObject>, k: &CFString) -> Option<Retained<NSNumber>> {
    dict.objectForKey(key(k))?.downcast::<NSNumber>().ok()
}

fn string(dict: &NSDictionary<NSString, AnyObject>, k: &CFString) -> Option<String> {
    dict.objectForKey(key(k))?.downcast::<NSString>().ok().map(|s| s.to_string())
}

fn rect(dict: &NSDictionary<NSString, AnyObject>, k: &CFString) -> Option<CGRect> {
    let value = dict.objectForKey(key(k))?;
    // SAFETY: kCGWindowBounds holds a CFDictionary, toll-free bridged with the object read here.
    let value: &CFDictionary = unsafe { &*(Retained::as_ptr(&value).cast::<CFDictionary>()) };
    let mut out = CGRect::default();
    // SAFETY: `out` is a valid place for the result.
    unsafe { CGRectMakeWithDictionaryRepresentation(Some(value), &mut out) }.then_some(out)
}
