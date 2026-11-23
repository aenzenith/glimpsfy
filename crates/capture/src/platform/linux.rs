//! The Linux backend: X11 and Wayland behind one `CaptureBackend`.
//!
//! **Geometry always comes from the X server, pixels from X or from the portal.** The app runs
//! GTK on its X11 backend in every session (natively, or through XWayland with
//! `GDK_BACKEND=x11`), because only X lets an app place its windows, keep them on top and make
//! them click-through. So the displays are RandR's, and the logical space is GTK's: X root
//! coordinates divided by the one integer window scale GTK uses for the screen (`GDK_SCALE`, else
//! XSETTINGS `Gdk/WindowScalingFactor`, else 1). The origin is the X screen's origin, which is
//! where GTK's window coordinates start, whether or not the primary display lies there.
//!
//! - On an **X11 session** the pixels are grabbed from the root window: one grab covers every
//!   display asked for, so they show one moment. X cannot leave a window out of a grab, so the
//!   app hides its own windows before it captures, and `exclude_own_windows` and a stream's
//!   `excluded_windows` do nothing here. Windows come from the window manager's EWMH properties,
//!   and one window's picture from the Composite extension when a compositing manager runs.
//! - On a **Wayland session** stills come from the Screenshot portal and recordings from the
//!   ScreenCast portal. The portal's picture of the desktop may have another resolution than
//!   XWayland's screen (fractional scaling), so it is split into displays in proportion to the X
//!   layout and every display's `scale` and `pixel_size` are the measured ones. `displays()`
//!   reports the X layout's pixels until the first portal picture has been seen, and the
//!   measured ones from then on (for as long as the X screen keeps its size; `invalidate`
//!   forgets them). Wayland lists no windows of other apps: `windows()` is empty and
//!   `capture_window` is an error. A Wayland session without XWayland is not supported, and every
//!   method says so.
//!
//! Everything here blocks and is called from worker threads, never from GTK's main thread.

mod media;
mod pixels;
mod portal;
mod stream;
mod x11;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use x11rb::protocol::ErrorKind;

use self::x11::{Layout, PixelRect, XDisplay, XError, XResult, XWindow, X11};
use crate::backend::{CaptureBackend, CaptureOptions, PermissionState, WindowCaptureOptions};
use crate::display::{Display, DisplayId, WindowId, WindowInfo, WindowListOptions};
use crate::error::{CaptureError, Result};
use crate::frame::{ColorInfo, ColorSpace, Frame, PixelFormat};
use crate::geometry::{LogicalRect, PhysicalRect, PhysicalSize, ScaleFactor};
use crate::stream::{CaptureStream, HostTime, MicrophoneSource, StreamConfig, StreamSink};
use crate::window::finish_window;

/// How long the Screenshot portal may take once it has answered before.
const PORTAL_LIMIT: Duration = Duration::from_secs(15);
/// How long it may take while the desktop may still be asking the user for the permission.
const PORTAL_FIRST_LIMIT: Duration = Duration::from_secs(120);

/// The kind of desktop session the process runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxSession {
    X11,
    Wayland,
}

/// The session, by the rule the backend and the app's windows share: Wayland when
/// `XDG_SESSION_TYPE` says so or `WAYLAND_DISPLAY` is set, X11 otherwise.
pub fn linux_session() -> LinuxSession {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind.eq_ignore_ascii_case("wayland"));
    if wayland || set("WAYLAND_DISPLAY") {
        LinuxSession::Wayland
    } else {
        LinuxSession::X11
    }
}

/// Sets the folder the backend keeps its small state in: the ScreenCast portal's restore tokens
/// (so the system's picker appears once per display, not on every recording) and the note that
/// the Screenshot portal has answered before. The app calls it once at start; without it,
/// nothing is kept between runs.
pub fn set_state_dir(path: impl Into<PathBuf>) {
    portal::set_state_dir(Some(path.into()));
}

/// The microphones the sound server lists, as (id, name for people). The id is the PulseAudio
/// source name, which is what `MicrophoneSource::device_id` takes on Linux. The list is empty
/// when no sound server answers; an error means GStreamer's PulseAudio plugin is missing.
pub fn microphones() -> Result<Vec<(String, String)>> {
    Ok(media::sources()?
        .into_iter()
        .filter(|source| !source.is_monitor)
        .map(|source| (source.id, source.name))
        .collect())
}

/// Whether a recording can carry system audio: a sound server answers and has an output whose
/// monitor can be read. A stream that asks for system audio without one records silence.
pub fn system_audio_available() -> bool {
    media::sources().map(|sources| sources.iter().any(|source| source.is_monitor)).unwrap_or(false)
}

/// What the backend found on this machine, in a few lines for a bug report or the smoke test.
pub fn diagnostics() -> String {
    let session = linux_session();
    let x = match X11::connect() {
        Ok(mut x) => match x.layout() {
            Ok(layout) => format!(
                "X screen {}×{}, GTK window scale {}, {}",
                layout.root.width,
                layout.root.height,
                layout.scale,
                x.summary()
            ),
            Err(error) => format!("X layout unreadable: {}", error.into_capture()),
        },
        Err(error) => format!("no X connection: {}", error.into_capture()),
    };
    format!("session {session:?}; {x}")
}

/// Now on `CLOCK_MONOTONIC`, the clock GStreamer's system clock and PipeWire stamp with.
pub fn host_now() -> HostTime {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `now` is a valid timespec to write to, and CLOCK_MONOTONIC always exists.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
    }
    HostTime(now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64)
}

/// Opens a microphone on its own (a level meter), stopped.
pub fn open_microphone(source: MicrophoneSource, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
    stream::open_microphone(source, sink)
}

const PERMISSION_UNKNOWN: u8 = 0;
const PERMISSION_GRANTED: u8 = 1;
const PERMISSION_DENIED: u8 = 2;

pub struct LinuxBackend {
    session: LinuxSession,
    /// The one connection every listing and still goes through, opened on first use and opened
    /// again when it breaks.
    x: Mutex<Option<X11>>,
    /// What is known of the Screenshot portal's permission on a Wayland session.
    permission: AtomicU8,
    /// The X screen's size and the portal picture's size as one capture saw them together.
    measured: Mutex<Option<(PhysicalSize, PhysicalSize)>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not take capturing down with it; the data stays usable.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A display of the X layout as the contract describes it, with `pixels` as its frame size.
fn describe(display: &XDisplay, scale: u32, pixels: PhysicalSize) -> Display {
    let scale = scale.max(1) as f64;
    let rect = display.rect;
    let frame = LogicalRect::new(
        rect.x as f64 / scale,
        rect.y as f64 / scale,
        rect.width as f64 / scale,
        rect.height as f64 / scale,
    );
    Display {
        id: DisplayId(display.id),
        name: display.name.clone(),
        frame,
        scale: ScaleFactor(if frame.width > 0.0 { pixels.width as f64 / frame.width } else { scale }),
        pixel_size: pixels,
        is_primary: display.primary,
        is_builtin: display.builtin,
        color_space: ColorSpace::Srgb,
        refresh_rate: display.refresh,
    }
}

/// Where a rectangle of the X screen lies in the portal's picture of the same desktop: each edge
/// in proportion and rounded on its own, so neighbouring displays share their edge.
fn portal_rect(rect: PixelRect, root: PhysicalSize, image: PhysicalSize) -> PhysicalRect {
    let edge = |value: i32, from: u32, to: u32| {
        (value as f64 * to as f64 / from.max(1) as f64).round().clamp(0.0, to as f64) as u32
    };
    let (left, right) = (edge(rect.x, root.width, image.width), edge(rect.right(), root.width, image.width));
    let (top, bottom) = (edge(rect.y, root.height, image.height), edge(rect.bottom(), root.height, image.height));
    PhysicalRect::new(left as i32, top as i32, right.saturating_sub(left), bottom.saturating_sub(top))
}

fn find(layout: &Layout, id: DisplayId) -> Result<&XDisplay> {
    layout.displays.iter().find(|d| d.id == id.0).ok_or(CaptureError::DisplayNotFound(id))
}

fn frame_of(data: Vec<u8>, size: PhysicalSize, scale: f64) -> Result<Frame> {
    Frame::new(size, size.width as usize * 4, PixelFormat::Bgra8, ColorInfo::srgb(), ScaleFactor(scale), data)
}

/// Rows `rect` of tight BGRA `data` that is `width` pixels wide.
fn rows(data: &[u8], width: usize, rect: PhysicalRect) -> Vec<u8> {
    let row = rect.width as usize * 4;
    let mut out = Vec::with_capacity(row * rect.height as usize);
    for y in 0..rect.height as usize {
        let start = ((rect.y as usize + y) * width + rect.x as usize) * 4;
        out.extend_from_slice(&data[start..start + row]);
    }
    out
}

impl LinuxBackend {
    pub fn new() -> Self {
        LinuxBackend {
            session: linux_session(),
            x: Mutex::new(None),
            permission: AtomicU8::new(PERMISSION_UNKNOWN),
            measured: Mutex::new(None),
        }
    }

    fn connect(&self) -> Result<X11> {
        if std::env::var_os("DISPLAY").is_none_or(|display| display.is_empty()) {
            return Err(CaptureError::Backend(match self.session {
                LinuxSession::Wayland => "this Wayland session has no XWayland (DISPLAY is not set), which Glimpsfy \
                                          needs to read the screen's layout and to place its windows"
                    .into(),
                LinuxSession::X11 => "no X display is set (DISPLAY is empty)".into(),
            }));
        }
        X11::connect().map_err(XError::into_capture)
    }

    /// Runs `work` on the kept connection. A connection that broke is opened again and the work
    /// runs once more, so a restarted server costs one retry and not an error.
    fn with_x<T>(&self, work: impl Fn(&mut X11) -> XResult<T>) -> Result<T> {
        let mut guard = lock(&self.x);
        let mut broken = None;
        for _ in 0..2 {
            if guard.is_none() {
                *guard = Some(self.connect()?);
            }
            let Some(x) = guard.as_mut() else { break };
            match x.drain_events().and_then(|_| work(x)) {
                Ok(value) => return Ok(value),
                Err(XError::Broken(words)) => {
                    *guard = None;
                    broken = Some(XError::Broken(words));
                }
                Err(error) => return Err(error.into_capture()),
            }
        }
        Err(broken.map(XError::into_capture).unwrap_or_else(|| CaptureError::Backend("no X connection".into())))
    }

    fn is_wayland(&self) -> bool {
        self.session == LinuxSession::Wayland
    }

    /// One picture of the whole desktop from the Screenshot portal, which also settles what is
    /// known of the permission.
    fn portal_shot(&self) -> Result<portal::Shot> {
        let granted = self.permission() == PermissionState::Granted;
        match portal::screenshot(if granted { PORTAL_LIMIT } else { PORTAL_FIRST_LIMIT }) {
            Ok(shot) => {
                if self.permission.swap(PERMISSION_GRANTED, Ordering::SeqCst) != PERMISSION_GRANTED {
                    portal::note_screenshots_work(true);
                }
                Ok(shot)
            }
            Err(CaptureError::PermissionDenied) => {
                self.permission.store(PERMISSION_DENIED, Ordering::SeqCst);
                portal::note_screenshots_work(false);
                Err(CaptureError::PermissionDenied)
            }
            Err(error) => Err(error),
        }
    }

    fn grab_displays(&self, displays: &[DisplayId], options: &CaptureOptions) -> Result<Vec<(DisplayId, Frame)>> {
        self.with_x(|x| {
            let mut attempt = 0;
            loop {
                let layout = x.layout()?;
                let mut wanted = Vec::with_capacity(displays.len());
                let mut bounds = PixelRect::default();
                for &id in displays {
                    let rect = find(&layout, id)?.rect.intersection(&layout.root_rect());
                    if rect.is_empty() {
                        return Err(CaptureError::DisplayNotFound(id).into());
                    }
                    bounds = bounds.union(&rect);
                    wanted.push((id, rect));
                }
                if wanted.is_empty() {
                    return Ok(Vec::new());
                }
                // One grab for every display, so a multi-display overlay freezes one moment.
                let grab = match x.grab_root(bounds, options.shows_cursor) {
                    Ok(grab) => grab,
                    Err(XError::Refused(ErrorKind::Match, _)) if attempt == 0 => {
                        // The screen changed its size between the listing and the grab.
                        x.forget_layout();
                        attempt += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let scale = layout.scale as f64;
                if let [(id, rect)] = wanted.as_slice() {
                    if *rect == bounds {
                        return Ok(vec![(*id, frame_of(grab.data, grab.size, scale)?)]);
                    }
                }
                let mut frames = Vec::with_capacity(wanted.len());
                for (id, rect) in wanted {
                    let local =
                        PhysicalRect::new(rect.x - bounds.x, rect.y - bounds.y, rect.width as u32, rect.height as u32);
                    frames
                        .push((id, frame_of(rows(&grab.data, grab.size.width as usize, local), local.size(), scale)?));
                }
                return Ok(frames);
            }
        })
    }

    fn portal_displays(&self, displays: &[DisplayId]) -> Result<Vec<(DisplayId, Frame)>> {
        let layout = self.with_x(|x| x.layout())?;
        for &id in displays {
            find(&layout, id)?;
        }
        if displays.is_empty() {
            return Ok(Vec::new());
        }
        let shot = self.portal_shot()?;
        *lock(&self.measured) = Some((layout.root, shot.size));
        let mut frames = Vec::with_capacity(displays.len());
        for &id in displays {
            let display = find(&layout, id)?;
            let rect = portal_rect(display.rect, layout.root, shot.size);
            if rect.width == 0 || rect.height == 0 {
                return Err(CaptureError::Backend("the portal's picture does not cover this display".into()));
            }
            let logical_width = display.rect.width as f64 / layout.scale.max(1) as f64;
            let scale = if logical_width > 0.0 { rect.width as f64 / logical_width } else { 1.0 };
            frames.push((id, frame_of(rows(&shot.data, shot.size.width as usize, rect), rect.size(), scale)?));
        }
        Ok(frames)
    }

    fn window_info(window: XWindow, scale: u32, z_index: u32) -> WindowInfo {
        let scale = scale.max(1) as f64;
        let frame = window.frame;
        WindowInfo {
            id: WindowId(window.id),
            title: window.title,
            app_name: window.app_name,
            bundle_id: None,
            pid: window.pid,
            bounds: LogicalRect::new(
                frame.x as f64 / scale,
                frame.y as f64 / scale,
                frame.width as f64 / scale,
                frame.height as f64 / scale,
            ),
            layer: window.layer,
            z_index,
            alpha: window.alpha,
            is_on_screen: window.on_screen,
            corner_radius: None,
            // X11 has no way for a window to keep itself out of a capture.
            capture_blocked: false,
        }
    }
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for LinuxBackend {
    fn displays(&self) -> Result<Vec<Display>> {
        let layout = self.with_x(|x| x.layout())?;
        let measured =
            if self.is_wayland() { lock(&self.measured).filter(|(root, _)| *root == layout.root) } else { None };
        Ok(layout
            .displays
            .iter()
            .map(|display| {
                let pixels = match measured {
                    Some((root, image)) => portal_rect(display.rect, root, image).size(),
                    None => display.rect.size(),
                };
                describe(display, layout.scale, pixels)
            })
            .collect())
    }

    fn windows(&self, options: WindowListOptions) -> Result<Vec<WindowInfo>> {
        if self.is_wayland() {
            // Wayland shows an app no window but its own; the app hides window mode there.
            return Ok(Vec::new());
        }
        let own = std::process::id() as i32;
        let (windows, scale) = self.with_x(|x| Ok((x.windows()?, x.layout()?.scale)))?;
        Ok(windows
            .into_iter()
            .filter(|w| !(options.on_screen_only && !w.on_screen))
            .filter(|w| !(options.exclude_desktop && w.is_desktop))
            .filter(|w| !(options.exclude_own && w.pid == own))
            .enumerate()
            .map(|(index, window)| LinuxBackend::window_info(window, scale, index as u32))
            .collect())
    }

    fn capture_displays(&self, displays: &[DisplayId], options: &CaptureOptions) -> Result<Vec<(DisplayId, Frame)>> {
        if self.is_wayland() {
            // The portal decides about the pointer itself; `shows_cursor` cannot reach it.
            self.portal_displays(displays)
        } else {
            self.grab_displays(displays, options)
        }
    }

    fn capture_window(&self, window: WindowId, options: &WindowCaptureOptions) -> Result<Frame> {
        if self.is_wayland() {
            return Err(CaptureError::Backend(
                "a Wayland session shows an app no window but its own, so a single window cannot be captured; \
                 displays and areas can"
                    .into(),
            ));
        }
        let (grab, scale) = self.with_x(|x| {
            let grab = x.grab_window(window.0, options.include_shadow, options.shows_cursor)?;
            Ok((grab, x.layout()?.scale))
        })?;
        Ok(finish_window(frame_of(grab.data, grab.size, scale as f64)?, options))
    }

    fn permission(&self) -> PermissionState {
        if !self.is_wayland() {
            return PermissionState::NotRequired;
        }
        match self.permission.load(Ordering::SeqCst) {
            PERMISSION_GRANTED => PermissionState::Granted,
            PERMISSION_DENIED => PermissionState::Denied,
            // Nothing was captured in this run yet: an earlier run's success is the best guess,
            // and the state directory may have been set after the backend was made.
            _ if portal::screenshots_worked_before() => PermissionState::Granted,
            _ => PermissionState::NotDetermined,
        }
    }

    fn request_permission(&self) -> PermissionState {
        // The portal has no question to ask on its own: one screenshot, thrown away, is what
        // makes the desktop ask (where it asks at all) and what shows that captures work.
        if self.is_wayland() && self.permission.load(Ordering::SeqCst) != PERMISSION_GRANTED {
            // An earlier run's success is checked again here, since this is the explicit question.
            let _ = self.portal_shot();
        }
        self.permission()
    }

    fn invalidate(&self) {
        if let Some(x) = lock(&self.x).as_mut() {
            x.forget_layout();
        }
        *lock(&self.measured) = None;
    }

    fn prepare(&self) {
        // The connection, the atoms, the layout and the shared memory segment; no portal call,
        // which could show a prompt.
        let wayland = self.is_wayland();
        let _ = self.with_x(|x| {
            x.layout()?;
            if !wayland {
                x.warm();
            }
            Ok(())
        });
    }

    fn open_stream(&self, config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
        if config.fps == 0 {
            return Err(CaptureError::InvalidArgument("the frame rate is 0".into()));
        }
        if self.is_wayland() {
            let layout = self.with_x(|x| x.layout())?;
            stream::open_portal(&layout, config, sink)
        } else {
            let plan = self.with_x(|x| stream::plan_x11(x, &config))?;
            stream::open_x11(plan, config, sink)
        }
    }
}
