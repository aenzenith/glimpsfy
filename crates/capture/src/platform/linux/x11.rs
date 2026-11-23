//! One connection to the X server and everything read through it: the display layout (RandR),
//! GTK's window scale (XSETTINGS), grabs of the root window or a window's pixmap (MIT-SHM, plain
//! `GetImage` as the fallback), the pointer (XFixes) and the window list (EWMH).
//!
//! The same code serves an X11 session and XWayland on a Wayland session, where only the
//! geometry is read here and the pixels come from the portal.

use std::collections::HashMap;
use std::sync::Arc;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::cookie::Cookie;
use x11rb::errors::{ConnectionError, ReplyError, ReplyOrIdError};
use x11rb::protocol::composite::{self, ConnectionExt as _};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, GetGeometryReply, GetPropertyReply,
    GetWindowAttributesReply, ImageFormat, ImageOrder, MapState, TranslateCoordinatesReply, Window, WindowClass,
};
use x11rb::protocol::ErrorKind;
use x11rb::rust_connection::RustConnection;

use super::pixels::{append_bgra, draw_pointer, unpremultiply, Alpha, ImageLayout, Pointer};
use crate::display::WindowId;
use crate::error::CaptureError;
use crate::geometry::PhysicalSize;

/// The layer of docks and panels: above every window a capture can pick (0 to 8 in the app).
const LAYER_DOCK: i32 = 20;
/// The layer of menus, tooltips and notifications.
const LAYER_POPUP: i32 = 101;
/// The layer of windows the user keeps above the others, which a capture can still pick.
const LAYER_ABOVE: i32 = 3;
/// A plain `GetImage` asks for at most this many bytes per request, so one reply stays small.
const PLAIN_CHUNK_BYTES: usize = 8 << 20;

x11rb::atom_manager! {
    pub(super) Atoms: AtomsCookie {
        _NET_CLIENT_LIST_STACKING,
        _NET_CURRENT_DESKTOP,
        _NET_WM_DESKTOP,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_STATE,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_ABOVE,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_TOOLTIP,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_TYPE_POPUP_MENU,
        _NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
        _NET_WM_WINDOW_TYPE_COMBO,
        _NET_WM_WINDOW_TYPE_DND,
        _NET_FRAME_EXTENTS,
        _GTK_FRAME_EXTENTS,
        _NET_WM_WINDOW_OPACITY,
        _XSETTINGS_SETTINGS,
        WM_STATE,
        EDID,
    }
}

/// Why a request to the X server failed.
#[derive(Debug)]
pub(super) enum XError {
    /// The connection is gone; a new one may work.
    Broken(String),
    /// The server refused one request (a window that closed, a rectangle off the screen).
    Refused(ErrorKind, String),
    Capture(CaptureError),
}

pub(super) type XResult<T> = std::result::Result<T, XError>;

impl From<ConnectionError> for XError {
    fn from(error: ConnectionError) -> Self {
        XError::Broken(error.to_string())
    }
}

impl From<ReplyError> for XError {
    fn from(error: ReplyError) -> Self {
        match error {
            ReplyError::ConnectionError(error) => XError::Broken(error.to_string()),
            ReplyError::X11Error(error) => XError::Refused(
                error.error_kind,
                format!("{:?} for {}", error.error_kind, error.request_name.unwrap_or("a request")),
            ),
        }
    }
}

impl From<ReplyOrIdError> for XError {
    fn from(error: ReplyOrIdError) -> Self {
        match error {
            ReplyOrIdError::ConnectionError(error) => XError::Broken(error.to_string()),
            ReplyOrIdError::X11Error(error) => ReplyError::X11Error(error).into(),
            ReplyOrIdError::IdsExhausted => XError::Broken("the X connection ran out of ids".into()),
        }
    }
}

impl From<CaptureError> for XError {
    fn from(error: CaptureError) -> Self {
        XError::Capture(error)
    }
}

impl XError {
    pub fn into_capture(self) -> CaptureError {
        match self {
            XError::Broken(words) => CaptureError::Backend(format!("the connection to the X server broke: {words}")),
            XError::Refused(_, words) => CaptureError::Backend(format!("the X server refused: {words}")),
            XError::Capture(error) => error,
        }
    }

    /// Whether the server said the window (or drawable) does not exist.
    fn is_gone(&self) -> bool {
        matches!(self, XError::Refused(ErrorKind::Window | ErrorKind::Drawable | ErrorKind::Pixmap, _))
    }
}

/// A rectangle in root pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl PixelRect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> PixelRect {
        PixelRect { x, y, width, height }
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub fn intersection(&self, other: &PixelRect) -> PixelRect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        PixelRect::new(x, y, (right - x).max(0), (bottom - y).max(0))
    }

    pub fn union(&self, other: &PixelRect) -> PixelRect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        PixelRect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }

    pub fn size(&self) -> PhysicalSize {
        PhysicalSize::new(self.width.max(0) as u32, self.height.max(0) as u32)
    }
}

/// One monitor as RandR describes it, in root pixels.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct XDisplay {
    pub id: u32,
    /// A name for people: the monitor's model, or the connector when the monitor names none.
    pub name: String,
    /// The output's name ("DP-1", "XWAYLAND0"), which stays the same between sessions.
    pub connector: String,
    pub rect: PixelRect,
    pub primary: bool,
    pub builtin: bool,
    pub refresh: Option<f64>,
}

/// The screen as one listing saw it: the root window's size, GTK's window scale and the
/// monitors, the primary one first.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Layout {
    pub root: PhysicalSize,
    pub scale: u32,
    pub displays: Vec<XDisplay>,
}

impl Layout {
    pub fn root_rect(&self) -> PixelRect {
        PixelRect::new(0, 0, self.root.width as i32, self.root.height as i32)
    }
}

/// A top-level window as the window manager's properties describe it.
#[derive(Clone, Debug)]
pub(super) struct XWindow {
    pub id: u32,
    pub title: Option<String>,
    pub app_name: String,
    pub pid: i32,
    /// What the user sees as the window, in root pixels: the client with the window manager's
    /// frame around it, or a client-side decorated surface without its shadow.
    pub frame: PixelRect,
    /// The client's whole surface, which holds the shadow of a client-side decorated window.
    pub surface: PixelRect,
    /// Whether the client draws its own decorations and shadow (`_GTK_FRAME_EXTENTS`).
    pub client_decorated: bool,
    pub layer: i32,
    pub alpha: f64,
    pub viewable: bool,
    pub on_screen: bool,
    pub is_desktop: bool,
}

/// Pixels grabbed from the server as tight BGRA.
pub(super) struct Grab {
    pub size: PhysicalSize,
    pub data: Vec<u8>,
}

/// A System V shared memory segment the server writes images into.
struct Segment {
    seg: shm::Seg,
    address: *mut u8,
    size: usize,
}

// SAFETY: the mapping belongs to this process for as long as the segment lives, and it is only
// read behind `&mut X11`, so one thread at a time touches it.
unsafe impl Send for Segment {}

impl Segment {
    fn bytes(&self, length: usize) -> &[u8] {
        // SAFETY: `address` maps `size` readable bytes until `Drop` detaches them, and the length
        // is clamped to that size.
        unsafe { std::slice::from_raw_parts(self.address, length.min(self.size)) }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: `address` came from a successful `shmat` and is detached exactly once.
        unsafe {
            libc::shmdt(self.address.cast());
        }
    }
}

enum Shm {
    Untried,
    /// The extension is missing, or the server cannot attach this process's memory (a remote
    /// display, a sandbox with its own IPC namespace).
    Unavailable,
    Ready(Segment),
}

pub(super) struct X11 {
    conn: RustConnection,
    screen: usize,
    pub root: Window,
    atoms: Atoms,
    settings_selection: u32,
    compositor_selection: u32,
    /// The window whose XSETTINGS property is watched for changes of the scale.
    settings_owner: Window,
    randr: Option<(u32, u32)>,
    has_xfixes: bool,
    has_composite: bool,
    shm: Shm,
    layout: Option<Arc<Layout>>,
}

impl X11 {
    /// Connects to the display `DISPLAY` names and asks which extensions it has.
    pub fn connect() -> XResult<X11> {
        let (conn, screen) = x11rb::connect(None)
            .map_err(|e| XError::Capture(CaptureError::Backend(format!("could not connect to the X server: {e}"))))?;
        let root = conn
            .setup()
            .roots
            .get(screen)
            .map(|s| s.root)
            .ok_or_else(|| XError::Capture(CaptureError::Backend("the X server has no such screen".into())))?;
        let atoms = Atoms::new(&conn)?.reply()?;
        let settings_selection = conn.intern_atom(false, format!("_XSETTINGS_S{screen}").as_bytes())?.reply()?.atom;
        let compositor_selection = conn.intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes())?.reply()?.atom;

        let randr = match conn.extension_information(randr::X11_EXTENSION_NAME)? {
            Some(_) => conn.randr_query_version(1, 5)?.reply().ok().map(|v| (v.major_version, v.minor_version)),
            None => None,
        };
        let has_xfixes = match conn.extension_information(xfixes::X11_EXTENSION_NAME)? {
            // XFixes answers only requests of the version a client asked for.
            Some(_) => conn.xfixes_query_version(4, 0)?.reply().is_ok(),
            None => false,
        };
        let has_composite = match conn.extension_information(composite::X11_EXTENSION_NAME)? {
            Some(_) => conn
                .composite_query_version(0, 4)?
                .reply()
                .map(|v| (v.major_version, v.minor_version) >= (0, 2))
                .unwrap_or(false),
            None => false,
        };
        // Any event on this connection means the layout may have changed, which is what clears
        // the cached layout: the root's size, the monitors, a new XSETTINGS manager.
        conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY))?
            .ignore_error();
        if randr.is_some() {
            let mask =
                randr::NotifyMask::SCREEN_CHANGE | randr::NotifyMask::CRTC_CHANGE | randr::NotifyMask::OUTPUT_CHANGE;
            conn.randr_select_input(root, mask)?.ignore_error();
        }
        conn.flush()?;
        Ok(X11 {
            conn,
            screen,
            root,
            atoms,
            settings_selection,
            compositor_selection,
            settings_owner: x11rb::NONE,
            randr,
            has_xfixes,
            has_composite,
            shm: Shm::Untried,
            layout: None,
        })
    }

    /// Reads the events that arrived since the last call; any of them clears the cached layout.
    pub fn drain_events(&mut self) -> XResult<()> {
        let mut changed = false;
        while self.conn.poll_for_event()?.is_some() {
            changed = true;
        }
        if changed {
            self.layout = None;
        }
        Ok(())
    }

    pub fn forget_layout(&mut self) {
        self.layout = None;
    }

    /// The layout, read once and kept until an event or `forget_layout` clears it.
    pub fn layout(&mut self) -> XResult<Arc<Layout>> {
        if let Some(layout) = &self.layout {
            return Ok(layout.clone());
        }
        let root = self.conn.get_geometry(self.root)?.reply()?;
        let root = PhysicalSize::new(root.width as u32, root.height as u32);
        let scale = self.window_scale()?;
        let mut displays = self.monitors().unwrap_or_default();
        displays.retain(|d| !d.rect.is_empty());
        // Mirrored outputs share one rectangle; one display stands for them.
        let mut seen: Vec<PixelRect> = Vec::new();
        displays.retain(|d| {
            let first = !seen.contains(&d.rect);
            seen.push(d.rect);
            first
        });
        if displays.is_empty() {
            displays.push(XDisplay {
                id: 1,
                name: "Screen".into(),
                connector: "screen".into(),
                rect: PixelRect::new(0, 0, root.width as i32, root.height as i32),
                primary: true,
                builtin: false,
                refresh: None,
            });
        }
        displays.sort_by_key(|d| (!d.primary, d.rect.x, d.rect.y));
        if !displays[0].primary {
            displays[0].primary = true;
        }
        for other in displays.iter_mut().skip(1) {
            other.primary = false;
        }
        let layout = Arc::new(Layout { root, scale, displays });
        self.layout = Some(layout.clone());
        Ok(layout)
    }

    /// The monitors RandR lists: logical monitors on RandR 1.5, active CRTCs before that.
    fn monitors(&mut self) -> XResult<Vec<XDisplay>> {
        let Some(version) = self.randr else { return Ok(Vec::new()) };
        if version < (1, 2) {
            return Ok(Vec::new());
        }
        let resources = if version >= (1, 3) {
            let r = self.conn.randr_get_screen_resources_current(self.root)?.reply()?;
            (r.config_timestamp, r.crtcs, r.modes)
        } else {
            let r = self.conn.randr_get_screen_resources(self.root)?.reply()?;
            (r.config_timestamp, r.crtcs, r.modes)
        };
        let (timestamp, crtcs, modes) = resources;
        let refresh_of: HashMap<u32, f64> = modes
            .iter()
            .filter_map(|mode| {
                let mut lines = mode.vtotal as f64;
                if mode.mode_flags.contains(randr::ModeFlag::DOUBLE_SCAN) {
                    lines *= 2.0;
                }
                if mode.mode_flags.contains(randr::ModeFlag::INTERLACE) {
                    lines /= 2.0;
                }
                let total = mode.htotal as f64 * lines;
                (total > 0.0 && mode.dot_clock > 0).then(|| (mode.id, mode.dot_clock as f64 / total))
            })
            .collect();

        let mut displays = Vec::new();
        if version >= (1, 5) {
            let monitors = self.conn.randr_get_monitors(self.root, true)?.reply()?.monitors;
            for (index, monitor) in monitors.iter().enumerate() {
                let rect =
                    PixelRect::new(monitor.x as i32, monitor.y as i32, monitor.width as i32, monitor.height as i32);
                let output = monitor.outputs.first().copied();
                let described = match output {
                    Some(output) => self.describe_output(output, timestamp, &refresh_of).ok(),
                    None => None,
                };
                let fallback = self.atom_name(monitor.name).unwrap_or_else(|| format!("Screen {}", index + 1));
                let (connector, model, refresh) = described.unwrap_or((fallback.clone(), None, None));
                let connector = if connector.is_empty() { fallback } else { connector };
                displays.push(XDisplay {
                    id: output.unwrap_or(monitor.name),
                    name: model.unwrap_or_else(|| connector.clone()),
                    builtin: is_builtin(&connector),
                    connector,
                    rect,
                    primary: monitor.primary,
                    refresh,
                });
            }
            if !displays.is_empty() {
                return Ok(displays);
            }
        }
        let primary = self.conn.randr_get_output_primary(self.root)?.reply().map(|r| r.output).unwrap_or(x11rb::NONE);
        for crtc in crtcs {
            let Ok(info) = self.conn.randr_get_crtc_info(crtc, timestamp)?.reply() else { continue };
            let Some(&output) = info.outputs.first() else { continue };
            if info.width == 0 || info.height == 0 {
                continue;
            }
            let (connector, model, refresh) =
                self.describe_output(output, timestamp, &refresh_of).unwrap_or((String::new(), None, None));
            let connector = if connector.is_empty() { format!("Screen {}", displays.len() + 1) } else { connector };
            displays.push(XDisplay {
                id: output,
                name: model.unwrap_or_else(|| connector.clone()),
                builtin: is_builtin(&connector),
                connector,
                rect: PixelRect::new(info.x as i32, info.y as i32, info.width as i32, info.height as i32),
                primary: info.outputs.contains(&primary),
                refresh,
            });
        }
        Ok(displays)
    }

    /// An output's connector name, its monitor's model from the EDID and its refresh rate.
    fn describe_output(
        &self,
        output: randr::Output,
        timestamp: u32,
        refresh_of: &HashMap<u32, f64>,
    ) -> XResult<(String, Option<String>, Option<f64>)> {
        let info = self.conn.randr_get_output_info(output, timestamp)?.reply()?;
        let connector = String::from_utf8_lossy(&info.name).into_owned();
        let refresh = if info.crtc == x11rb::NONE {
            None
        } else {
            let crtc = self.conn.randr_get_crtc_info(info.crtc, timestamp)?.reply()?;
            refresh_of.get(&crtc.mode).copied()
        };
        let edid = self
            .conn
            .randr_get_output_property(output, self.atoms.EDID, AtomEnum::ANY, 0, 128, false, false)?
            .reply()
            .ok()
            .and_then(|reply| edid_model(&reply.data));
        Ok((connector, edid, refresh))
    }

    fn atom_name(&self, atom: u32) -> Option<String> {
        if atom == x11rb::NONE {
            return None;
        }
        let reply = self.conn.get_atom_name(atom).ok()?.reply().ok()?;
        Some(String::from_utf8_lossy(&reply.name).into_owned())
    }

    /// The one integer scale GTK gives every window of this screen: `GDK_SCALE` when it is set,
    /// else the XSETTINGS value `Gdk/WindowScalingFactor`, else 1.
    fn window_scale(&mut self) -> XResult<u32> {
        if let Some(scale) = std::env::var("GDK_SCALE").ok().and_then(|v| v.trim().parse::<u32>().ok()) {
            return Ok(scale.clamp(1, 16));
        }
        let owner = self.conn.get_selection_owner(self.settings_selection)?.reply()?.owner;
        if owner == x11rb::NONE {
            return Ok(1);
        }
        if owner != self.settings_owner {
            // A change of the settings arrives as an event and clears the cached layout.
            let watch =
                ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY);
            self.conn.change_window_attributes(owner, &watch)?.ignore_error();
            self.settings_owner = owner;
        }
        let property = self.atoms._XSETTINGS_SETTINGS;
        let reply = match self.conn.get_property(false, owner, property, AtomEnum::ANY, 0, 1 << 18)?.reply() {
            Ok(reply) => reply,
            Err(ReplyError::X11Error(_)) => return Ok(1),
            Err(error) => return Err(error.into()),
        };
        Ok(xsettings_integer(&reply.value, "Gdk/WindowScalingFactor").filter(|&v| v >= 1).unwrap_or(1).clamp(1, 16)
            as u32)
    }

    /// Whether a compositing manager runs, which is when the server keeps every window's picture
    /// off screen.
    pub fn is_composited(&self) -> XResult<bool> {
        if !self.has_composite {
            return Ok(false);
        }
        Ok(self.conn.get_selection_owner(self.compositor_selection)?.reply()?.owner != x11rb::NONE)
    }

    fn layout_for(&self, depth: u8, visual: u32) -> XResult<ImageLayout> {
        let setup = self.conn.setup();
        let format = setup.pixmap_formats.iter().find(|f| f.depth == depth).ok_or_else(|| {
            XError::Capture(CaptureError::Backend(format!("the X server names no pixel format for depth {depth}")))
        })?;
        let screen = &setup.roots[self.screen];
        let visuals = || screen.allowed_depths.iter().flat_map(|d| d.visuals.iter().map(move |v| (d.depth, v)));
        let wanted = if visual != x11rb::NONE { visual } else { screen.root_visual };
        let masks = visuals()
            .find(|(d, v)| *d == depth && v.visual_id == wanted)
            .or_else(|| visuals().find(|(d, v)| *d == depth && v.red_mask != 0))
            .map(|(_, v)| (v.red_mask, v.green_mask, v.blue_mask))
            .unwrap_or(match depth {
                30 => (0x3ff0_0000, 0x000f_fc00, 0x0000_03ff),
                16 => (0xf800, 0x07e0, 0x001f),
                15 => (0x7c00, 0x03e0, 0x001f),
                _ => (0x00ff_0000, 0x0000_ff00, 0x0000_00ff),
            });
        Ok(ImageLayout {
            bits_per_pixel: format.bits_per_pixel,
            scanline_pad: format.scanline_pad,
            msb_first: setup.image_byte_order == ImageOrder::MSB_FIRST,
            red: masks.0,
            green: masks.1,
            blue: masks.2,
        })
    }

    /// The shared memory segment, at least `size` bytes large, or `None` when the server cannot
    /// share memory with this process.
    fn segment(&mut self, size: usize) -> XResult<Option<&Segment>> {
        if matches!(self.shm, Shm::Unavailable) {
            return Ok(None);
        }
        let large_enough = matches!(&self.shm, Shm::Ready(segment) if segment.size >= size);
        if !large_enough {
            if let Shm::Ready(old) = std::mem::replace(&mut self.shm, Shm::Unavailable) {
                self.conn.shm_detach(old.seg)?.ignore_error();
            }
            if self.conn.extension_information(shm::X11_EXTENSION_NAME)?.is_none()
                || self.conn.shm_query_version()?.reply().is_err()
            {
                return Ok(None);
            }
            // Sized for the whole screen at once, so a stream of regions never reallocates.
            let whole = self.layout.as_ref().map(|l| l.root.area() as usize * 4).unwrap_or(0);
            let size = size.max(whole).max(4096);
            let seg = self.conn.generate_id()?;
            // SAFETY: plain System V calls on a segment this function creates. It is marked for
            // removal as soon as both sides are attached (or the attach failed), so the kernel
            // frees it when the last of them detaches, also after a crash.
            let address = unsafe {
                let id = libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600);
                if id < 0 {
                    return Ok(None);
                }
                let address = libc::shmat(id, std::ptr::null(), 0);
                if address as isize == -1 {
                    libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                    return Ok(None);
                }
                let attached = match self.conn.shm_attach(seg, id as u32, false) {
                    Ok(cookie) => cookie.check(),
                    Err(error) => Err(ReplyError::ConnectionError(error)),
                };
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                match attached {
                    Ok(()) => address.cast::<u8>(),
                    Err(error) => {
                        libc::shmdt(address);
                        return match error {
                            ReplyError::X11Error(_) => Ok(None),
                            other => Err(other.into()),
                        };
                    }
                }
            };
            self.shm = Shm::Ready(Segment { seg, address, size });
        }
        Ok(match &self.shm {
            Shm::Ready(segment) => Some(segment),
            _ => None,
        })
    }

    /// Grabs `rect` of `drawable` (in the drawable's own pixels, wholly inside it) as BGRA.
    fn grab(&mut self, drawable: u32, rect: PixelRect, alpha: Alpha) -> XResult<Grab> {
        let size = rect.size();
        if rect.is_empty() || rect.width > u16::MAX as i32 || rect.height > u16::MAX as i32 {
            return Err(CaptureError::InvalidArgument("the region to grab is empty".into()).into());
        }
        let (x, y, w, h) = (rect.x as i16, rect.y as i16, rect.width as u16, rect.height as u16);
        let mut data = Vec::new();
        // The largest layout a server uses is 32 bits per pixel.
        if let Some(seg) = self.segment(size.area() as usize * 4)?.map(|s| s.seg) {
            let reply =
                self.conn.shm_get_image(drawable, x, y, w, h, !0, ImageFormat::Z_PIXMAP.into(), seg, 0)?.reply();
            match reply {
                Ok(reply) => {
                    let layout = self.layout_for(reply.depth, reply.visual)?;
                    let alpha = if reply.depth == 32 { alpha } else { Alpha::Opaque };
                    if let Shm::Ready(segment) = &self.shm {
                        let bytes = segment.bytes(reply.size as usize);
                        append_bgra(&mut data, bytes, size.width as usize, size.height as usize, &layout, alpha)?;
                    }
                    return Ok(Grab { size, data });
                }
                Err(ReplyError::X11Error(error))
                    if matches!(
                        error.error_kind,
                        ErrorKind::Match | ErrorKind::Window | ErrorKind::Drawable | ErrorKind::Pixmap
                    ) =>
                {
                    return Err(ReplyError::X11Error(error).into());
                }
                Err(ReplyError::X11Error(_)) => {
                    // The segment itself was refused: go on without shared memory.
                    self.shm = Shm::Unavailable;
                }
                Err(error) => return Err(error.into()),
            }
        }
        // Without shared memory the image comes in rows over the socket. The requests are all
        // sent before the first reply is read, so the server answers them back to back and the
        // rows show (as nearly as X allows) one moment.
        let rows_per_chunk = (PLAIN_CHUNK_BYTES / (size.width as usize * 4).max(1)).clamp(1, u16::MAX as usize) as i32;
        let mut cookies = Vec::new();
        let mut top = 0;
        while top < rect.height {
            let rows = rows_per_chunk.min(rect.height - top);
            let cookie =
                self.conn.get_image(ImageFormat::Z_PIXMAP, drawable, x, (rect.y + top) as i16, w, rows as u16, !0)?;
            cookies.push((cookie, rows));
            top += rows;
        }
        for (cookie, rows) in cookies {
            let reply = cookie.reply()?;
            let layout = self.layout_for(reply.depth, reply.visual)?;
            let alpha = if reply.depth == 32 { alpha } else { Alpha::Opaque };
            append_bgra(&mut data, &reply.data, size.width as usize, rows as usize, &layout, alpha)?;
        }
        Ok(Grab { size, data })
    }

    /// The pointer's picture and place, or `None` when the server has no XFixes.
    pub fn pointer(&self) -> XResult<Option<Pointer>> {
        if !self.has_xfixes {
            return Ok(None);
        }
        let image = match self.conn.xfixes_get_cursor_image()?.reply() {
            Ok(image) => image,
            Err(ReplyError::X11Error(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(Pointer {
            x: image.x as i32 - image.xhot as i32,
            y: image.y as i32 - image.yhot as i32,
            width: image.width as u32,
            height: image.height as u32,
            argb: image.cursor_image,
        }))
    }

    /// Grabs `rect` of the root window, clipped to the screen by the caller, with the pointer
    /// drawn in when asked.
    pub fn grab_root(&mut self, rect: PixelRect, with_pointer: bool) -> XResult<Grab> {
        let mut grab = self.grab(self.root, rect, Alpha::Opaque)?;
        if with_pointer {
            if let Some(pointer) = self.pointer()? {
                draw_pointer(&mut grab.data, grab.size, rect.x, rect.y, &pointer);
            }
        }
        Ok(grab)
    }

    /// The windows in stacking order, the frontmost first.
    pub fn windows(&mut self) -> XResult<Vec<XWindow>> {
        let stacking = self.conn.get_property(
            false,
            self.root,
            self.atoms._NET_CLIENT_LIST_STACKING,
            AtomEnum::WINDOW,
            0,
            1 << 16,
        )?;
        let listed: Vec<u32> = stacking.reply()?.value32().map(|v| v.collect()).unwrap_or_default();
        let clients = if listed.is_empty() {
            // No EWMH window manager: the server's own stacking of the root's children.
            let children = self.conn.query_tree(self.root)?.reply()?.children;
            let mut clients = Vec::new();
            for child in children {
                if let Some(client) = self.client_of(child)? {
                    clients.push(client);
                }
            }
            clients
        } else {
            listed
        };
        let described = self.describe(&clients)?;
        Ok(described.into_iter().rev().flatten().collect())
    }

    /// The client window inside a child of the root: the child itself or the window in it that
    /// carries `WM_STATE`. `None` for windows that are no application's (menus, hidden helpers).
    fn client_of(&self, child: Window) -> XResult<Option<Window>> {
        let attributes = match self.conn.get_window_attributes(child)?.reply() {
            Ok(attributes) => attributes,
            Err(ReplyError::X11Error(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if attributes.override_redirect || attributes.class != WindowClass::INPUT_OUTPUT {
            return Ok(None);
        }
        let mut level = vec![child];
        for _ in 0..3 {
            let mut next = Vec::new();
            for &window in &level {
                if self.has_property(window, self.atoms.WM_STATE)? {
                    return Ok(Some(window));
                }
                if let Ok(tree) = self.conn.query_tree(window)?.reply() {
                    next.extend(tree.children);
                }
            }
            if next.is_empty() || next.len() > 64 {
                break;
            }
            level = next;
        }
        // A window manager that sets no WM_STATE at all: a mapped window with a class counts.
        let named =
            attributes.map_state == MapState::VIEWABLE && self.has_property(child, AtomEnum::WM_CLASS.into())?;
        Ok(named.then_some(child))
    }

    fn has_property(&self, window: Window, property: u32) -> XResult<bool> {
        match self.conn.get_property(false, window, property, AtomEnum::ANY, 0, 0)?.reply() {
            Ok(reply) => Ok(reply.type_ != x11rb::NONE),
            Err(ReplyError::X11Error(_)) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Describes each of `windows`; a window that closed meanwhile is `None`. Every request is
    /// sent before the first reply is read, so a long list costs one round trip.
    pub fn describe(&self, windows: &[Window]) -> XResult<Vec<Option<XWindow>>> {
        struct Asked<'c> {
            geometry: Cookie<'c, RustConnection, GetGeometryReply>,
            origin: Cookie<'c, RustConnection, TranslateCoordinatesReply>,
            attributes: Cookie<'c, RustConnection, GetWindowAttributesReply>,
            properties: Vec<Cookie<'c, RustConnection, GetPropertyReply>>,
        }
        let a = &self.atoms;
        let wanted: [u32; 10] = [
            a._NET_WM_NAME,
            AtomEnum::WM_NAME.into(),
            AtomEnum::WM_CLASS.into(),
            a._NET_WM_PID,
            a._NET_WM_DESKTOP,
            a._NET_WM_STATE,
            a._NET_WM_WINDOW_TYPE,
            a._NET_FRAME_EXTENTS,
            a._GTK_FRAME_EXTENTS,
            a._NET_WM_WINDOW_OPACITY,
        ];
        let current = self.conn.get_property(false, self.root, a._NET_CURRENT_DESKTOP, AtomEnum::CARDINAL, 0, 1)?;
        let mut asked = Vec::with_capacity(windows.len());
        for &window in windows {
            let mut properties = Vec::with_capacity(wanted.len());
            let geometry = self.conn.get_geometry(window)?;
            let origin = self.conn.translate_coordinates(window, self.root, 0, 0)?;
            let attributes = self.conn.get_window_attributes(window)?;
            for property in wanted {
                properties.push(self.conn.get_property(false, window, property, AtomEnum::ANY, 0, 2048)?);
            }
            asked.push(Asked { geometry, origin, attributes, properties });
        }
        let current_desktop = match current.reply() {
            Ok(reply) => reply.value32().and_then(|mut v| v.next()),
            Err(ReplyError::X11Error(_)) => None,
            Err(error) => return Err(error.into()),
        };

        let mut described = Vec::with_capacity(windows.len());
        for (&id, asked) in windows.iter().zip(asked) {
            // A protocol error means the window closed; only a broken connection ends the listing.
            let geometry = optional(asked.geometry.reply())?;
            let origin = optional(asked.origin.reply())?;
            let attributes = optional(asked.attributes.reply())?;
            let mut properties = Vec::with_capacity(wanted.len());
            for cookie in asked.properties {
                properties.push(optional(cookie.reply())?);
            }
            let (Some(geometry), Some(origin), Some(attributes)) = (geometry, origin, attributes) else {
                described.push(None);
                continue;
            };
            let property =
                |index: usize| properties.get(index).and_then(|p| p.as_ref()).filter(|p| p.type_ != x11rb::NONE);
            let numbers = |index: usize| -> Vec<u32> {
                property(index).and_then(|p| p.value32()).map(|v| v.collect()).unwrap_or_default()
            };
            let title = property(0).or(property(1)).map(|p| text(&p.value)).filter(|t| !t.is_empty());
            let class = property(2).map(|p| p.value.clone()).unwrap_or_default();
            let mut class_parts = class.split(|&b| b == 0).filter(|part| !part.is_empty()).map(text);
            let instance = class_parts.next();
            let app_name = class_parts.next().or(instance).unwrap_or_default();
            let pid = numbers(3).first().copied().unwrap_or(0) as i32;
            let desktop = numbers(4).first().copied();
            let states = numbers(5);
            let types = numbers(6);
            let frame_extents = numbers(7);
            let gtk_extents = numbers(8);
            let opacity = numbers(9).first().copied();

            let surface =
                PixelRect::new(origin.dst_x as i32, origin.dst_y as i32, geometry.width as i32, geometry.height as i32);
            let extent = |list: &[u32], index: usize| list.get(index).copied().unwrap_or(0).min(4096) as i32;
            let client_decorated = gtk_extents.len() >= 4 && gtk_extents.iter().any(|&e| e > 0);
            // Both lists are left, right, top, bottom.
            let frame = if client_decorated {
                let (l, r, t, b) = (
                    extent(&gtk_extents, 0),
                    extent(&gtk_extents, 1),
                    extent(&gtk_extents, 2),
                    extent(&gtk_extents, 3),
                );
                let inner = PixelRect::new(surface.x + l, surface.y + t, surface.width - l - r, surface.height - t - b);
                if inner.is_empty() {
                    surface
                } else {
                    inner
                }
            } else {
                let (l, r, t, b) = (
                    extent(&frame_extents, 0),
                    extent(&frame_extents, 1),
                    extent(&frame_extents, 2),
                    extent(&frame_extents, 3),
                );
                PixelRect::new(surface.x - l, surface.y - t, surface.width + l + r, surface.height + t + b)
            };

            let is_desktop = types.contains(&a._NET_WM_WINDOW_TYPE_DESKTOP);
            let popup = [
                a._NET_WM_WINDOW_TYPE_TOOLTIP,
                a._NET_WM_WINDOW_TYPE_NOTIFICATION,
                a._NET_WM_WINDOW_TYPE_POPUP_MENU,
                a._NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
                a._NET_WM_WINDOW_TYPE_COMBO,
                a._NET_WM_WINDOW_TYPE_DND,
            ];
            let layer = if is_desktop {
                -1
            } else if types.contains(&a._NET_WM_WINDOW_TYPE_DOCK) {
                LAYER_DOCK
            } else if types.iter().any(|t| popup.contains(t)) {
                LAYER_POPUP
            } else if states.contains(&a._NET_WM_STATE_ABOVE) {
                LAYER_ABOVE
            } else {
                0
            };
            let viewable = attributes.map_state == MapState::VIEWABLE;
            let on_this_desktop = match (desktop, current_desktop) {
                (Some(u32::MAX), _) => true,
                (Some(own), Some(current)) => own == current,
                _ => true,
            };
            let hidden = states.contains(&a._NET_WM_STATE_HIDDEN);
            described.push(Some(XWindow {
                id,
                title,
                app_name,
                pid,
                frame,
                surface,
                client_decorated,
                layer,
                alpha: opacity.map(|o| o as f64 / u32::MAX as f64).unwrap_or(1.0),
                viewable,
                on_screen: viewable && on_this_desktop && !hidden,
                is_desktop,
            }));
        }
        Ok(described)
    }

    /// Where a window's surface is now, in root pixels: the cheap question a stream that follows
    /// a window asks for every frame.
    pub fn surface_of(&self, window: Window) -> XResult<PixelRect> {
        let geometry = self.conn.get_geometry(window)?;
        let origin = self.conn.translate_coordinates(window, self.root, 0, 0)?;
        let (geometry, origin) = (geometry.reply(), origin.reply());
        match (geometry, origin) {
            (Ok(g), Ok(o)) => Ok(PixelRect::new(o.dst_x as i32, o.dst_y as i32, g.width as i32, g.height as i32)),
            (Err(error), _) | (_, Err(error)) => {
                let error = XError::from(error);
                if error.is_gone() {
                    Err(CaptureError::WindowNotFound(WindowId(window)).into())
                } else {
                    Err(error)
                }
            }
        }
    }

    /// One window's picture: its off-screen pixmap when a compositing manager keeps one (so
    /// covered and off-screen parts are there, with alpha for 32-bit windows), else the part of
    /// the screen it occupies. The pixels are straight alpha.
    pub fn grab_window(&mut self, id: Window, include_shadow: bool, with_pointer: bool) -> XResult<Grab> {
        let window =
            self.describe(&[id])?.pop().flatten().ok_or(XError::Capture(CaptureError::WindowNotFound(WindowId(id))))?;
        // With the shadow, a client-side decorated window is its whole surface; a window the
        // window manager frames has no shadow in its own picture, so it stays its frame.
        let wanted = if include_shadow && window.client_decorated { window.surface } else { window.frame };
        if wanted.is_empty() {
            return Err(CaptureError::InvalidArgument("the window has no size".into()).into());
        }
        let pointer = if with_pointer { self.pointer()? } else { None };

        if window.viewable && self.is_composited()? {
            match self.grab_pixmap(id, wanted, pointer.as_ref()) {
                Ok(grab) => return Ok(grab),
                Err(XError::Broken(words)) => return Err(XError::Broken(words)),
                // The server would not name the pixmap: the screen still shows the window.
                Err(_) => {}
            }
        }
        if !window.viewable {
            return Err(CaptureError::Backend(
                "the window is minimised or on another desktop, and X11 keeps no picture of it then".into(),
            )
            .into());
        }
        let layout = self.layout()?;
        let visible = wanted.intersection(&layout.root_rect());
        if visible.is_empty() {
            return Err(CaptureError::InvalidArgument("the window lies outside the screen".into()).into());
        }
        let mut grab = self.grab(self.root, visible, Alpha::Opaque)?;
        if let Some(pointer) = &pointer {
            draw_pointer(&mut grab.data, grab.size, visible.x, visible.y, pointer);
        }
        Ok(grab)
    }

    /// The part `wanted` (root pixels) of the pixmap the compositing manager's redirection keeps
    /// for the top-level window that holds `id`.
    fn grab_pixmap(&mut self, id: Window, wanted: PixelRect, pointer: Option<&Pointer>) -> XResult<Grab> {
        // The redirected window is the child of the root: the window manager's frame around the
        // client, or the client itself.
        let mut top = id;
        for _ in 0..16 {
            let tree = self.conn.query_tree(top)?.reply()?;
            if tree.parent == self.root || tree.parent == x11rb::NONE {
                break;
            }
            top = tree.parent;
        }
        let outer = self.conn.get_geometry(top)?.reply()?;
        let pixmap = self.conn.generate_id()?;
        self.conn.composite_name_window_pixmap(top, pixmap)?.check()?;
        let result = self.grab_named(pixmap, (outer.x as i32, outer.y as i32), wanted, pointer);
        self.conn.free_pixmap(pixmap)?.ignore_error();
        result
    }

    /// Grabs the part `wanted` of a named window pixmap whose top-left corner (the window's
    /// outer corner, its border included) is at `origin` in root pixels.
    fn grab_named(
        &mut self,
        pixmap: u32,
        origin: (i32, i32),
        wanted: PixelRect,
        pointer: Option<&Pointer>,
    ) -> XResult<Grab> {
        let geometry = self.conn.get_geometry(pixmap)?.reply()?;
        let whole = PixelRect::new(origin.0, origin.1, geometry.width as i32, geometry.height as i32);
        let mut part = wanted.intersection(&whole);
        if part.is_empty() {
            part = whole;
        }
        let local = PixelRect::new(part.x - origin.0, part.y - origin.1, part.width, part.height);
        let mut grab = self.grab(pixmap, local, Alpha::Keep)?;
        if let Some(pointer) = pointer {
            draw_pointer(&mut grab.data, grab.size, part.x, part.y, pointer);
        }
        if geometry.depth == 32 {
            unpremultiply(&mut grab.data);
        }
        Ok(grab)
    }

    /// Sets up the shared memory segment ahead of the first grab.
    pub fn warm(&mut self) {
        let _ = self.segment(4096);
    }

    /// What this connection found, in words, for the smoke test's report.
    pub fn summary(&mut self) -> String {
        let shm = match self.segment(4096) {
            Ok(Some(_)) => "shared memory",
            _ => "plain GetImage",
        };
        format!(
            "RandR {:?}, XFixes {}, Composite {} (compositing manager {}), grabs through {shm}",
            self.randr,
            self.has_xfixes,
            self.has_composite,
            self.is_composited().unwrap_or(false),
        )
    }
}

/// A reply, `None` when the server refused the request, an error only when the connection broke.
fn optional<T>(reply: std::result::Result<T, ReplyError>) -> XResult<Option<T>> {
    match reply {
        Ok(reply) => Ok(Some(reply)),
        Err(ReplyError::X11Error(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// A text property as a string: UTF-8 when it is valid, Latin-1 (what `STRING` holds) otherwise.
fn text(bytes: &[u8]) -> String {
    let bytes = bytes.split(|&b| b == 0).next().unwrap_or_default();
    match std::str::from_utf8(bytes) {
        Ok(text) => text.trim().to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect::<String>().trim().to_string(),
    }
}

/// Laptop panels by their connector's name.
fn is_builtin(connector: &str) -> bool {
    let name = connector.to_ascii_lowercase();
    name.starts_with("edp") || name.starts_with("lvds") || name.starts_with("dsi")
}

/// The monitor's model name from an EDID block (descriptor `0xFC`).
fn edid_model(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 || edid[..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
        return None;
    }
    [54usize, 72, 90, 108].iter().find_map(|&at| {
        let block = &edid[at..at + 18];
        if block[..3] != [0, 0, 0] || block[3] != 0xfc {
            return None;
        }
        let name: String = block[5..].iter().take_while(|&&b| b != 0x0a && b != 0).map(|&b| b as char).collect();
        let name = name.trim().to_string();
        (!name.is_empty()).then_some(name)
    })
}

/// An integer setting from the `_XSETTINGS_SETTINGS` property's binary layout.
fn xsettings_integer(data: &[u8], wanted: &str) -> Option<i32> {
    let msb_first = *data.first()? == 1;
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = data.get(at..at + 2)?.try_into().ok()?;
        Some(if msb_first { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = data.get(at..at + 4)?.try_into().ok()?;
        Some(if msb_first { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    let pad = |length: usize| length.div_ceil(4) * 4;
    let count = u32_at(8)?;
    let mut at = 12;
    for _ in 0..count.min(4096) {
        let kind = *data.get(at)?;
        let name_length = u16_at(at + 2)? as usize;
        let name = data.get(at + 4..at + 4 + name_length)?;
        // The name is padded to four bytes and followed by the serial of its last change.
        at += 4 + pad(name_length) + 4;
        match kind {
            0 => {
                let value = u32_at(at)? as i32;
                if name == wanted.as_bytes() {
                    return Some(value);
                }
                at += 4;
            }
            1 => at += 4 + pad(u32_at(at)? as usize),
            2 => at += 8,
            _ => return None,
        }
    }
    None
}
