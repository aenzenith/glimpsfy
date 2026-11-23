//! What the app shell needs from a Linux desktop itself: the session (X11 or Wayland), the
//! pointer, the focus, the keys other apps receive, the window types that keep the panels where
//! they belong, the clipboard's files, dragging out, the tray host, the login item and the
//! system's languages. `desktop/mod.rs` reaches these as `platform::…`, and `windows.rs` offers
//! the same functions.
//!
//! GTK always runs on its X11 backend (natively, or through XWayland on a Wayland session; see
//! [`prepare_environment`]), because only X lets an app place its windows, keep them on top and
//! make them click-through. Raw X11 goes through connections of the shell's own (`x11rb`), so
//! the pointer and the focus can be read from any thread without the GTK main thread, and what
//! has to touch GTK runs on the main thread through `super::on_main`.
//!
//! Panels that must cover an exact place above everything (the overlays, the toast and the
//! recorder's panels) are override-redirect windows: no window manager moves, tiles, decorates
//! or animates them, and they stay above docks and panels, on GNOME, KDE, tiling and bare window
//! managers alike. Such a window gets no keyboard focus from the window manager, so the shell
//! gives it the focus itself ([`focus`]) and hands it back afterwards ([`restore_foreground`]).
//! Pins stay ordinary utility windows, which the user moves and clicks into like any other.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use glimpsfy_capture::{Display, LinuxSession, LogicalRect, PermissionState, WindowId};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tauri::{AppHandle, Manager, WebviewWindow};
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::permissions::Media;
use super::recording::InputWatch;
use crate::commands::windows::{Capabilities, DisplaySession, Os, SystemPane};
use crate::permissions::Permission;
use crate::recording::setup::GlassPiece;
use crate::trace;
use crate::windows::ToastGlass;

pub const OS: Os = Os::Linux;

/// Whether the empty toast window stays on screen, click-through. Not on Linux: a transparent
/// window is black without a compositor, and no window can be kept out of a capture, so one
/// that is always there would have to be hidden before every capture.
pub const TOAST_WAITS_ON_SCREEN: bool = false;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Runs `f` on the main thread and waits for its answer, at once when called there.
fn on_main_answer<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    if gtk::is_initialized_main_thread() {
        Some(f())
    } else {
        super::main_wait(f)
    }
}

// ------------------------------------------------------------------------------- the session

/// The session the desktop runs, by the rule the capture backend uses
/// (`glimpsfy_capture::linux_session`), so the shell and the backend never disagree.
pub fn session() -> Option<DisplaySession> {
    Some(match glimpsfy_capture::linux_session() {
        LinuxSession::X11 => DisplaySession::X11,
        LinuxSession::Wayland => DisplaySession::Wayland,
    })
}

fn x11() -> bool {
    session() == Some(DisplaySession::X11)
}

/// Before anything starts GTK: GTK is put on its X11 backend wherever an X server is reachable
/// (XWayland on a Wayland session), unless the user chose a backend. Without any display there
/// is nothing to show a window or a tray icon on, so the process says so and exits instead of
/// failing inside GTK's start.
pub fn prepare_environment() {
    if !is_set("DISPLAY") && !is_set("WAYLAND_DISPLAY") {
        eprintln!(
            "Glimpsfy needs a graphical session: neither DISPLAY nor WAYLAND_DISPLAY is set, so it has nowhere \
             to show its windows."
        );
        std::process::exit(1);
    }
    if is_set("DISPLAY") && std::env::var_os("GDK_BACKEND").is_none() {
        // Only the main thread runs this early, so no other thread reads the environment yet.
        std::env::set_var("GDK_BACKEND", "x11");
    }
}

/// Whether this is a Wayland session without XWayland, where Glimpsfy can show its tray icon and
/// its windows but cannot capture anything.
fn wayland_without_x() -> bool {
    !x11() && !is_set("DISPLAY")
}

/// What the shell can do on Linux. Wayland gives an app no window list, no pointer position, no
/// keys of other apps and no window on every workspace, so those are `false` there for good;
/// `glimpsfy --action <name>` and the GlobalShortcuts portal stand in for the shortcuts.
/// `capture_exclusion`, `native_glass`, `system_settings_panes` and `tray_popover` have no way on
/// Linux, and `camera_bubble` and `shutter_sound` are not written.
pub fn capabilities() -> Capabilities {
    let x11 = x11();
    Capabilities {
        window_capture: x11,
        capture_exclusion: false,
        non_activating_overlay: false,
        // The clicks and keys are drawn in a transparent window over the recorded area, which
        // needs a compositor (`prepare_panel`).
        input_overlays: x11 && composited(),
        camera_bubble: false,
        pin_all_spaces: x11,
        pin_lock: x11,
        native_glass: false,
        drag_out: true,
        copy_file: true,
        shutter_sound: false,
        system_settings_panes: false,
        screen_recording_permission: false,
        self_update: can_self_update(),
        global_shortcuts: x11,
        tray_popover: false,
        global_pointer: x11,
        transparent_windows: composited(),
        // A window that draws its own decorations draws its shadow too, and the capture keeps
        // it (`x11.rs`, `grab_window`).
        window_shadow: x11,
        subject_lift: false,
        // Wayland lets no app press keys in another, and X11 is not written yet.
        send_to_app: false,
        agents: false,
        scroll_capture: false,
        clean_desktop: false,
    }
}

// ----------------------------------------------------------------------------- X connection

/// The atoms the shell asks the window manager about.
struct Atoms {
    net_active_window: xproto::Atom,
    net_supported: xproto::Atom,
    net_wm_pid: xproto::Atom,
    net_system_tray: xproto::Atom,
}

/// A connection of the shell's own to the X server, apart from GTK's, so it answers from any
/// thread without waiting for the main thread.
struct X {
    conn: RustConnection,
    root: xproto::Window,
    atoms: Atoms,
}

impl X {
    fn open() -> Option<X> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let intern = |name: &str| -> Option<xproto::Atom> {
            Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
        };
        let atoms = Atoms {
            net_active_window: intern("_NET_ACTIVE_WINDOW")?,
            net_supported: intern("_NET_SUPPORTED")?,
            net_wm_pid: intern("_NET_WM_PID")?,
            net_system_tray: intern(&format!("_NET_SYSTEM_TRAY_S{screen}"))?,
        };
        Some(X { conn, root, atoms })
    }
}

static X_CONNECTION: Mutex<Option<X>> = Mutex::new(None);

/// Runs `work` on the shell's X connection, opened on first use and opened again once when it
/// broke. `None` without an X server or when the server refused the request (a window that
/// closed meanwhile).
fn with_x<T>(work: impl Fn(&X) -> Result<T, ReplyError>) -> Option<T> {
    if !is_set("DISPLAY") {
        return None;
    }
    let mut guard = lock(&X_CONNECTION);
    for _ in 0..2 {
        if guard.is_none() {
            *guard = X::open();
        }
        let x = guard.as_ref()?;
        match work(x) {
            Ok(value) => return Some(value),
            Err(ReplyError::ConnectionError(_)) => *guard = None,
            Err(ReplyError::X11Error(_)) => return None,
        }
    }
    None
}

fn first_u32(reply: &xproto::GetPropertyReply) -> Option<u32> {
    reply.value32().and_then(|mut values| values.next())
}

/// The window `_NET_ACTIVE_WINDOW` names, when there is one.
fn active_window(x: &X) -> Result<Option<xproto::Window>, ReplyError> {
    let reply =
        x.conn.get_property(false, x.root, x.atoms.net_active_window, xproto::AtomEnum::WINDOW, 0, 1)?.reply()?;
    Ok(first_u32(&reply).filter(|window| *window != 0))
}

/// Whether the window manager lists `atom` in `_NET_SUPPORTED`.
fn wm_supports(x: &X, atom: xproto::Atom) -> Result<bool, ReplyError> {
    let reply = x.conn.get_property(false, x.root, x.atoms.net_supported, xproto::AtomEnum::ATOM, 0, 4096)?.reply()?;
    Ok(reply.value32().is_some_and(|mut atoms| atoms.any(|known| known == atom)))
}

fn query_pointer() -> Option<xproto::QueryPointerReply> {
    with_x(|x| x.conn.query_pointer(x.root)?.reply())
}

// -------------------------------------------------------------------------------- geometry

/// GTK's one integer window scale, as last read from GDK (0 until then).
static WINDOW_SCALE: AtomicU32 = AtomicU32::new(0);

fn read_window_scale() -> Option<u32> {
    let display = gdk::Display::default()?;
    let monitor = display.primary_monitor().or_else(|| display.monitor(0))?;
    u32::try_from(monitor.scale_factor()).ok().filter(|scale| *scale > 0)
}

/// How many of tao's pixels (X pixels) make one logical point: GTK's integer window scale, which
/// is the capture backend's too. A display's own `scale` can differ on a Wayland session, where
/// the portal's pictures are measured in the compositor's pixels, so windows are placed with
/// this one.
pub fn window_scale() -> f64 {
    let known = WINDOW_SCALE.load(Ordering::SeqCst);
    if known > 0 {
        return f64::from(known);
    }
    match on_main_answer(read_window_scale).flatten() {
        Some(scale) => {
            WINDOW_SCALE.store(scale, Ordering::SeqCst);
            f64::from(scale)
        }
        None => 1.0,
    }
}

/// A display's top-left corner in the pixels tao places windows in. The capture crate's points
/// are X pixels divided by GTK's window scale, one space for every display whose origin is the X
/// root's, so the origin is the frame's times that scale. (`glimpsfy_capture::desktop` maps the
/// per-display pixel grid of Windows, where a frame's origin is already in pixels; it does not
/// fit this space.)
pub fn physical_origin(display: &Display) -> (f64, f64) {
    let scale = window_scale();
    (display.frame.x * scale, display.frame.y * scale)
}

/// The inverse of [`physical_origin`] for a monitor tao lists, whose scale is GTK's.
pub fn logical_origin(position: (f64, f64), scale: f64) -> (f64, f64) {
    (position.0 / scale, position.1 / scale)
}

/// The pointer in X root pixels, from any thread. `None` on a Wayland session: XWayland knows
/// the pointer only while it is over an X window and keeps a stale position otherwise, so the
/// shell treats it as unknown there (the primary display stands in for "the display under the
/// pointer", the toast takes clicks while it shows, the recorder's bar always takes the mouse).
pub fn pointer_physical(_app: &AppHandle) -> Option<(f64, f64)> {
    if !x11() {
        return None;
    }
    let reply = query_pointer()?;
    Some((f64::from(reply.root_x), f64::from(reply.root_y)))
}

/// The generation of the last display change, so only the last of a burst calls back.
static DISPLAY_CHANGES: AtomicU64 = AtomicU64::new(0);

/// How long the screen is given to settle after a monitor change before the shell rereads it.
const DISPLAY_SETTLE: Duration = Duration::from_millis(250);

/// Whether the X screen has a compositing manager now: 0 not read yet, 1 yes, 2 no.
static COMPOSITED: AtomicU8 = AtomicU8::new(0);

fn note_composited(screen: &gdk::Screen) {
    COMPOSITED.store(if screen.is_composited() { 1 } else { 2 }, Ordering::SeqCst);
}

/// Whether a transparent window shows what lies under it: a compositing manager runs. Without
/// one (a bare window manager) its transparent parts are black.
fn composited() -> bool {
    match COMPOSITED.load(Ordering::SeqCst) {
        1 => true,
        2 => false,
        _ => on_main_answer(|| gdk::Screen::default().map(|screen| screen.is_composited())).flatten().unwrap_or(true),
    }
}

/// Calls `changed` 250 ms after the last of GDK's `monitors-changed` and `size-changed` (a monitor
/// connected, removed, moved or rescaled, the screen resized), off the main thread, and follows
/// the compositor coming and going. On the main thread, once, at start.
pub fn watch_displays(app: &AppHandle, changed: fn(&AppHandle)) -> bool {
    if !gtk::is_initialized_main_thread() {
        return false;
    }
    let Some(screen) = gdk::Screen::default() else {
        return false;
    };
    note_composited(&screen);
    if let Some(scale) = read_window_scale() {
        WINDOW_SCALE.store(scale, Ordering::SeqCst);
    }
    let handler = move |app: AppHandle| {
        move |_: &gdk::Screen| {
            if let Some(scale) = read_window_scale() {
                WINDOW_SCALE.store(scale, Ordering::SeqCst);
            }
            let generation = DISPLAY_CHANGES.fetch_add(1, Ordering::SeqCst) + 1;
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(DISPLAY_SETTLE);
                if DISPLAY_CHANGES.load(Ordering::SeqCst) == generation {
                    changed(&app);
                }
            });
        }
    };
    screen.connect_monitors_changed(handler(app.clone()));
    screen.connect_size_changed(handler(app.clone()));
    screen.connect_composited_changed(note_composited);
    true
}

// --------------------------------------------------------------------------------- windows

/// The shell's panels, by their labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Overlay,
    Toast,
    Popover,
    Pin,
    Target,
    Bar,
    Canvas,
    BubbleControls,
    Other,
}

fn panel_of(label: &str) -> Panel {
    match label {
        crate::windows::TOAST => Panel::Toast,
        crate::popover::LABEL => Panel::Popover,
        crate::recording::TARGET => Panel::Target,
        crate::recording::BAR => Panel::Bar,
        crate::recording::CANVAS => Panel::Canvas,
        // The drawing rail is a keyless panel at the bars' level, as the bubble's controls are.
        crate::recording::BUBBLE_CONTROLS | crate::recording::RAIL => Panel::BubbleControls,
        _ if label.starts_with(crate::windows::OVERLAY_PREFIX) => Panel::Overlay,
        _ if label.starts_with(crate::pins::PREFIX) => Panel::Pin,
        _ => Panel::Other,
    }
}

/// Whether the panel is an override-redirect window, which the window manager never touches.
fn unmanaged(panel: Panel) -> bool {
    matches!(panel, Panel::Overlay | Panel::Toast | Panel::Target | Panel::Bar | Panel::Canvas | Panel::BubbleControls)
}

/// Called once for every panel the shell makes (overlay, toast, popover, pin, `rec-*`), before
/// it is placed or shown. GTK sizes a window that is not resizable to its content and ignores a
/// size set from code, and every panel is sized from code, so the panels are made resizable;
/// without a title bar they have no edge for the user to drag.
///
/// Every panel is kept off the taskbar and the pager and never takes the focus when it maps.
/// The overlays, the toast and the recorder's panels become override-redirect windows (made
/// before they ever map, which is when a window manager decides). Their type still tells an
/// XWayland compositor whether one wants the keyboard: the toast, the canvas and the bubble's
/// controls are notifications or utilities that never take it. Pins and the popover stay
/// managed, as utility windows, which tiling window managers float instead of tiling.
pub fn prepare_panel(window: &WebviewWindow) {
    let window = window.clone();
    super::on_main(move || {
        let Ok(gtk_window) = window.gtk_window() else {
            return;
        };
        // The panels are built unresizable, which tao applies again on the window's first
        // configure event, and GTK keeps an unresizable window at least as large as it was
        // made: the toast grew to its height but stayed 800 points wide, past the screen's edge.
        // Every configure event makes it resizable again, so `set_frame` can shrink it too.
        gtk_window.set_resizable(true);
        gtk_window.connect_configure_event(|gtk_window, _| {
            if !gtk_window.is_resizable() {
                gtk_window.set_resizable(true);
            }
            false
        });
        let panel = panel_of(window.label());
        let hint = match panel {
            Panel::Toast | Panel::Canvas => gdk::WindowTypeHint::Notification,
            Panel::Overlay | Panel::Target => gdk::WindowTypeHint::Normal,
            _ => gdk::WindowTypeHint::Utility,
        };
        gtk_window.set_type_hint(hint);
        gtk_window.set_skip_taskbar_hint(true);
        gtk_window.set_skip_pager_hint(true);
        gtk_window.set_focus_on_map(false);
        if matches!(panel, Panel::Toast | Panel::Canvas | Panel::BubbleControls) {
            gtk_window.set_accept_focus(false);
        }
        if unmanaged(panel) {
            gtk_window.realize();
            if let Some(surface) = gtk_window.window() {
                surface.set_override_redirect(true);
            }
        }
        // The canvas lies over the whole recorded rectangle while a take runs. Without a
        // compositor its transparent page is an opaque slab, which the file would show instead
        // of the screen, so there it is cut to nothing (and `input_overlays` is off).
        if matches!(panel, Panel::Canvas) && !composited() {
            shape_to(&window, Vec::new());
        }
        // A pin's window is its image with `pins::MARGIN` around it for the shadow. Without a
        // compositor that margin is opaque, so the window is cut to the image at every size the
        // pin takes (`shape_to` leaves it whole where a compositor runs).
        if matches!(panel, Panel::Pin) {
            let pin = window.clone();
            gtk_window.connect_configure_event(move |_, event| {
                let (width, height) = event.size();
                let margin = crate::pins::MARGIN;
                let image = LogicalRect {
                    x: margin,
                    y: margin,
                    width: (f64::from(width) - 2.0 * margin).max(0.0),
                    height: (f64::from(height) - 2.0 * margin).max(0.0),
                };
                shape_to(&pin, vec![image]);
                false
            });
        }
    });
}

/// Called once for an overlay after it got its display's frame. The overlay is an
/// override-redirect window of exactly its display's frame ([`prepare_panel`]), which covers the
/// display above every panel and dock without a fullscreen request: a fullscreen window is put
/// on a monitor of the window manager's choosing, may animate, and on GNOME sits under the top
/// bar while another monitor's overlay has the focus. Nothing more is needed here.
pub fn cover_display(_window: &WebviewWindow) {}

/// The window that has the keyboard focus, kept while an overlay or the recorder's target
/// borrows it: its X window id.
pub struct Foreground(xproto::Window);

/// The window that has the keyboard focus now, as `_NET_ACTIVE_WINDOW` names it. `None` on a
/// Wayland session, where the compositor gives the focus back by itself.
pub fn foreground() -> Option<Foreground> {
    if !x11() {
        return None;
    }
    with_x(active_window).flatten().map(Foreground)
}

/// Gives the keyboard focus back to `window`: a `_NET_ACTIVE_WINDOW` request to the window
/// manager as a pager sends it (source indication 2), or the focus set directly where no window
/// manager answers that request. A window that closed meanwhile is ignored.
pub fn restore_foreground(window: Foreground) {
    let _ = with_x(|x| {
        if wm_supports(x, x.atoms.net_active_window)? {
            let event = xproto::ClientMessageEvent::new(
                32,
                window.0,
                x.atoms.net_active_window,
                [2, x11rb::CURRENT_TIME, 0, 0, 0],
            );
            let mask = xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY;
            x.conn.send_event(false, x.root, mask, event)?.check()?;
        } else {
            x.conn.set_input_focus(xproto::InputFocus::PARENT, window.0, x11rb::CURRENT_TIME)?.check()?;
        }
        Ok(())
    });
}

/// The X window id of a window's GTK surface. On the main thread.
fn xid_of(window: &WebviewWindow) -> Option<xproto::Window> {
    let surface = window.gtk_window().ok()?.window()?;
    let x11 = surface.downcast::<gdkx11::X11Window>().ok()?;
    u32::try_from(x11.xid()).ok()
}

/// Gives an override-redirect panel the keyboard: it is raised, GTK's requests reach the server,
/// and then the focus is set on it. Managed windows ask the window manager as tao does. On the
/// main thread.
pub fn focus(window: &WebviewWindow) {
    if !unmanaged(panel_of(window.label())) {
        let _ = window.set_focus();
        return;
    }
    let Ok(gtk_window) = window.gtk_window() else {
        return;
    };
    let Some(surface) = gtk_window.window() else {
        return;
    };
    surface.raise();
    // The map and the raise went out on GTK's connection; the focus goes out on the shell's,
    // and the server refuses it for a window that is not mapped yet.
    surface.display().sync();
    let Some(xid) = xid_of(window) else {
        return;
    };
    let _ = with_x(|x| {
        x.conn.set_input_focus(xproto::InputFocus::PARENT, xid, x11rb::CURRENT_TIME)?.check()?;
        Ok(())
    });
}

/// Ctrl+W closes `window` (Settings, the history, the editor, the recorder), as Close Window does
/// on macOS. A GTK accelerator runs before the page gets the key, so a field in the page cannot
/// swallow it; the close goes through Tauri, so a window that holds its close still can.
pub fn close_on_ctrl_w(window: &WebviewWindow) {
    let window = window.clone();
    super::on_main(move || {
        let Ok(gtk_window) = window.gtk_window() else {
            return;
        };
        let accels = gtk::AccelGroup::new();
        let target = window.clone();
        accels.connect_accel_group(
            *gdk::keys::constants::w,
            gdk::ModifierType::CONTROL_MASK,
            gtk::AccelFlags::VISIBLE,
            move |_, _, _, _| {
                let _ = target.close();
                true
            },
        );
        gtk_window.add_accel_group(&accels);
    });
}

/// Shows a window and gives it the keyboard focus. On the main thread. An override-redirect
/// panel is mapped here at once, so the focus can follow it in the same turn.
pub fn show_active(window: &WebviewWindow) {
    if !unmanaged(panel_of(window.label())) {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Ok(gtk_window) = window.gtk_window() {
        gtk_window.show_all();
    }
    focus(window);
}

/// Shows a window without giving it the keyboard focus. On the main thread. Every panel maps
/// with `focus-on-map` off ([`prepare_panel`]), which window managers honour, and an
/// override-redirect panel is never focused by one.
pub fn show_inactive(window: &WebviewWindow) {
    if let Ok(gtk_window) = window.gtk_window() {
        gtk_window.set_focus_on_map(false);
    }
    let _ = window.show();
}

/// Keeps a window out of captures and recordings, and says whether the system took it. Linux
/// has no such thing on X11 or Wayland, so every own window is one `hide_for_capture` hides.
pub fn exclude_from_capture(_window: &WebviewWindow) -> bool {
    false
}

/// The capture backend's id of a window: its X window id, which is what the X11 backend lists
/// as a `WindowId` (the client window, not the window manager's frame around it). `None` on a
/// GTK that does not run on X.
pub fn window_id(window: &WebviewWindow) -> Option<WindowId> {
    let window = window.clone();
    on_main_answer(move || xid_of(&window)).flatten().map(WindowId)
}

// ----------------------------------------------------------------------------------- input

/// Whether Alt (`Mod1`) is held now, in any app, from any thread. Always `false` on a Wayland
/// session, where XWayland does not see the keys other apps receive.
pub fn option_key_down() -> bool {
    x11() && query_pointer().is_some_and(|reply| reply.mask.contains(xproto::KeyButMask::MOD1))
}

/// Whether the left mouse button is down now, in any app, from any thread.
pub fn left_button_down() -> bool {
    x11() && query_pointer().is_some_and(|reply| reply.mask.contains(xproto::KeyButMask::BUTTON1))
}

/// The process id of the app whose window has the keyboard focus: `_NET_WM_PID` of the window
/// `_NET_ACTIVE_WINDOW` names.
pub fn frontmost_pid() -> Option<i32> {
    if !x11() {
        return None;
    }
    with_x(|x| {
        let Some(window) = active_window(x)? else {
            return Ok(None);
        };
        let reply =
            x.conn.get_property(false, window, x.atoms.net_wm_pid, xproto::AtomEnum::CARDINAL, 0, 1)?.reply()?;
        Ok(first_u32(&reply).and_then(|pid| i32::try_from(pid).ok()).filter(|pid| *pid > 0))
    })
    .flatten()
}

/// Bumped by every watch and unwatch, so the watch thread of an earlier take stops.
static INPUT_WATCH: AtomicU64 = AtomicU64::new(0);

/// How long the watch thread rests when no event waits.
const INPUT_POLL: Duration = Duration::from_millis(8);

/// Watches clicks and keys in every other app while a take runs: XInput 2 raw button and key
/// presses selected on the root window, on a thread and an X connection of their own, until
/// [`unwatch_input`]. A left or right button going down calls `watch.on_click` with the pointer
/// in global logical points. A key going down (no repeats) calls `watch.on_keys` with its caps,
/// Ctrl, Alt, Shift and Super first and then the key, named as macOS names them; with
/// `only_shortcuts`, a key without Ctrl, Alt or Super is skipped. Nothing typed is kept or
/// logged. X11 only: Wayland gives no app the input of others.
pub fn watch_input(app: &AppHandle, watch: InputWatch) {
    let generation = INPUT_WATCH.fetch_add(1, Ordering::SeqCst) + 1;
    if !x11() || !(watch.clicks || watch.keys) {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new().name("glimpsfy-input".into()).spawn(move || {
        if let Err(error) = watch_input_on(&app, &watch, generation) {
            trace::log(format!("[glimpsfy] click and key watch: {error}"));
        }
    });
    if let Err(error) = spawned {
        trace::log(format!("[glimpsfy] click and key watch: {error}"));
    }
}

pub fn unwatch_input() {
    INPUT_WATCH.fetch_add(1, Ordering::SeqCst);
}

fn watch_input_on(app: &AppHandle, watch: &InputWatch, generation: u64) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots.get(screen).ok_or("the X screen is missing")?.root;
    let version = conn.xinput_xi_query_version(2, 2)?.reply()?;
    if (version.major_version, version.minor_version) < (2, 0) {
        return Err("the X server has no XInput 2".into());
    }
    let mut bits = 0u32;
    if watch.clicks {
        bits |= u32::from(xinput::XIEventMask::RAW_BUTTON_PRESS);
    }
    if watch.keys {
        bits |= u32::from(xinput::XIEventMask::RAW_KEY_PRESS) | u32::from(xinput::XIEventMask::RAW_KEY_RELEASE);
    }
    // Device 1 is "all master devices": the pointer and the keyboard the user works with.
    let select = xinput::EventMask { deviceid: 1, mask: vec![xinput::XIEventMask::from(bits)] };
    conn.xinput_xi_select_events(root, &[select])?.check()?;
    let keymap = if watch.keys { Some(Keymap::read(&conn)?) } else { None };
    let mut down: HashSet<u32> = HashSet::new();

    while INPUT_WATCH.load(Ordering::SeqCst) == generation {
        let Some(event) = conn.poll_for_event()? else {
            std::thread::sleep(INPUT_POLL);
            continue;
        };
        match event {
            // 1 is the left button and 3 the right one; 4 to 7 are the wheel.
            Event::XinputRawButtonPress(press) if matches!(press.detail, 1 | 3) => {
                let pointer = conn.query_pointer(root)?.reply()?;
                let point = super::to_logical(f64::from(pointer.root_x), f64::from(pointer.root_y));
                (watch.on_click)(app, point);
            }
            Event::XinputRawKeyPress(press) => {
                let repeat = press.flags.contains(xinput::KeyEventFlags::KEY_REPEAT);
                if !down.insert(press.detail) || repeat {
                    continue;
                }
                let Some(keymap) = &keymap else { continue };
                let mask = conn.query_pointer(root)?.reply()?.mask;
                if let Some(caps) = key_caps(keymap.keysym(press.detail), mask, watch.only_shortcuts) {
                    (watch.on_keys)(app, caps);
                }
            }
            Event::XinputRawKeyRelease(release) => {
                down.remove(&release.detail);
            }
            _ => {}
        }
    }
    Ok(())
}

/// The keyboard's keysyms, read once per take: each key as it reads without Shift in the first
/// layout, which is how a key cap shows it.
struct Keymap {
    min_keycode: u32,
    per_keycode: usize,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn read(conn: &RustConnection) -> Result<Keymap, ReplyError> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn.get_keyboard_mapping(min, max.saturating_sub(min).saturating_add(1))?.reply()?;
        Ok(Keymap {
            min_keycode: u32::from(min),
            per_keycode: usize::from(reply.keysyms_per_keycode),
            keysyms: reply.keysyms,
        })
    }

    fn keysym(&self, keycode: u32) -> u32 {
        let Some(index) = keycode.checked_sub(self.min_keycode) else {
            return 0;
        };
        self.keysyms.get(index as usize * self.per_keycode).copied().unwrap_or(0)
    }
}

/// The caps of a key press: the held modifiers and the key, or `None` for a modifier itself, a
/// key without a printable name, or a plain key when only shortcuts show.
fn key_caps(keysym: u32, mask: xproto::KeyButMask, only_shortcuts: bool) -> Option<Vec<String>> {
    let ctrl = mask.contains(xproto::KeyButMask::CONTROL);
    let alt = mask.contains(xproto::KeyButMask::MOD1);
    let shift = mask.contains(xproto::KeyButMask::SHIFT);
    let logo = mask.contains(xproto::KeyButMask::MOD4);
    if only_shortcuts && !(ctrl || alt || logo) {
        return None;
    }
    let named = match keysym {
        0xff0d | 0xff8d => "↩",
        0xff09 | 0xfe20 => "⇥",
        0x0020 => "Space",
        0xff08 => "⌫",
        0xff1b => "⎋",
        0xffff => "⌦",
        0xff51 => "←",
        0xff53 => "→",
        0xff54 => "↓",
        0xff52 => "↑",
        0xff50 => "↖",
        0xff57 => "↘",
        0xff55 => "⇞",
        0xff56 => "⇟",
        0xffbe..=0xffc9 => return Some(caps_with(ctrl, alt, shift, logo, format!("F{}", keysym - 0xffbe + 1))),
        _ => "",
    };
    let key = if named.is_empty() {
        // Latin-1 keysyms are their character; others carry a code point above 0x01000000.
        let code = match keysym {
            0x0021..=0x007e | 0x00a0..=0x00ff => keysym,
            0x0100_0100..=0x0110_ffff => keysym - 0x0100_0000,
            _ => return None,
        };
        let c = char::from_u32(code).filter(|c| !c.is_control())?;
        c.to_uppercase().collect::<String>()
    } else {
        named.to_string()
    };
    Some(caps_with(ctrl, alt, shift, logo, key))
}

fn caps_with(ctrl: bool, alt: bool, shift: bool, logo: bool, key: String) -> Vec<String> {
    let mut caps = Vec::with_capacity(5);
    for (on, cap) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (logo, "Super")] {
        if on {
            caps.push(cap.to_string());
        }
    }
    caps.push(key);
    caps
}

// ------------------------------------------------------------------- clipboard and dragging

/// A file's `file://` URI, as file managers write it.
fn file_uri(path: &Path) -> Result<String, String> {
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    glib::filename_to_uri(&path, None).map(|uri| uri.to_string()).map_err(|error| error.to_string())
}

/// Puts the file itself on the clipboard through GTK's clipboard, as GNOME's Files, Nemo and
/// Dolphin offer a copied file: `x-special/gnome-copied-files` ("copy", a line break and the
/// URI), `text/uri-list`, and the path as text for a text field. The clipboard's content lives
/// in this process, which keeps running in the tray.
pub fn copy_file(path: &Path) -> Result<(), String> {
    let uri = file_uri(path)?;
    let text = path.to_string_lossy().into_owned();
    let set = on_main_answer(move || {
        let clipboard = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
        let flags = gtk::TargetFlags::empty();
        let targets = [
            gtk::TargetEntry::new("x-special/gnome-copied-files", flags, 0),
            gtk::TargetEntry::new("text/uri-list", flags, 1),
            gtk::TargetEntry::new("UTF8_STRING", flags, 2),
            gtk::TargetEntry::new("text/plain;charset=utf-8", flags, 2),
            gtk::TargetEntry::new("text/plain", flags, 2),
        ];
        clipboard.set_with_data(&targets, move |_, data, info| match info {
            0 => data.set(&data.target(), 8, format!("copy\n{uri}").as_bytes()),
            1 => data.set(&data.target(), 8, format!("{uri}\r\n").as_bytes()),
            _ => {
                data.set_text(&text);
            }
        })
    });
    match set {
        Some(true) => Ok(()),
        Some(false) => Err("the clipboard refused the file".into()),
        None => Err("the main thread did not answer in time".into()),
    }
}

/// Puts a JPEG on the clipboard as an image. GdkPixbuf decodes it with the JPEG loader every
/// desktop ships, and GTK's clipboard offers the picture in each format it can write (PNG among
/// them), as a paste target asks.
pub fn copy_jpeg(bytes: &[u8]) -> Result<(), String> {
    let bytes = bytes.to_vec();
    on_main_answer(move || -> Result<(), String> {
        let loader = gtk::gdk_pixbuf::PixbufLoader::with_type("jpeg").map_err(|error| error.to_string())?;
        loader.write(&bytes).map_err(|error| error.to_string())?;
        loader.close().map_err(|error| error.to_string())?;
        let pixbuf = loader.pixbuf().ok_or("the JPEG could not be decoded")?;
        gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD).set_image(&pixbuf);
        Ok(())
    })
    .unwrap_or_else(|| Err("the main thread did not answer in time".into()))
}

/// Starts dragging `file` out of `window` as a GTK drag source offering `text/uri-list`, the copy
/// action only, with `image` scaled to `size` points under the pointer. The page calls this while
/// the button is down; the drag takes the pointer over from the webview's press.
pub fn drag_out(window: &WebviewWindow, file: &Path, image: &Path, size: (f64, f64)) -> Result<(), String> {
    let uri = file_uri(file)?;
    let (window, image) = (window.clone(), image.to_path_buf());
    on_main_answer(move || begin_drag(&window, uri, &image, size))
        .unwrap_or_else(|| Err("the main thread did not answer in time".into()))
}

fn begin_drag(window: &WebviewWindow, uri: String, image: &Path, size: (f64, f64)) -> Result<(), String> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let gtk_window = window.gtk_window().map_err(|error| error.to_string())?;
    let widget = gtk_window.upcast_ref::<gtk::Widget>().clone();
    let targets = gtk::TargetList::new(&[gtk::TargetEntry::new("text/uri-list", gtk::TargetFlags::OTHER_APP, 0)]);
    // The handlers belong to this drag only; the next drag connects its own.
    let handlers: Rc<RefCell<Vec<glib::SignalHandlerId>>> = Rc::default();
    let get = widget.connect_drag_data_get(move |_, _, data, _, _| {
        data.set_uris(&[uri.as_str()]);
    });
    let ended = {
        let (widget, handlers) = (widget.clone(), handlers.clone());
        move || {
            for handler in handlers.borrow_mut().drain(..) {
                widget.disconnect(handler);
            }
        }
    };
    let ended = Rc::new(RefCell::new(Some(ended)));
    let finish = {
        let ended = ended.clone();
        move || {
            if let Some(ended) = ended.borrow_mut().take() {
                ended();
            }
        }
    };
    let end = {
        let finish = finish.clone();
        widget.connect_drag_end(move |_, _| finish())
    };
    let failed = {
        let finish = finish.clone();
        widget.connect_drag_failed(move |_, _, _| {
            finish();
            glib::Propagation::Proceed
        })
    };
    handlers.borrow_mut().extend([get, end, failed]);
    let Some(context) = widget.drag_begin_with_coordinates(&targets, gdk::DragAction::COPY, 1, None, -1, -1) else {
        finish();
        return Err("GTK did not start the drag".into());
    };
    let scale = f64::from(widget.scale_factor().max(1));
    let (width, height) = ((size.0 * scale).round() as i32, (size.1 * scale).round() as i32);
    if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(image, width.max(1), height.max(1), true) {
        context.drag_set_icon_pixbuf(&pixbuf, pixbuf.width() / 2, pixbuf.height() / 2);
    }
    Ok(())
}

/// Plays the capture sound. Nothing plays yet, and `shutter_sound` is `false`.
///
/// Native: the sound theme's `screen-capture` event through libcanberra (`canberra-gtk-play -i
/// screen-capture` does the same), which stays silent when the desktop's event sounds are off.
pub fn shutter() {}

// ------------------------------------------------------------------------------------ system

/// The user's preferred languages, most wanted first, from the environment as gettext reads it:
/// `LANGUAGE` (a list), then `LC_ALL`, `LC_MESSAGES` and `LANG`, without the encoding and the
/// modifier (`tr_TR.UTF-8@euro` reads `tr_TR`).
pub fn preferred_languages() -> Vec<String> {
    let mut tags = Vec::new();
    let listed = std::env::var("LANGUAGE").unwrap_or_default();
    let single = ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().filter_map(|name| std::env::var(name).ok());
    for value in listed.split(':').map(str::to_string).chain(single) {
        let tag = value.split(['.', '@']).next().unwrap_or_default().trim();
        if !tag.is_empty() && tag != "C" && tag != "POSIX" && !tags.iter().any(|known| known == tag) {
            tags.push(tag.to_string());
        }
    }
    tags
}

/// Whether a desktop file is the default for the scheme of `url` (`x-scheme-handler/tel`), as
/// `xdg-open` would pick it. A desktop without `xdg-mime` cannot tell, and the link is offered.
pub fn can_open_url(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else { return false };
    let asked = std::process::Command::new("xdg-mime")
        .args(["query", "default", &format!("x-scheme-handler/{}", scheme.to_ascii_lowercase())])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match asked {
        Ok(out) if out.status.success() => !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        Ok(_) => false,
        Err(_) => true,
    }
}

/// The settings page that stands for a System Settings pane of macOS. Linux desktops share no
/// address for their settings, so there is none and `system_settings_panes` is `false`.
pub fn system_settings_url(_pane: SystemPane) -> Option<&'static str> {
    None
}

/// The settings page where a permission is granted. No Linux desktop has one for these.
pub fn permission_settings_url(_permission: Permission) -> Option<&'static str> {
    None
}

/// Whether the system lets apps use the microphone or the camera. Outside a sandbox nothing asks
/// (inside Flatpak the portal would, and Glimpsfy ships as a package and an AppImage).
pub fn media_permission(_media: Media) -> PermissionState {
    PermissionState::NotRequired
}

/// Only an AppImage can replace itself: the AppImage runtime names the file in `APPIMAGE`. A
/// `.deb` or `.rpm` install belongs to the package manager, so the updater only reports that a
/// newer version exists there.
pub fn can_self_update() -> bool {
    std::env::var_os("APPIMAGE").is_some()
}

/// An `Exec` value that starts `program` as one argument, quoted as the Desktop Entry spec asks
/// when the path holds a space or another reserved character, and escaped once more for the
/// key file's own string rules.
fn exec_value(program: &str) -> String {
    let reserved = |c: char| c.is_whitespace() || "\"'\\><~|&;$*?#()`".contains(c);
    let quoted = if program.contains(reserved) {
        let inner = program.replace('\\', "\\\\").replace('"', "\\\"").replace('`', "\\`").replace('$', "\\$");
        format!("\"{inner}\"")
    } else {
        program.to_string()
    };
    quoted.replace('\\', "\\\\").replace('%', "%%")
}

/// Start at login: an XDG autostart entry in `$XDG_CONFIG_HOME/autostart` under the name the
/// autostart plugin uses, written here because the plugin leaves the path unquoted, which breaks
/// an AppImage kept in a folder with a space in its name. It starts the AppImage itself
/// (`APPIMAGE`), not the temporary place the AppImage runtime mounts it at, and otherwise the
/// installed binary (`/usr/bin/glimpsfy` for the `.deb`).
pub fn set_login_item(app: &AppHandle, on: bool) -> Result<(), String> {
    let dir = dirs::config_dir().ok_or("the configuration folder is unknown")?.join("autostart");
    let path = dir.join(format!("{}.desktop", app.package_info().name));
    if !on {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        };
    }
    let program: PathBuf = match std::env::var_os("APPIMAGE").filter(|value| !value.is_empty()) {
        Some(appimage) => PathBuf::from(appimage),
        None => std::env::current_exe().map_err(|error| error.to_string())?,
    };
    let name = &app.package_info().name;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={name}\nComment={name} starts in the tray when you log in.\n\
         Exec={}\nIcon=glimpsfy\nStartupNotify=false\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exec_value(&program.to_string_lossy())
    );
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    // Written beside and moved into place, so a session that starts meanwhile never reads half.
    let partial = dir.join(format!(".{name}.desktop.partial"));
    std::fs::write(&partial, entry).map_err(|error| error.to_string())?;
    std::fs::rename(&partial, &path).map_err(|error| error.to_string())
}

/// What a Linux session needs at start, once, after the tray icon and the windows are made: the
/// capture backend keeps its portal state beside the settings, a Wayland session without
/// XWayland is told what Glimpsfy needs, and a desktop that shows no tray icon gets Settings, so
/// the app is never running invisibly.
pub fn start(app: &AppHandle) {
    if let Ok(dir) = crate::paths::config_dir(app) {
        glimpsfy_capture::linux_set_state_dir(dir);
    }
    trace::log(format!("[glimpsfy] {}", glimpsfy_capture::linux_diagnostics()));
    if wayland_without_x() {
        use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
        let language = app.state::<crate::AppState>().settings.get().general.language;
        let text = crate::i18n::text(language, "tray.needsXwayland", &[]);
        app.dialog().message(text).title("Glimpsfy").kind(MessageDialogKind::Warning).show(|_| {});
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // A login starts the panel and Glimpsfy together; the panel gets a moment to appear.
        std::thread::sleep(TRAY_HOST_GRACE);
        if !tray_host_present() {
            trace::log("[glimpsfy] no tray host shows the icon, so Settings opens");
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                crate::windows::open(&handle, crate::commands::windows::WindowKind::Settings)
            });
        }
    });
}

/// How long a start waits for a tray host before it decides there is none.
const TRAY_HOST_GRACE: Duration = Duration::from_secs(4);

/// Whether anything shows the tray icon: a StatusNotifier host behind the watcher on the session
/// bus (KDE Plasma, GNOME with the AppIndicator extension, Xfce, waybar, …), or an XEmbed tray
/// that the indicator library falls back to without a watcher. Stock GNOME has neither. It asks
/// the session bus, so it is called off the main thread.
pub fn tray_host_present() -> bool {
    let present = status_notifier_host().unwrap_or(false) || xembed_tray();
    TRAY_HOST.store(if present { 1 } else { 2 }, Ordering::SeqCst);
    present
}

/// What [`tray_host_present`] found last: 0 before it was asked, 1 a host, 2 none.
static TRAY_HOST: AtomicU8 = AtomicU8::new(0);

/// Whether the last look found no tray host. Read on any thread without asking the bus again.
pub fn tray_host_absent() -> bool {
    TRAY_HOST.load(Ordering::SeqCst) == 2
}

fn status_notifier_host() -> Option<bool> {
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    let timeout = 1000;
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let owned = bus
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(WATCHER,).to_variant()),
            Some(glib::VariantTy::new("(b)").ok()?),
            gio::DBusCallFlags::NONE,
            timeout,
            gio::Cancellable::NONE,
        )
        .ok()?;
    let (has_owner,): (bool,) = owned.get()?;
    if !has_owner {
        return Some(false);
    }
    // A watcher without a host (a session daemon that keeps one for a panel that is gone)
    // shows nothing; a watcher that does not say counts as showing.
    let registered = bus.call_sync(
        Some(WATCHER),
        "/StatusNotifierWatcher",
        "org.freedesktop.DBus.Properties",
        "Get",
        Some(&(WATCHER, "IsStatusNotifierHostRegistered").to_variant()),
        Some(glib::VariantTy::new("(v)").ok()?),
        gio::DBusCallFlags::NONE,
        timeout,
        gio::Cancellable::NONE,
    );
    Some(match registered {
        Ok(reply) => reply.child_value(0).as_variant().and_then(|value| value.get::<bool>()).unwrap_or(true),
        Err(_) => true,
    })
}

fn xembed_tray() -> bool {
    with_x(|x| Ok(x.conn.get_selection_owner(x.atoms.net_system_tray)?.reply()?.owner != x11rb::NONE)).unwrap_or(false)
}

// ----------------------------------------------------------------------- portal shortcuts

/// One action for the GlobalShortcuts portal: its id, what the desktop's dialog calls it, and
/// the keys Settings › Shortcuts gives it, in the portal's notation (`CTRL+SHIFT+a`).
pub struct PortalShortcut {
    pub id: String,
    pub description: String,
    pub trigger: Option<String>,
}

/// An accelerator of Settings › Shortcuts (`CommandOrControl+Shift+4`) in the notation of the
/// XDG shortcuts specification (`CTRL+SHIFT+4`), or `None` for a key it has no name for.
pub fn portal_trigger(accelerator: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut key = None;
    for token in accelerator.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        let modifier = match token.to_ascii_lowercase().as_str() {
            "commandorcontrol" | "cmdorctrl" | "control" | "ctrl" => Some("CTRL"),
            "alt" | "option" => Some("ALT"),
            "shift" => Some("SHIFT"),
            "super" | "meta" | "command" | "cmd" => Some("LOGO"),
            _ => None,
        };
        match modifier {
            Some(modifier) => parts.push(modifier.to_string()),
            None => key = Some(token),
        }
    }
    let key = key?;
    let name = match key {
        _ if key.len() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric()) => key.to_ascii_lowercase(),
        _ if key.starts_with("Key") && key.len() == 4 => key[3..].to_ascii_lowercase(),
        _ if key.starts_with("Digit") && key.len() == 6 => key[5..].to_string(),
        _ if key.starts_with('F') && key[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => key.to_string(),
        "PrintScreen" | "Print" => "Print".to_string(),
        "Space" => "space".to_string(),
        _ => return None,
    };
    parts.push(name);
    Some(parts.join("+"))
}

/// Binds `shortcuts` through the xdg-desktop-portal's GlobalShortcuts interface where the
/// desktop offers it (KDE Plasma, GNOME 48 and later, Hyprland), and calls `activated` with a
/// shortcut's id each time the desktop reports it pressed. The desktop asks the user to confirm
/// the keys the first time. Where the portal is absent this ends quietly: the user binds
/// `glimpsfy --action <name>` in the desktop's keyboard settings instead. On a thread of its own,
/// for as long as the app runs.
pub fn bind_portal_shortcuts(shortcuts: Vec<PortalShortcut>, activated: impl Fn(&str) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("glimpsfy-portal-shortcuts".into()).spawn(move || {
        let outcome = futures_lite::future::block_on(listen_portal(shortcuts, activated));
        if let Err(error) = outcome {
            trace::log(format!("[glimpsfy] global shortcuts portal: {error}"));
        }
    });
    if let Err(error) = spawned {
        trace::log(format!("[glimpsfy] global shortcuts portal: {error}"));
    }
}

async fn listen_portal(shortcuts: Vec<PortalShortcut>, activated: impl Fn(&str)) -> Result<(), ashpd::Error> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_lite::StreamExt;

    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    let wanted: Vec<NewShortcut> = shortcuts
        .iter()
        .map(|shortcut| {
            NewShortcut::new(shortcut.id.as_str(), shortcut.description.as_str())
                .preferred_trigger(shortcut.trigger.as_deref())
        })
        .collect();
    let bound = portal.bind_shortcuts(&session, &wanted, None).await?.response()?;
    trace::log(format!("[glimpsfy] global shortcuts portal bound {} shortcuts", bound.shortcuts().len()));
    let mut presses = Box::pin(portal.receive_activated().await?);
    let mut last: Option<(String, Instant)> = None;
    while let Some(press) = presses.next().await {
        // Some desktops report a press twice in a row; one is enough.
        let id = press.shortcut_id().to_string();
        if last.as_ref().is_some_and(|(known, at)| *known == id && at.elapsed() < Duration::from_millis(150)) {
            continue;
        }
        activated(&id);
        last = Some((id, Instant::now()));
    }
    Ok(())
}

// --------------------------------------------------------------------------------- tray icon

/// The tray glyph: white with a dark outline, which reads on a light and on a dark panel alike.
/// AppIndicator hosts differ (GNOME's extension, KDE, Xfce, Cinnamon) and none tells an app its
/// panel's colour, so no glyph is picked by theme here.
pub fn tray_glyph() -> &'static [u8] {
    include_bytes!("../../icons/tray-on-any.png")
}

/// Calls `changed` when the panel's theme changes. The Linux glyph does not depend on it.
pub fn watch_panel_theme(_changed: Box<dyn Fn() + Send>) {}

// -------------------------------------------------------------------------------------- glass

/// Cuts a window to `rects` (points from its top-left corner) where no compositor runs: there a
/// transparent window is black, so only the parts the page draws on may be on screen. With a
/// compositor the window keeps its whole shape.
///
/// The X server gets the shape directly. GDK's own (`gdk_window_shape_combine_region`) also
/// clips GTK's drawing, and a WebKit view mapped under a partial GDK shape never painted inside
/// it: the toast stayed black until the shape had once been the whole window.
fn shape_to(window: &WebviewWindow, rects: Vec<LogicalRect>) {
    use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
    if composited() {
        return;
    }
    let window = window.clone();
    super::on_main(move || {
        let Some(xid) = xid_of(&window) else {
            return;
        };
        // The window was made on GDK's connection; what GDK still holds must reach the server
        // before a request about the window comes on this one.
        if let Ok(gtk_window) = window.gtk_window() {
            gtk_window.display().flush();
        }
        let scale = window_scale();
        let pixels = |value: f64| (value * scale).round();
        let rects: Vec<xproto::Rectangle> = rects
            .iter()
            .map(|rect| xproto::Rectangle {
                x: pixels(rect.x).clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
                y: pixels(rect.y).clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
                width: pixels(rect.width).clamp(0.0, f64::from(u16::MAX)) as u16,
                height: pixels(rect.height).clamp(0.0, f64::from(u16::MAX)) as u16,
            })
            .collect();
        with_x(|x| {
            x.conn.shape_rectangles(SO::SET, SK::BOUNDING, xproto::ClipOrdering::UNSORTED, xid, 0, 0, &rects)?.check()
        });
    });
}

/// Linux has no blur behind a window an app can ask for (KDE's `_KDE_NET_WM_BLUR_BEHIND_REGION`
/// aside), so the toast page draws its own background and `native_glass` is `false`. Without a
/// compositor the toast window is cut to the toasts it shows, which keeps the empty, black rest
/// of its tall window off the screen.
pub fn toast_glass(window: &WebviewWindow, glass: &[ToastGlass]) {
    if composited() {
        return;
    }
    let Ok(size) = window.inner_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0).max(1.0);
    let (width, height) = (f64::from(size.width) / scale, f64::from(size.height) / scale);
    let rects = glass
        .iter()
        .filter(|toast| toast.alpha > 0.1)
        .map(|toast| LogicalRect {
            x: width - toast.right - toast.width,
            y: height - toast.bottom - toast.height,
            width: toast.width,
            height: toast.height,
        })
        .collect();
    shape_to(window, rects);
}

/// The bar's page draws its own background, as the toast's does; without a compositor the bar
/// window is cut to its pieces of glass.
pub fn backdrop_show(bar: &WebviewWindow, _frame: LogicalRect, pieces: &[GlassPiece]) {
    shape_to(bar, pieces.iter().map(|piece| piece.rect).collect());
}

pub fn backdrop_hide() {}

// --------------------------------------------------------------------------------- the bubble

/// Shows the webcam bubble. Not written yet (`camera_bubble` is `false`).
///
/// Native: an undecorated GTK window above the others that a recording shows (it is not one of
/// the windows `hide_for_capture` hides), at `frame` in global logical points (place it with
/// `super::to_physical`), shaped to a circle when `round` and to corners of 22 % of its shorter
/// side otherwise, showing the mirrored preview of the camera `device` (a V4L2 device or a
/// PipeWire node through GStreamer, or the default camera), the camera opened the first time and
/// when the device changes. It never takes the focus.
pub fn bubble_show(_device: Option<&str>, _frame: LogicalRect, _round: bool) -> Result<(), String> {
    Err("the webcam bubble is not implemented on Linux yet".into())
}

/// Moves the bubble to `frame`, sliding when `animate`.
pub fn bubble_move(_frame: LogicalRect, _round: bool, _animate: bool) {}

/// Hides the bubble and turns the camera off.
pub fn bubble_hide() {}

/// The capture backend's id of the bubble's window, which a window recording must show.
pub fn bubble_window_id() -> Option<WindowId> {
    None
}
