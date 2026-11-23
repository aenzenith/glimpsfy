//! What the app shell needs from Windows itself: panels that stay out of Alt+Tab and show
//! without taking the keyboard, their place on monitors of different scales, the foreground
//! window, the pointer and the keys, a take's click and key hooks, the capture exclusion, the
//! display and taskbar-theme watch, files on the clipboard and dragged out, the login item, the
//! settings pages and the system's languages. `desktop/mod.rs` reaches these as `platform::…`,
//! and `linux.rs` offers the same functions.
//!
//! Every call that touches one of the app's windows runs on the main thread, which owns them all
//! (subclassing and the display affinity work only there). The display and theme watch and a
//! take's input hooks each run a message loop on a thread of their own, so they never wait for
//! the main thread and the main thread never waits for them.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use glimpsfy_capture::{Display, LogicalPoint, LogicalRect, PermissionState, WindowId};
use tauri::{AppHandle, WebviewWindow};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GlobalFree, ERROR_CLASS_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, HANDLE, HGLOBAL, HWND,
    LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, CreateDIBSection, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, GetSysColor,
    MonitorFromWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, COLOR_WINDOW, DIB_RGB_COLORS, HBITMAP, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, RGBQUAD,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevationType, TokenElevationTypeFull, TokenElevationTypeLimited, TOKEN_ELEVATION_TYPE,
    TOKEN_QUERY,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::{CF_HDROP, DROPEFFECT_COPY};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcessToken,
};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyboardLayout, MapVirtualKeyExW, MAPVK_VK_TO_CHAR, VK_ADD, VK_BACK, VK_CAPITAL, VK_CONTROL,
    VK_DECIMAL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F24, VK_HOME, VK_LBUTTON, VK_LCONTROL,
    VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_MULTIPLY, VK_NEXT, VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD9, VK_PRIOR,
    VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SCROLL, VK_SHIFT, VK_SPACE, VK_SUBTRACT, VK_TAB,
    VK_UP,
};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass, DROPFILES,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetAncestor, GetClientRect, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, KillTimer, PeekMessageW, PostThreadMessageW, RegisterClassExW, SetForegroundWindow, SetTimer,
    SetWindowDisplayAffinity, SetWindowLongW, SetWindowPos, SetWindowsHookExW, ShowWindow, SystemParametersInfoW,
    TranslateMessage, UnhookWindowsHookEx, WindowFromPoint, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, HC_ACTION,
    KBDLLHOOKSTRUCT, MONITORINFOF_PRIMARY, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, SPI_GETHIGHCONTRAST, STYLESTRUCT,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SW_SHOWNA, WDA_EXCLUDEFROMCAPTURE, WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOWPOS, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_NCDESTROY, WM_QUIT, WM_RBUTTONDOWN, WM_SETTINGCHANGE, WM_STYLECHANGING,
    WM_SYSCOLORCHANGE, WM_SYSKEYDOWN, WM_THEMECHANGED, WM_TIMER, WM_USER, WM_WINDOWPOSCHANGING, WNDCLASSEXW,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};

use super::permissions::Media;
use super::recording::InputWatch;
use crate::commands::windows::{Capabilities, DisplaySession, Os, SystemPane};
use crate::permissions::Permission;
use crate::recording::setup::GlassPiece;
use crate::trace;
use crate::windows::ToastGlass;

pub const OS: Os = Os::Windows;

/// Whether the empty toast window stays on screen, click-through, so its page keeps drawing at
/// full speed and the first toast moves as smoothly as the others (as on macOS). It needs a
/// transparent window that is proven to show nothing while empty; turn it on after checking that
/// on a real machine. Until then the window hides while it has no toast.
pub const TOAST_WAITS_ON_SCREEN: bool = false;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Windows has one kind of session.
pub fn session() -> Option<DisplaySession> {
    None
}

/// What the shell can do on Windows. Still `false`: `camera_bubble` ([`bubble_show`]),
/// `native_glass` ([`toast_glass`], [`backdrop_show`]), `shutter_sound` ([`shutter`]),
/// `non_activating_overlay` (an overlay that takes the keys activates Glimpsfy, which Windows
/// has no way around) and `pin_all_spaces` (Tauri cannot show a window on every virtual
/// desktop, and `IVirtualDesktopPinnedApps` is an undocumented interface that changes between
/// builds).
pub fn capabilities() -> Capabilities {
    Capabilities {
        window_capture: true,
        // Known once the first window was marked, which happens at start (the toast).
        capture_exclusion: EXCLUSION.load(Ordering::SeqCst) == EXCLUSION_WORKS,
        non_activating_overlay: false,
        input_overlays: true,
        camera_bubble: false,
        pin_all_spaces: false,
        pin_lock: true,
        native_glass: false,
        drag_out: true,
        copy_file: true,
        shutter_sound: false,
        system_settings_panes: true,
        screen_recording_permission: false,
        self_update: true,
        global_shortcuts: true,
        tray_popover: true,
        global_pointer: true,
        // DWM always composites.
        transparent_windows: true,
        // DWM draws the shadow outside the window, where neither capture path reads.
        window_shadow: false,
        subject_lift: false,
        send_to_app: false,
        agents: false,
        scroll_capture: false,
    }
}

// -------------------------------------------------------------------------------- geometry

/// A display's top-left corner in virtual-screen pixels, as the capture crate's mapping defines
/// it on Windows (`glimpsfy_capture::desktop`): monitors of different scales share no space in
/// points, so `Display.frame.x/y` is that pixel corner.
pub fn physical_origin(display: &Display) -> (f64, f64) {
    let frame = glimpsfy_capture::desktop::physical_frame(display);
    (f64::from(frame.x), f64::from(frame.y))
}

/// The inverse of [`physical_origin`] for a monitor tao lists: its pixel position is its frame's
/// origin.
pub fn logical_origin(position: (f64, f64), _scale: f64) -> (f64, f64) {
    position
}

/// The pointer in virtual-screen pixels. Callable from any thread, and it never waits for the
/// main thread.
pub fn pointer_physical(_app: &AppHandle) -> Option<(f64, f64)> {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid place for the answer.
    let read = unsafe { GetCursorPos(&mut point) };
    (read != 0).then(|| (f64::from(point.x), f64::from(point.y)))
}

// ------------------------------------------------------------------ display and theme watch
//
// One hidden top-level window on a thread of its own hears what Windows broadcasts: a display
// change, a scale change and every setting change (the taskbar's theme among them). A
// message-only window would hear none of these, since broadcasts go to top-level windows only.
// The window is never shown, and it also compares the monitors every few seconds, because a
// scale change of a monitor the window is not on reaches it by no message. Should the window not
// be made, the thread reads the monitors and the theme every few seconds instead; either way
// nothing waits for the main thread.

/// What the watch tells the display thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DisplayNotice {
    /// Only a work area changed: the taskbar moved, hid or came back.
    WorkAreas,
    /// A monitor was connected, removed, moved, rescaled or made the primary one.
    Monitors,
}

/// One monitor as the watch compares it.
#[derive(Clone, PartialEq, Eq)]
struct MonitorShape {
    device: [u16; 32],
    bounds: (i32, i32, i32, i32),
    work: (i32, i32, i32, i32),
    dpi: u32,
    primary: bool,
}

/// Where the watch sends display changes, once [`watch_displays`] listens.
static DISPLAY_NOTICES: Mutex<Option<Sender<DisplayNotice>>> = Mutex::new(None);
/// What draws the tray icon again, once [`watch_panel_theme`] listens.
static THEME_CHANGED: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
/// The taskbar's look as last seen: 0 not read yet, 1 dark, 2 light.
static TASKBAR_LIGHT: AtomicU8 = AtomicU8::new(0);

const WATCH_CLASS: &str = "GlimpsfySystemWatch";
const DEBOUNCE_TIMER: usize = 1;
const POLL_TIMER: usize = 2;
/// A monitor being plugged in sends a burst of messages; the layout is read once, this long
/// after the last of them, as `macos::watch_displays` waits.
const DEBOUNCE: u32 = 250;
const POLL: u32 = 3000;

thread_local! {
    /// The monitors as the watch thread last saw them.
    static LAST_SHAPES: RefCell<Vec<MonitorShape>> = const { RefCell::new(Vec::new()) };
}

/// Starts the watch thread once and says whether it runs.
fn system_watch() -> bool {
    static STARTED: OnceLock<bool> = OnceLock::new();
    *STARTED.get_or_init(|| {
        std::thread::Builder::new().name("glimpsfy-system-watch".into()).spawn(run_system_watch).is_ok()
    })
}

fn run_system_watch() {
    let shapes = monitor_shapes();
    LAST_SHAPES.with(|last| *last.borrow_mut() = shapes);
    let window = create_watch_window();
    if window.is_null() {
        trace::log("[glimpsfy] system watch: no window, so displays and the theme are polled");
        loop {
            std::thread::sleep(Duration::from_millis(u64::from(POLL)));
            displays_check();
            theme_check();
        }
    }
    // SAFETY: the window belongs to this thread; a timer without a procedure posts WM_TIMER.
    unsafe { SetTimer(window, POLL_TIMER, POLL, None) };
    let mut message = MSG::default();
    // SAFETY: `message` is a valid place for each message; the loop ends with WM_QUIT or an error.
    while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
        // SAFETY: a message this thread's queue just handed out.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn create_watch_window() -> HWND {
    let class = wide(WATCH_CLASS);
    // SAFETY: a null name asks for the module of the running executable.
    let instance = unsafe { GetModuleHandleW(null()) };
    let description = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(watch_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    // SAFETY: the description and the class name it points at outlive the call.
    let atom = unsafe { RegisterClassExW(&description) };
    // SAFETY: a plain query of this thread's last error.
    if atom == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
        return null_mut();
    }
    // SAFETY: the class is registered and both names are NUL-terminated. A popup that is never
    // shown takes no room, no focus and no taskbar button.
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    }
}

unsafe extern "system" fn watch_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            // Setting the timer again restarts it, which is the debounce.
            // SAFETY: the watch window, on its own thread.
            unsafe { SetTimer(window, DEBOUNCE_TIMER, DEBOUNCE, None) };
            if message == WM_SETTINGCHANGE {
                theme_check();
            }
        }
        WM_SYSCOLORCHANGE | WM_THEMECHANGED => theme_check(),
        WM_TIMER => {
            if wparam == DEBOUNCE_TIMER {
                // SAFETY: the watch window's own timer.
                unsafe { KillTimer(window, DEBOUNCE_TIMER) };
            }
            displays_check();
            return 0;
        }
        _ => {}
    }
    // SAFETY: what the watch does not handle gets the default handling.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// The monitors now, in a stable order.
fn monitor_shapes() -> Vec<MonitorShape> {
    unsafe extern "system" fn add(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the vector `monitor_shapes` passed, alive and unborrowed for the whole
        // enumeration.
        let shapes = unsafe { &mut *(data as *mut Vec<MonitorShape>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: a MONITORINFOEXW with its size set, which the call accepts in place of a
        // MONITORINFO.
        if unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>()) } == 0 {
            return 1;
        }
        let (mut dpi, mut unused) = (0u32, 0u32);
        // SAFETY: both are valid places for the answers.
        if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused) } != 0 {
            dpi = 0;
        }
        let (bounds, work) = (info.monitorInfo.rcMonitor, info.monitorInfo.rcWork);
        shapes.push(MonitorShape {
            device: info.szDevice,
            bounds: (bounds.left, bounds.top, bounds.right, bounds.bottom),
            work: (work.left, work.top, work.right, work.bottom),
            dpi,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
        1
    }
    let mut shapes: Vec<MonitorShape> = Vec::new();
    // SAFETY: the callback only writes into `shapes`, which outlives the call.
    unsafe { EnumDisplayMonitors(null_mut(), null(), Some(add), (&mut shapes as *mut Vec<MonitorShape>) as LPARAM) };
    shapes.sort_by_key(|shape| shape.bounds);
    shapes
}

fn displays_check() {
    let now = monitor_shapes();
    // No monitor at all is a layout in the middle of changing (sleep, a remote session
    // reconnecting), not one to place windows on.
    if now.is_empty() {
        return;
    }
    let notice = LAST_SHAPES.with(|last| {
        let mut last = last.try_borrow_mut().ok()?;
        let same_monitors = last.len() == now.len()
            && last
                .iter()
                .zip(&now)
                .all(|(a, b)| a.device == b.device && a.bounds == b.bounds && a.dpi == b.dpi && a.primary == b.primary);
        let notice = if !same_monitors {
            Some(DisplayNotice::Monitors)
        } else if last.iter().zip(&now).any(|(a, b)| a.work != b.work) {
            Some(DisplayNotice::WorkAreas)
        } else {
            None
        };
        *last = now;
        notice
    });
    if let (Some(notice), Some(notices)) = (notice, lock(&DISPLAY_NOTICES).as_ref()) {
        let _ = notices.send(notice);
    }
}

/// Calls `changed` 250 ms after a monitor is connected, removed, moved or rescaled, from a thread
/// of its own, as `macos::watch_displays` does, and reads the work areas again when only the
/// taskbar moved; then says `true`. Only when no thread can be started does it say `false`, and
/// `desktop/mod.rs` compares tao's monitors every two seconds instead.
pub fn watch_displays(app: &AppHandle, changed: fn(&AppHandle)) -> bool {
    let (notices, received) = mpsc::channel::<DisplayNotice>();
    let app = app.clone();
    let worker = std::thread::Builder::new().name("glimpsfy-displays".into()).spawn(move || {
        while let Ok(first) = received.recv() {
            // Changes that came while the last one was handled are handled once.
            match received.try_iter().fold(first, Ord::max) {
                DisplayNotice::Monitors => changed(&app),
                DisplayNotice::WorkAreas => super::refresh_screens(),
            }
        }
    });
    if worker.is_err() {
        return false;
    }
    *lock(&DISPLAY_NOTICES) = Some(notices);
    if system_watch() {
        return true;
    }
    // Dropping the sender ends the worker.
    *lock(&DISPLAY_NOTICES) = None;
    false
}

// ----------------------------------------------------------------------------------- panels
//
// tao keeps every window's styles in flags of its own and writes the whole extended style again
// on each change (click-through, always on top, focusable), so a style set once from outside is
// lost at the next change. A subclass of each panel corrects every such write instead, and it
// also holds a placement against the size and place tao's scale-change handling picks.

const SUBCLASS_ID: usize = 0x676C_6D70;
/// The subclass's reference data: the window is one of the shell's panels.
const PANEL: usize = 1;

/// A rectangle as `(left, top, right, bottom)`.
type Edges = (i32, i32, i32, i32);

/// Where the shell wants a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The client area (what the page fills) at x, y with a width and a height, in
    /// virtual-screen pixels. The popover's shadow keeps invisible borders around it.
    Client(i32, i32, i32, i32),
    /// The outer frame's top-left corner; the window keeps its size in points.
    Corner(i32, i32),
}

/// The outer frame a move must end at while the shell places a window.
#[derive(Clone, Copy, PartialEq)]
struct Pin {
    window: HWND,
    x: i32,
    y: i32,
    /// `None` leaves the size to whoever moves the window.
    size: Option<(i32, i32)>,
}

/// Where the shell last put a window, and the scale it put it there for.
#[derive(Clone, Copy)]
struct Placed {
    window: HWND,
    target: Target,
    outer: Edges,
    dpi: u32,
}

thread_local! {
    /// The window [`show_inactive`] shows now.
    static SHOWING: Cell<HWND> = const { Cell::new(null_mut()) };
    /// The window the shell moves now and where it must land.
    static PLACING: Cell<Option<Pin>> = const { Cell::new(None) };
    static PLACED: RefCell<Vec<Placed>> = const { RefCell::new(Vec::new()) };
}

fn hwnd(window: &WebviewWindow) -> Option<HWND> {
    window.hwnd().ok().map(|handle| handle.0)
}

/// Subclasses `window` once; `panel` turns the panel's styles on for a window subclassed
/// before as an ordinary one. On the window's thread, as subclassing requires.
fn ensure_subclass(window: HWND, panel: bool) {
    let mut data = 0usize;
    // SAFETY: the window lives on this thread, and `data` is a valid place for the answer.
    let installed = unsafe { GetWindowSubclass(window, Some(subclass_proc), SUBCLASS_ID, &mut data) } != 0;
    let wanted = if panel { data | PANEL } else { data };
    if !installed || wanted != data {
        // SAFETY: the same window and thread; installing again only updates the reference data.
        unsafe { SetWindowSubclass(window, Some(subclass_proc), SUBCLASS_ID, wanted) };
    }
}

unsafe extern "system" fn subclass_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_STYLECHANGING if lparam != 0 => {
            // SAFETY: for this message `lparam` points at the styles about to be set, which the
            // handler may change.
            let styles = unsafe { &mut *(lparam as *mut STYLESTRUCT) };
            let index = wparam as i32;
            if index == GWL_EXSTYLE && data & PANEL != 0 {
                // Out of Alt+Tab; tao's taskbar flag would put the window back in with
                // WS_EX_APPWINDOW.
                styles.styleNew = (styles.styleNew | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
            } else if index == GWL_STYLE && SHOWING.get() == window {
                // tao clears WS_VISIBLE of a window it believes hidden before it shows it, which
                // would hide the window [`show_inactive`] has just shown and show it again with
                // activation.
                // SAFETY: a plain query of the window this message is for.
                if unsafe { IsWindowVisible(window) } != 0 {
                    styles.styleNew |= WS_VISIBLE;
                }
            }
        }
        WM_WINDOWPOSCHANGING if lparam != 0 => {
            // SAFETY: for this message `lparam` points at the move about to happen, which the
            // handler may change.
            let position = unsafe { &mut *(lparam as *mut WINDOWPOS) };
            if SHOWING.get() == window && position.flags & SWP_SHOWWINDOW != 0 {
                position.flags |= SWP_NOACTIVATE;
            }
            if let Some(pin) = PLACING.get().filter(|pin| pin.window == window) {
                hold(position, pin);
            }
        }
        WM_DPICHANGED => {
            if let Some(result) = catch_up(window, message, wparam, lparam) {
                return result;
            }
        }
        WM_NCDESTROY => {
            forget(window);
            // SAFETY: a subclass is removed from its window before the window is gone.
            unsafe { RemoveWindowSubclass(window, Some(subclass_proc), SUBCLASS_ID) };
        }
        _ => {}
    }
    // SAFETY: the next procedure in the window's chain gets the message, changed or not.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

/// Makes a move or resize end at `pin`; a call that only restacks or redraws is left alone.
fn hold(position: &mut WINDOWPOS, pin: Pin) {
    if position.flags & SWP_NOMOVE != 0 && position.flags & SWP_NOSIZE != 0 {
        return;
    }
    position.x = pin.x;
    position.y = pin.y;
    position.flags &= !SWP_NOMOVE;
    if let Some((width, height)) = pin.size {
        position.cx = width;
        position.cy = height;
        position.flags &= !SWP_NOSIZE;
    }
}

/// A scale change that only catches up with where the shell put the window (a hidden window
/// gets it when it shows) keeps that place and size: tao would resize the window to keep its old
/// size in points, and on Windows 10 nudge it. Any other scale change (the user dragged the
/// window to another monitor, a monitor was rescaled) is tao's to handle.
fn catch_up(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if PLACING.get().is_some_and(|pin| pin.window == window) {
        return None;
    }
    let placed = placed(window)?;
    let dpi = (wparam & 0xFFFF) as u32;
    if dpi != placed.dpi || window_rect(window) != Some(placed.outer) {
        forget(window);
        return None;
    }
    let (left, top, right, bottom) = placed.outer;
    let size = matches!(placed.target, Target::Client(..)).then_some((right - left, bottom - top));
    let previous = PLACING.replace(Some(Pin { window, x: left, y: top, size }));
    // SAFETY: the next procedure in the window's chain (tao's) handles the change under the pin.
    let result = unsafe { DefSubclassProc(window, message, wparam, lparam) };
    PLACING.set(previous);
    // The borders around the popover's client area scale with the monitor.
    if matches!(placed.target, Target::Client(..)) {
        place_now(window, placed.target);
    }
    Some(result)
}

fn placed(window: HWND) -> Option<Placed> {
    PLACED.with(|placed| placed.try_borrow().ok()?.iter().find(|p| p.window == window).copied())
}

fn remember(entry: Placed) {
    PLACED.with(|placed| {
        if let Ok(mut placed) = placed.try_borrow_mut() {
            placed.retain(|p| p.window != entry.window);
            placed.push(entry);
        }
    });
}

fn forget(window: HWND) {
    PLACED.with(|placed| {
        if let Ok(mut placed) = placed.try_borrow_mut() {
            placed.retain(|p| p.window != window);
        }
    });
}

fn window_rect(window: HWND) -> Option<Edges> {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid place for the answer; a stale handle only fails.
    (unsafe { GetWindowRect(window, &mut rect) } != 0).then_some((rect.left, rect.top, rect.right, rect.bottom))
}

/// The client area in virtual-screen pixels.
fn client_rect(window: HWND) -> Option<Edges> {
    let mut rect = RECT::default();
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: both are valid places for the answers; a stale handle only fails.
    let read = unsafe { GetClientRect(window, &mut rect) != 0 && ClientToScreen(window, &mut origin) != 0 };
    read.then_some((origin.x, origin.y, origin.x + rect.right, origin.y + rect.bottom))
}

/// The scale the window is due on: that of the monitor that holds most of it.
fn monitor_dpi(window: HWND) -> u32 {
    // SAFETY: plain queries; the nearest monitor exists while any is connected.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let (mut dpi, mut unused) = (0u32, 0u32);
    // SAFETY: both are valid places for the answers.
    let read = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused) };
    if read == 0 {
        dpi
    } else {
        0
    }
}

fn pin_for(window: HWND, target: Target) -> Option<Pin> {
    match target {
        Target::Corner(x, y) => Some(Pin { window, x, y, size: None }),
        Target::Client(x, y, width, height) => {
            let (outer, client) = (window_rect(window)?, client_rect(window)?);
            let (left, top) = (client.0 - outer.0, client.1 - outer.1);
            let (right, bottom) = (outer.2 - client.2, outer.3 - client.3);
            let size = (width.saturating_add(left + right).max(1), height.saturating_add(top + bottom).max(1));
            Some(Pin { window, x: x.saturating_sub(left), y: y.saturating_sub(top), size: Some(size) })
        }
    }
}

fn landed(window: HWND, target: Target) -> bool {
    match target {
        Target::Client(x, y, width, height) => {
            client_rect(window) == Some((x, y, x.saturating_add(width), y.saturating_add(height)))
        }
        Target::Corner(x, y) => window_rect(window).is_some_and(|outer| (outer.0, outer.1) == (x, y)),
    }
}

/// Moves the window to `target` on the main thread. A move onto a monitor of another scale makes
/// the system send a scale change in the middle of it, which tao answers with a size of its own;
/// the pin holds the frame meanwhile, and the client area is measured again afterwards because
/// the borders around it scale too.
fn place_now(window: HWND, target: Target) {
    for _ in 0..3 {
        let Some(pin) = pin_for(window, target) else {
            return;
        };
        let (width, height) = pin.size.unwrap_or((0, 0));
        let keep_size = if pin.size.is_none() { SWP_NOSIZE } else { 0 };
        let previous = PLACING.replace(Some(pin));
        // SAFETY: the shell's own window, on its own thread.
        unsafe {
            SetWindowPos(
                window,
                null_mut(),
                pin.x,
                pin.y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | keep_size,
            )
        };
        PLACING.set(previous);
        if landed(window, target) {
            break;
        }
    }
    if let Some(outer) = window_rect(window) {
        remember(Placed { window, target, outer, dpi: monitor_dpi(window) });
    }
}

/// Puts a window where the shell wants it, from any thread: with `size`, its client area at
/// `position` (virtual-screen pixels) with that size, which is what `set_frame` means even for
/// the popover, whose shadow keeps invisible borders around it; without, its outer frame's
/// top-left corner at `position`, the window keeping its size in points (an ordinary window
/// centred on a screen). The work is queued on the main thread after whatever was asked before.
pub fn place(window: &WebviewWindow, position: tauri::PhysicalPosition<i32>, size: Option<tauri::PhysicalSize<u32>>) {
    let side = |length: u32| i32::try_from(length).unwrap_or(i32::MAX).clamp(1, 1 << 16);
    let target = match size {
        Some(size) => Target::Client(position.x, position.y, side(size.width), side(size.height)),
        None => Target::Corner(position.x, position.y),
    };
    let window = window.clone();
    super::on_main(move || match hwnd(&window) {
        Some(handle) => {
            ensure_subclass(handle, false);
            place_now(handle, target);
        }
        None => {
            let _ = window.set_position(position);
            if let Some(size) = size {
                let _ = window.set_size(size);
            }
        }
    });
}

/// Called once for every panel the shell makes (overlay, toast, popover, pin, `rec-*`), before
/// it is placed: `WS_EX_TOOLWINDOW`, so no panel shows in Alt+Tab (tao's `skip_taskbar` only
/// takes it off the taskbar), kept through tao's later style writes by the subclass, and
/// `DWMWA_TRANSITIONS_FORCEDISABLED`, so a panel shows and hides without DWM's animation.
pub fn prepare_panel(window: &WebviewWindow) {
    let window = window.clone();
    super::on_main(move || {
        let Some(handle) = hwnd(&window) else {
            return;
        };
        ensure_subclass(handle, true);
        // SAFETY: the shell's own window, on its own thread; `disabled` outlives the call that
        // reads it.
        unsafe {
            let style = GetWindowLongW(handle, GWL_EXSTYLE) as u32;
            let wanted = (style | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
            if wanted != style {
                SetWindowLongW(handle, GWL_EXSTYLE, wanted as i32);
                SetWindowPos(
                    handle,
                    null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
            let disabled: BOOL = 1;
            DwmSetWindowAttribute(
                handle,
                DWMWA_TRANSITIONS_FORCEDISABLED as u32,
                (&disabled as *const BOOL).cast::<c_void>(),
                std::mem::size_of::<BOOL>() as u32,
            );
        }
    });
}

/// Called once for an overlay after it got its display's frame. A topmost, undecorated window
/// of exactly the monitor's size already covers the taskbar on Windows, which then counts it as
/// a full-screen window and stays below it.
pub fn cover_display(_window: &WebviewWindow) {}

/// Shows a window without giving it the keyboard focus. On the main thread. The window is shown
/// natively with `SW_SHOWNA`, and tao is then told it is visible (it hides only a window it
/// believes visible), while the subclass keeps tao's own show from activating it; should the
/// window still have taken the foreground, the window that had it gets it back.
pub fn show_inactive(window: &WebviewWindow) {
    let Some(handle) = hwnd(window) else {
        let _ = window.show();
        return;
    };
    // SAFETY: a plain query without arguments.
    let before = unsafe { GetForegroundWindow() };
    ensure_subclass(handle, false);
    // SAFETY: shows the shell's own window, on its own thread, without activating it.
    unsafe { ShowWindow(handle, SW_SHOWNA) };
    let previous = SHOWING.replace(handle);
    // On the main thread tao changes its flags before this returns.
    let _ = window.show();
    SHOWING.set(previous);
    // SAFETY: a plain query without arguments.
    let now = unsafe { GetForegroundWindow() };
    if !before.is_null() && now != before && now == handle {
        // SAFETY: Glimpsfy holds the foreground, so it may hand it on; a stale handle only fails.
        unsafe { SetForegroundWindow(before) };
    }
}

// ------------------------------------------------------------------------------- foreground

/// The window that has the keyboard focus, kept while an overlay or the recorder's target
/// borrows it.
pub struct Foreground(isize);

/// The window that has the keyboard focus now. Callable from any thread.
pub fn foreground() -> Option<Foreground> {
    // SAFETY: a plain query without arguments.
    let window = unsafe { GetForegroundWindow() };
    (!window.is_null()).then_some(Foreground(window as isize))
}

/// Whether `window` belongs to this process.
fn is_own(window: HWND) -> bool {
    if window.is_null() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid place for the answer; a stale handle leaves it 0.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    // SAFETY: a plain query without arguments.
    pid != 0 && pid == unsafe { GetCurrentProcessId() }
}

/// Gives the keyboard focus back to `window`, only while one of Glimpsfy's windows holds it:
/// that is when Windows lets an app hand the foreground on, and it is never taken from an app
/// the user went to meanwhile. A window that closed, hid or was minimised meanwhile is left
/// alone; the system has already picked the next window then.
pub fn restore_foreground(window: Foreground) {
    let target = window.0 as HWND;
    // SAFETY: plain queries on a handle that may be stale, which they report as such.
    let usable = unsafe { IsWindow(target) != 0 && IsWindowVisible(target) != 0 && IsIconic(target) == 0 };
    if !usable {
        return;
    }
    // SAFETY: a plain query without arguments.
    let now = unsafe { GetForegroundWindow() };
    if now == target || !is_own(now) {
        return;
    }
    // SAFETY: Glimpsfy holds the foreground, checked above; a stale handle only fails.
    unsafe { SetForegroundWindow(target) };
}

/// Whether `window` is the foreground window. tao answers `is_focused` with `false` while the
/// page inside has the keyboard (WebView2's own child window holds it), which is nearly always,
/// so the system is asked instead. On the main thread.
pub fn is_foreground(window: &WebviewWindow) -> bool {
    let Some(handle) = hwnd(window) else {
        return false;
    };
    // SAFETY: plain queries.
    let now = unsafe { GetForegroundWindow() };
    !now.is_null() && unsafe { GetAncestor(now, GA_ROOT) } == handle
}

// ------------------------------------------------------------------------ capture exclusion

const EXCLUSION_WORKS: u8 = 1;
const EXCLUSION_REFUSED: u8 = 2;
/// Whether Windows took `WDA_EXCLUDEFROMCAPTURE` for the first window that asked.
static EXCLUSION: AtomicU8 = AtomicU8::new(0);

/// Whether this Windows knows `WDA_EXCLUDEFROMCAPTURE`: 10 version 2004 (build 19041) and later.
/// Earlier builds take the call and act as for `WDA_MONITOR`, painting the window black in every
/// capture, so the call succeeding proves nothing there.
fn exclusion_exists() -> bool {
    static EXISTS: OnceLock<bool> = OnceLock::new();
    *EXISTS.get_or_init(|| windows_version::OsVersion::current().build >= 19041)
}

/// Keeps a window out of every capture and recording (screenshots, Windows.Graphics.Capture,
/// desktop duplication) with `WDA_EXCLUDEFROMCAPTURE`, and says whether Windows took it. Before
/// Windows 10 version 2004 nothing is asked, and the window is one `hide_for_capture` hides.
pub fn exclude_from_capture(window: &WebviewWindow) -> bool {
    let taken = exclusion_exists() && {
        let window = window.clone();
        // The affinity belongs to the window's own thread, which is the main thread.
        super::main_wait(move || {
            let handle = hwnd(&window)?;
            // SAFETY: the handle is this window's, and the call checks it itself.
            Some(unsafe { SetWindowDisplayAffinity(handle, WDA_EXCLUDEFROMCAPTURE) } != 0)
        })
        .flatten()
        .unwrap_or(false)
    };
    let _ = EXCLUSION.compare_exchange(
        0,
        if taken { EXCLUSION_WORKS } else { EXCLUSION_REFUSED },
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
    taken
}

/// The capture backend's id of a window: its `HWND`, as `glimpsfy_capture`'s Windows backend
/// writes a `WindowId`.
pub fn window_id(window: &WebviewWindow) -> Option<WindowId> {
    hwnd(window).map(|handle| WindowId(handle as usize as u32))
}

// ----------------------------------------------------------------------------------- input

fn key_down(key: u16) -> bool {
    // SAFETY: a plain query; the high bit says the key is down now.
    (unsafe { GetAsyncKeyState(i32::from(key)) } as u16) & 0x8000 != 0
}

/// Whether Alt is held now, in any app. Callable from any thread.
pub fn option_key_down() -> bool {
    key_down(VK_MENU)
}

/// Whether the left mouse button is down now, in any app. Callable from any thread.
pub fn left_button_down() -> bool {
    key_down(VK_LBUTTON)
}

/// The process id of the app whose window has the keyboard focus.
pub fn frontmost_pid() -> Option<i32> {
    // SAFETY: a plain query without arguments.
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid place for the answer.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    i32::try_from(pid).ok().filter(|pid| *pid > 0)
}

/// The modifiers held with a key, as the hook saw them.
#[derive(Clone, Copy, Default)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
}

/// What the hooks hand to the take's input thread.
enum Input {
    /// A left or right button went down here, in virtual-screen pixels.
    Click {
        x: i32,
        y: i32,
    },
    Key {
        key: u16,
        down: bool,
        mods: Mods,
    },
}

/// The thread id of the hook thread that watches now.
static HOOKS: Mutex<Option<u32>> = Mutex::new(None);
/// Counts the watches, so a hook thread told to stop before it got going does not start.
static HOOK_GENERATION: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The hook thread's way to the input thread.
    static INPUT: RefCell<Option<Sender<Input>>> = const { RefCell::new(None) };
    /// AltGr is held: Windows sends it as a left Ctrl it makes up (scan code 0x21D) and a right
    /// Alt, and neither then counts as a modifier of its own.
    static ALTGR: Cell<bool> = const { Cell::new(false) };
}

/// Watches clicks and keys in every other app while a take runs. `WH_MOUSE_LL` and
/// `WH_KEYBOARD_LL` run on a thread with a message loop of their own, and they only hand each
/// event to a second thread, since Windows drops a low-level hook that answers slowly. A left or
/// right button going down calls `watch.on_click` with the pointer in global logical points; a
/// key going down (no repeats) calls `watch.on_keys` with its caps in the order Ctrl, Alt,
/// Shift, Win and then the key, named as `macos::recording::key_caps` names them; with
/// `only_shortcuts` a key without Ctrl, Alt or Win is skipped. Clicks on Glimpsfy's own windows
/// and keys typed into them are not reported, as on macOS. Nothing typed is kept or logged.
pub fn watch_input(app: &AppHandle, watch: InputWatch) {
    unwatch_input();
    let InputWatch { clicks, keys, only_shortcuts, on_click, on_keys } = watch;
    if !clicks && !keys {
        return;
    }
    let generation = HOOK_GENERATION.load(Ordering::SeqCst);
    let (input, received) = mpsc::channel();
    let app = app.clone();
    let reader = std::thread::Builder::new()
        .name("glimpsfy-input".into())
        .spawn(move || read_input(&app, received, only_shortcuts, on_click, on_keys));
    if let Err(error) = reader {
        trace::log(format!("[glimpsfy] input watch: {error}"));
        return;
    }
    let hooks = std::thread::Builder::new()
        .name("glimpsfy-input-hooks".into())
        .spawn(move || run_hooks(generation, input, clicks, keys));
    if let Err(error) = hooks {
        trace::log(format!("[glimpsfy] input hooks: {error}"));
    }
}

/// Stops the take's hooks. Their thread removes them and ends, which ends the input thread.
pub fn unwatch_input() {
    let thread = {
        let mut hooks = lock(&HOOKS);
        HOOK_GENERATION.fetch_add(1, Ordering::SeqCst);
        hooks.take()
    };
    if let Some(thread) = thread {
        // SAFETY: the hook thread made its queue before it published its id; a thread that has
        // ended meanwhile makes the call fail.
        unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0) };
    }
}

fn run_hooks(generation: u64, input: Sender<Input>, clicks: bool, keys: bool) {
    let mut message = MSG::default();
    // SAFETY: peeking makes this thread's message queue, so a WM_QUIT posted to it is kept.
    unsafe { PeekMessageW(&mut message, null_mut(), WM_USER, WM_USER, PM_NOREMOVE) };
    // SAFETY: a plain query without arguments.
    let thread = unsafe { GetCurrentThreadId() };
    {
        let mut hooks = lock(&HOOKS);
        if HOOK_GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        *hooks = Some(thread);
    }
    INPUT.with(|slot| *slot.borrow_mut() = Some(input));
    ALTGR.set(false);
    // SAFETY: a null name asks for the module of the running executable, which holds the hook
    // procedures.
    let module = unsafe { GetModuleHandleW(null()) };
    // SAFETY: global low-level hooks with procedures of this module; they are called on this
    // thread while it waits for messages below.
    let mouse =
        if clicks { unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0) } } else { null_mut() };
    // SAFETY: as above.
    let keyboard =
        if keys { unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), module, 0) } } else { null_mut() };
    if (clicks && mouse.is_null()) || (keys && keyboard.is_null()) {
        trace::log("[glimpsfy] input hooks: Windows refused a low-level hook");
    }
    if !mouse.is_null() || !keyboard.is_null() {
        // SAFETY: `message` is a valid place for each message; WM_QUIT from `unwatch_input` ends
        // the loop.
        while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
            // SAFETY: a message this thread's queue just handed out.
            unsafe { DispatchMessageW(&message) };
        }
    }
    for hook in [mouse, keyboard] {
        if !hook.is_null() {
            // SAFETY: a hook this thread installed and nothing removed yet.
            unsafe { UnhookWindowsHookEx(hook) };
        }
    }
    // Dropping the sender ends the input thread.
    INPUT.with(|slot| slot.borrow_mut().take());
    let mut hooks = lock(&HOOKS);
    if *hooks == Some(thread) {
        *hooks = None;
    }
}

fn send_input(event: Input) {
    INPUT.with(|slot| {
        let Ok(slot) = slot.try_borrow() else {
            return;
        };
        if let Some(input) = slot.as_ref() {
            let _ = input.send(event);
        }
    });
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 && matches!(wparam as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN) {
        // SAFETY: for these messages `lparam` points at the event's MSLLHOOKSTRUCT.
        let event = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        send_input(Input::Click { x: event.pt.x, y: event.pt.y });
    }
    // SAFETY: the event goes on unchanged, as a hook must let it.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn key_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for a keyboard event `lparam` points at its KBDLLHOOKSTRUCT.
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if let Ok(key) = u16::try_from(event.vkCode) {
            note_key(key, event.scanCode, down);
        }
    }
    // SAFETY: the event goes on unchanged, as a hook must let it.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn is_modifier(key: u16) -> bool {
    matches!(
        key,
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
            | VK_CAPITAL
            | VK_NUMLOCK
            | VK_SCROLL
    )
}

/// In the keyboard hook. The modifiers are read here, because a hook sees each event after the
/// system took in the ones before it, so a held Ctrl reads as held even when a tool sends the
/// whole shortcut at once.
fn note_key(key: u16, scan: u32, down: bool) {
    if key == VK_LCONTROL && scan & 0x200 != 0 {
        if down {
            ALTGR.set(true);
        }
        return;
    }
    if key == VK_RMENU && !down {
        ALTGR.set(false);
    }
    if is_modifier(key) {
        return;
    }
    let mods = if down { held_modifiers() } else { Mods::default() };
    send_input(Input::Key { key, down, mods });
}

fn held_modifiers() -> Mods {
    // A right Alt let go while another desktop had the keyboard (the sign-in screen, an
    // elevation prompt) sent this hook no release.
    if ALTGR.get() && !key_down(VK_RMENU) {
        ALTGR.set(false);
    }
    let altgr = ALTGR.get();
    Mods {
        ctrl: key_down(VK_RCONTROL) || (!altgr && key_down(VK_LCONTROL)),
        alt: key_down(VK_LMENU) || (!altgr && key_down(VK_RMENU)),
        shift: key_down(VK_LSHIFT) || key_down(VK_RSHIFT),
        win: key_down(VK_LWIN) || key_down(VK_RWIN),
    }
}

fn read_input(
    app: &AppHandle,
    received: Receiver<Input>,
    only_shortcuts: bool,
    on_click: fn(&AppHandle, LogicalPoint),
    on_keys: fn(&AppHandle, Vec<String>),
) {
    // The hook has no repeat flag: a key that is already down repeats.
    let mut held: HashSet<u16> = HashSet::new();
    for event in received {
        match event {
            Input::Click { x, y } => {
                if !own_window_at(x, y) {
                    on_click(app, super::to_logical(f64::from(x), f64::from(y)));
                }
            }
            Input::Key { key, down: false, .. } => {
                held.remove(&key);
            }
            Input::Key { key, down: true, mods } => {
                if !held.insert(key) || own_foreground() {
                    continue;
                }
                if let Some(caps) = key_caps(key, mods, only_shortcuts) {
                    on_keys(app, caps);
                }
            }
        }
    }
}

/// Whether a click at a virtual-screen pixel lands on one of Glimpsfy's windows. One that lets
/// the clicks through (the canvas, the bar over its empty part) does not count, since the click
/// goes to the app below it.
fn own_window_at(x: i32, y: i32) -> bool {
    // SAFETY: plain queries.
    let window = unsafe { WindowFromPoint(POINT { x, y }) };
    if window.is_null() {
        return false;
    }
    // SAFETY: plain queries; WebView2's child windows belong to its own process, so the
    // top-level window is the one asked about.
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    // SAFETY: a plain query; a stale handle reads as 0.
    is_own(root) && (unsafe { GetWindowLongW(root, GWL_EXSTYLE) } as u32) & WS_EX_TRANSPARENT == 0
}

fn own_foreground() -> bool {
    // SAFETY: a plain query without arguments.
    is_own(unsafe { GetForegroundWindow() })
}

/// The caps of a key press: the held modifiers and the key, or `None` for a key without a
/// printable name, or for a plain key when only shortcuts show.
fn key_caps(key: u16, mods: Mods, only_shortcuts: bool) -> Option<Vec<String>> {
    if only_shortcuts && !(mods.ctrl || mods.alt || mods.win) {
        return None;
    }
    let name = key_name(key)?;
    let mut caps = Vec::with_capacity(5);
    for (on, cap) in [(mods.ctrl, "Ctrl"), (mods.alt, "Alt"), (mods.shift, "Shift"), (mods.win, "Win")] {
        if on {
            caps.push(cap.to_string());
        }
    }
    caps.push(name);
    Some(caps)
}

fn key_name(key: u16) -> Option<String> {
    let named = match key {
        VK_RETURN => "↩",
        VK_TAB => "⇥",
        VK_SPACE => "Space",
        VK_BACK => "⌫",
        VK_ESCAPE => "⎋",
        VK_DELETE => "⌦",
        VK_LEFT => "←",
        VK_RIGHT => "→",
        VK_DOWN => "↓",
        VK_UP => "↑",
        VK_HOME => "↖",
        VK_END => "↘",
        VK_PRIOR => "⇞",
        VK_NEXT => "⇟",
        VK_MULTIPLY => "*",
        VK_ADD => "+",
        VK_SUBTRACT => "-",
        VK_DECIMAL => ".",
        VK_DIVIDE => "/",
        VK_F1..=VK_F24 => return Some(format!("F{}", key - VK_F1 + 1)),
        VK_NUMPAD0..=VK_NUMPAD9 => return Some(char::from(b'0' + (key - VK_NUMPAD0) as u8).to_string()),
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    // The key's character without Shift on the layout of the app the user types in, so Shift+4
    // shows 4 and not $.
    // SAFETY: plain queries; no foreground window reads as this thread's layout.
    let layout = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(GetForegroundWindow(), null_mut())) };
    // SAFETY: a plain query. A dead key (a lone accent) has the top bit set.
    let code = unsafe { MapVirtualKeyExW(u32::from(key), MAPVK_VK_TO_CHAR, layout) } & 0x7FFF_FFFF;
    let c = char::from_u32(code).filter(|c| *c != '\0' && !c.is_control())?;
    Some(c.to_uppercase().collect())
}

// ------------------------------------------------------------------- clipboard and dragging

/// A path as Explorer reads it: absolute, without the `\\?\` prefix that only some APIs take.
fn plain_path(path: &Path) -> Vec<u16> {
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
    let (unc, verbatim) = (prefix(r"\\?\UNC\"), prefix(r"\\?\"));
    if units.starts_with(&unc) {
        let mut plain = prefix(r"\\");
        plain.extend_from_slice(&units[unc.len()..]);
        plain
    } else if units.starts_with(&verbatim) {
        units[verbatim.len()..].to_vec()
    } else {
        units
    }
}

/// Global memory holding `bytes`, which the clipboard takes over once `SetClipboardData`
/// succeeds.
fn global_copy(bytes: &[u8]) -> Option<HGLOBAL> {
    // SAFETY: a moveable block of the bytes' length, as the clipboard requires.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
    if memory.is_null() {
        return None;
    }
    // SAFETY: the block just allocated.
    let target = unsafe { GlobalLock(memory) };
    if target.is_null() {
        // SAFETY: the block is still ours.
        unsafe { GlobalFree(memory) };
        return None;
    }
    // SAFETY: the locked block holds `bytes.len()` bytes and overlaps nothing of Rust's.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
        GlobalUnlock(memory);
    }
    Some(memory)
}

/// Opens the clipboard, waiting a little while another app holds it.
fn open_clipboard() -> bool {
    for _ in 0..10 {
        // SAFETY: no owner window; the clipboard is closed again by the caller.
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Puts the file itself on the clipboard: `CF_HDROP` (a `DROPFILES` header with `fWide`, the
/// path as UTF-16, two NULs) together with a "Preferred DropEffect" of `DROPEFFECT_COPY`, so
/// Explorer, mail and chat apps paste a copy and never move the file. arboard's `file_list`
/// writes the drop list without the effect.
pub fn copy_file(path: &Path) -> Result<(), String> {
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    if !absolute.exists() {
        return Err("the file to copy is missing".into());
    }
    let header = std::mem::size_of::<DROPFILES>() as u32;
    let mut files = Vec::new();
    // DROPFILES: where the list starts, the drop point (none), not in a non-client area, wide.
    for field in [header, 0, 0, 0, 1] {
        files.extend_from_slice(&field.to_le_bytes());
    }
    for unit in plain_path(&absolute).into_iter().chain([0, 0]) {
        files.extend_from_slice(&unit.to_le_bytes());
    }
    let effect_name = wide("Preferred DropEffect");
    // SAFETY: a NUL-terminated name; the same name always gives the same format.
    let effect_format = unsafe { RegisterClipboardFormatW(effect_name.as_ptr()) };
    let files = global_copy(&files).ok_or("no memory for the clipboard")?;
    let effect = global_copy(&DROPEFFECT_COPY.to_le_bytes());
    if !open_clipboard() {
        // SAFETY: neither block went to the clipboard.
        unsafe {
            GlobalFree(files);
            if let Some(effect) = effect {
                GlobalFree(effect);
            }
        }
        return Err("another app holds the clipboard".into());
    }
    // SAFETY: the clipboard is open; a block the clipboard takes is the system's from then on, and
    // one it refuses is freed here. The clipboard is closed on every path.
    let placed = unsafe {
        EmptyClipboard();
        let placed = !SetClipboardData(u32::from(CF_HDROP), files).is_null();
        if !placed {
            GlobalFree(files);
        }
        if let Some(effect) = effect {
            if !placed || effect_format == 0 || SetClipboardData(effect_format, effect).is_null() {
                GlobalFree(effect);
            }
        }
        CloseClipboard();
        placed
    };
    if placed {
        Ok(())
    } else {
        Err("the clipboard refused the file".into())
    }
}

/// The largest side of a JPEG put on the clipboard.
const MAX_SIDE: usize = 16_384;

/// A JPEG's pixels as RGBA rows from the top, for the clipboard plugin. The decoder is asked for
/// RGB, which it converts every kind of JPEG to (grey, YCbCr, CMYK and YCCK alike), as the OCR
/// crate does.
pub fn jpeg_rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(|error| error.to_string())?;
    let info = decoder.info().ok_or("the JPEG header could not be read")?;
    let (width, height) = (usize::from(info.width), usize::from(info.height));
    if width == 0 || height == 0 || decoder.output_buffer_size() != Some(width * height * 3) {
        return Err("the JPEG cannot be decoded to RGB".into());
    }
    let mut rgb = vec![0u8; width * height * 3];
    decoder.decode_into(&mut rgb).map_err(|error| error.to_string())?;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok((rgba, u32::from(info.width), u32::from(info.height)))
}

/// The drag image: rows of BGRA pixels from the top, with straight alpha, which is what
/// `IDragSourceHelper::InitializeFromBitmap` takes (it multiplies the alpha in itself).
struct Picture {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

/// A PNG's or a JPEG's pixels as RGBA.
fn decode_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    if bytes.starts_with(b"\x89PNG") {
        let image = tauri::image::Image::from_bytes(bytes).ok()?;
        return Some((image.rgba().to_vec(), image.width(), image.height()));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return jpeg_rgba(bytes).ok();
    }
    None
}

/// `image` fitted into `size` pixels, or `None` when it cannot be read (a recording's video);
/// the drag then shows the shell's own picture of the file.
fn picture_of(image: &Path, size: (f64, f64)) -> Option<Picture> {
    // A saved capture stands in when the entry has no thumbnail; a huge one is not decoded for a
    // drag image.
    if std::fs::metadata(image).ok()?.len() > 64 << 20 {
        return None;
    }
    let (rgba, width, height) = decode_rgba(&std::fs::read(image).ok()?)?;
    let (source_w, source_h) = (f64::from(width), f64::from(height));
    let fit = (size.0 / source_w).min(size.1 / source_h);
    if !fit.is_finite() || fit <= 0.0 {
        return None;
    }
    let target_w = ((source_w * fit).round() as u32).clamp(1, 1024);
    let target_h = ((source_h * fit).round() as u32).clamp(1, 1024);
    let (sw, sh, tw, th) = (width as usize, height as usize, target_w as usize, target_h as usize);
    let mut bgra = Vec::with_capacity(tw * th * 4);
    // Each target pixel is the mean of the source pixels it covers.
    for ty in 0..th {
        let (y0, y1) = (ty * sh / th, ((ty + 1) * sh / th).max(ty * sh / th + 1).min(sh));
        for tx in 0..tw {
            let (x0, x1) = (tx * sw / tw, ((tx + 1) * sw / tw).max(tx * sw / tw + 1).min(sw));
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let at = (y * sw + x) * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += u64::from(rgba[at + channel]);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)).max(1) as u64;
            let [r, g, b, a] = sum.map(|total| (total / count) as u8);
            bgra.extend_from_slice(&[b, g, r, a]);
        }
    }
    Some(Picture { width: target_w, height: target_h, bgra })
}

/// A 32-bit top-down DIB section holding the picture.
fn bitmap_of(picture: &Picture) -> Option<HBITMAP> {
    if picture.bgra.len() != picture.width as usize * picture.height as usize * 4 {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: picture.width as i32,
            biHeight: -(picture.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
    };
    let mut bits: *mut c_void = null_mut();
    // SAFETY: a DIB of the picture's size; with DIB_RGB_COLORS no device context is read, and
    // `bits` receives the section's pixels.
    let bitmap = unsafe { CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
    if bitmap.is_null() || bits.is_null() {
        if !bitmap.is_null() {
            // SAFETY: the bitmap just made, which nothing else holds.
            unsafe { DeleteObject(bitmap) };
        }
        return None;
    }
    // SAFETY: the section holds width × height × 4 bytes, the picture's length checked above.
    unsafe { std::ptr::copy_nonoverlapping(picture.bgra.as_ptr(), bits.cast::<u8>(), picture.bgra.len()) };
    Some(bitmap)
}

/// Sets the drag image of the shell's data object; without it the shell draws its own.
fn set_drag_image(data: &windows::Win32::System::Com::IDataObject, picture: &Picture) {
    use windows::Win32::Foundation::{COLORREF, POINT as ShellPoint, SIZE};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDragSourceHelper, SHDRAGIMAGE};

    let Some(bitmap) = bitmap_of(picture) else {
        return;
    };
    let image = SHDRAGIMAGE {
        sizeDragImage: SIZE { cx: picture.width as i32, cy: picture.height as i32 },
        ptOffset: ShellPoint { x: (picture.width / 2) as i32, y: (picture.height / 2) as i32 },
        hbmpDragImage: windows::Win32::Graphics::Gdi::HBITMAP(bitmap),
        // CLR_NONE: the picture's alpha, not a colour key, says what is transparent.
        crColorKey: COLORREF(0xFFFF_FFFF),
    };
    // SAFETY: COM is up on this thread (`drag_now` initialised OLE); the helper is in-process.
    let helper: windows::core::Result<IDragSourceHelper> =
        unsafe { CoCreateInstance(&CLSID_DragDropHelper, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER) };
    // SAFETY: `image` and `data` outlive the call. The helper owns the bitmap once the call
    // succeeds, so it is deleted here only when it did not.
    let taken = helper.is_ok_and(|helper| unsafe { helper.InitializeFromBitmap(&image, data) }.is_ok());
    if !taken {
        // SAFETY: the bitmap stayed ours.
        unsafe { DeleteObject(bitmap) };
    }
}

/// The drag on the main thread: OLE for the thread, then [`drag_with_ole`], then OLE released.
fn drag_now(
    window: HWND,
    path: &[u16],
    picture: Option<&Picture>,
    started: &Sender<Result<(), String>>,
) -> Result<(), String> {
    use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};

    // SAFETY: the main thread's apartment is single-threaded (wry made it so), which OLE needs;
    // a successful call is paired with `OleUninitialize` below.
    let ole = unsafe { OleInitialize(None) }.is_ok();
    let result = drag_with_ole(window, path, picture, started);
    if ole {
        // SAFETY: pairs the successful `OleInitialize` above.
        unsafe { OleUninitialize() };
    }
    result
}

/// The shell's own data object for the file (it offers `CF_HDROP` and everything Explorer, mail
/// and chat apps read), the drag image, then the system's modal drag loop with its default drop
/// source, copy only. `started` hears just before the loop begins, which holds the main thread
/// until the drop.
fn drag_with_ole(
    window: HWND,
    path: &[u16],
    picture: Option<&Picture>,
    started: &Sender<Result<(), String>>,
) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{IBindCtx, IDataObject};
    use windows::Win32::System::Ole::{IDropSource, DROPEFFECT_COPY as COPY};
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop};

    // SAFETY: `path` is NUL-terminated and outlives the call.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None::<&IBindCtx>) }
        .map_err(|error| error.to_string())?;
    // SAFETY: asks the item for its data object; the GUID outlives the call.
    let data: IDataObject =
        unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) }.map_err(|error| error.to_string())?;
    if let Some(picture) = picture {
        set_drag_image(&data, picture);
    }
    let _ = started.send(Ok(()));
    // SAFETY: on the window's own thread, with OLE up; `data` lives until the loop returns.
    let _ = unsafe { SHDoDragDrop(Some(windows::Win32::Foundation::HWND(window)), &data, None::<&IDropSource>, COPY) };
    Ok(())
}

/// Starts dragging `file` out of `window` as a file Explorer copies, with `image` (fitted into
/// `size` points at the pointer's screen's scale) under the pointer. The page calls this while
/// the button is down; it returns once the drag has begun, and the drag itself holds the main
/// thread until the drop, as every Windows drag does.
pub fn drag_out(window: &WebviewWindow, file: &Path, image: &Path, size: (f64, f64)) -> Result<(), String> {
    let file = std::path::absolute(file).map_err(|error| error.to_string())?;
    if !file.is_file() {
        return Err("the file to drag is missing".into());
    }
    let mut path = plain_path(&file);
    path.push(0);
    let scale = super::screen_area(None).map_or(1.0, |(_, scale)| scale);
    let picture = picture_of(image, (size.0 * scale, size.1 * scale));
    let (started, start) = mpsc::channel();
    let window = window.clone();
    super::on_main(move || {
        let Some(handle) = hwnd(&window) else {
            let _ = started.send(Err("the window has no handle".to_string()));
            return;
        };
        if let Err(error) = drag_now(handle, &path, picture.as_ref(), &started) {
            let _ = started.send(Err(error));
        }
    });
    start.recv_timeout(Duration::from_secs(2)).map_err(|_| "the drag did not start in time".to_string())?
}

/// Plays the capture sound. Windows has no screenshot sound of its own, so nothing plays and
/// `shutter_sound` is `false`.
///
/// Native: ship a short sound and play it with `PlaySoundW(…, SND_ASYNC | SND_NODEFAULT)`, or
/// leave it silent.
pub fn shutter() {}

// ------------------------------------------------------------------------------------ system

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn registry_dword(key: &str, value: &str) -> Option<u32> {
    let (key, value) = (wide(key), wide(value));
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: both names are NUL-terminated, and `data` and `size` describe four writable bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut data as *mut u32).cast::<c_void>(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

/// A string value of any length.
fn registry_text(root: HKEY, key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut size = 0u32;
    // SAFETY: both names are NUL-terminated; a null buffer asks for the size the value needs.
    let status =
        unsafe { RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), &mut size) };
    if status != 0 || size == 0 {
        return None;
    }
    let mut data = vec![0u16; (size as usize).div_ceil(2)];
    let mut size = (data.len() * 2) as u32;
    // SAFETY: `data` holds `size` writable bytes, the size just asked for.
    let status = unsafe {
        RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, null_mut(), data.as_mut_ptr().cast(), &mut size)
    };
    if status != 0 {
        return None;
    }
    let end = data.iter().position(|unit| *unit == 0).unwrap_or(data.len());
    Some(String::from_utf16_lossy(&data[..end]))
}

fn registry_string(key: &str, value: &str) -> Option<String> {
    registry_text(HKEY_CURRENT_USER, key, value)
}

/// Removes a value; one that is not there counts as removed.
fn registry_delete(root: HKEY, key: &str, value: &str) -> bool {
    let (key, value) = (wide(key), wide(value));
    // SAFETY: both names are NUL-terminated.
    let status = unsafe { RegDeleteKeyValueW(root, key.as_ptr(), value.as_ptr()) };
    status == 0 || status == ERROR_FILE_NOT_FOUND
}

/// The user's preferred display languages, most wanted first (`tr-TR`, `en-US`).
pub fn preferred_languages() -> Vec<String> {
    let (mut count, mut length) = (0u32, 0u32);
    // SAFETY: a null buffer asks for the length the list needs.
    let sized = unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, null_mut(), &mut length) };
    if sized == 0 || length == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: `buffer` holds `length` units, the size just asked for.
    let read = unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buffer.as_mut_ptr(), &mut length) };
    if read == 0 {
        return Vec::new();
    }
    buffer.split(|unit| *unit == 0).filter(|tag| !tag.is_empty()).map(String::from_utf16_lossy).collect()
}

/// Whether an app is registered for the scheme of `url` (`http`, `mailto`, `tel`), as the shell
/// would pick it for a click. Without a handler the call answers `ERROR_NO_ASSOCIATION`.
pub fn can_open_url(url: &str) -> bool {
    use windows_sys::Win32::UI::Shell::{AssocQueryStringW, ASSOCF_IS_PROTOCOL, ASSOCSTR_EXECUTABLE};
    let Some((scheme, _)) = url.split_once(':') else { return false };
    let scheme = wide(scheme);
    let mut length = 0u32;
    // SAFETY: the scheme is NUL-terminated, and a null buffer asks only for the length.
    let status = unsafe {
        AssocQueryStringW(ASSOCF_IS_PROTOCOL, ASSOCSTR_EXECUTABLE, scheme.as_ptr(), null(), null_mut(), &mut length)
    };
    // S_OK, or S_FALSE with the length; any failure HRESULT is negative.
    status >= 0 && length > 1
}

/// The Settings page that stands for a System Settings pane of macOS.
pub fn system_settings_url(pane: SystemPane) -> Option<&'static str> {
    Some(match pane {
        // Accessibility › Keyboard holds "Use the Print screen key to open screen capture".
        SystemPane::KeyboardShortcuts => "ms-settings:easeofaccess-keyboard",
        SystemPane::LoginItems => "ms-settings:startupapps",
    })
}

/// The Settings page where the user lets desktop apps use the microphone or the camera. Screen
/// capture and input monitoring need no permission on Windows.
pub fn permission_settings_url(permission: Permission) -> Option<&'static str> {
    match permission {
        Permission::Microphone => Some("ms-settings:privacy-microphone"),
        Permission::Camera => Some("ms-settings:privacy-webcam"),
        Permission::ScreenRecording | Permission::Accessibility => None,
    }
}

/// Whether Settings › Privacy lets desktop apps use the microphone or the camera: the device's
/// own switch and the one for desktop (non-packaged) apps, as the consent store keeps them.
/// Without either entry (older builds) nothing stands in the way.
pub fn media_permission(media: Media) -> PermissionState {
    let device = match media {
        Media::Audio => "microphone",
        Media::Video => "webcam",
    };
    let store = format!(r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{device}");
    let denied = |key: &str| registry_string(key, "Value").is_some_and(|value| value.eq_ignore_ascii_case("Deny"));
    if denied(&store) || denied(&format!(r"{store}\NonPackaged")) {
        PermissionState::Denied
    } else {
        PermissionState::Granted
    }
}

/// Whether this copy can install an update itself. The per-user NSIS install lives in the user's
/// own folder and replaces itself without a prompt. A per-machine MSI install sits where only an
/// administrator writes: an administrator gets Windows' elevation prompt, while a standard user
/// could not answer it and the updater (which quits before the installer runs) would leave
/// nothing installed, so there the pages hear `available` and link the release instead.
pub fn can_self_update() -> bool {
    static ANSWER: OnceLock<bool> = OnceLock::new();
    *ANSWER.get_or_init(|| install_folder_writable() || can_elevate())
}

fn install_folder_writable() -> bool {
    let Some(folder) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) else {
        return false;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let probe = folder.join(format!(".glimpsfy-update-probe-{}-{nanos}", std::process::id()));
    // A new file of its own, so nothing that is there is replaced.
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Whether the user is an administrator: elevated now, or one whose rights User Account Control
/// holds back until its prompt.
fn can_elevate() -> bool {
    let mut token: HANDLE = null_mut();
    // SAFETY: the process's own pseudo-handle; `token` is a valid place for the answer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut kind: TOKEN_ELEVATION_TYPE = 0;
    let mut length = 0u32;
    // SAFETY: `kind` holds the TOKEN_ELEVATION_TYPE this class answers with.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevationType,
            (&mut kind as *mut TOKEN_ELEVATION_TYPE).cast::<c_void>(),
            std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut length,
        )
    };
    // SAFETY: the token opened above.
    unsafe { CloseHandle(token) };
    read != 0 && (kind == TokenElevationTypeLimited || kind == TokenElevationTypeFull)
}

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Start at login: the current user's Run value, named as the NSIS uninstaller removes it (the
/// product name), with the executable's path in quotes. The autostart plugin writes the path
/// without quotes and with a trailing space, and a copy started as administrator would write it
/// for every user.
pub fn set_login_item(app: &AppHandle, on: bool) -> Result<(), String> {
    let name = app.package_info().name.clone();
    if !on {
        let removed = registry_delete(HKEY_CURRENT_USER, RUN, &name);
        registry_delete(HKEY_CURRENT_USER, STARTUP_APPROVED, &name);
        // An entry for every user, from a copy that once ran as administrator; removing it takes
        // rights this process may lack, so a failure is not reported.
        registry_delete(HKEY_LOCAL_MACHINE, RUN, &name);
        return if removed { Ok(()) } else { Err("the login item could not be removed".into()) };
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command: Vec<u16> = vec![u16::from(b'"')];
    command.extend(plain_path(&exe));
    command.extend([u16::from(b'"'), 0]);
    let existed = registry_text(HKEY_CURRENT_USER, RUN, &name).is_some();
    let (key, value) = (wide(RUN), wide(&name));
    // SAFETY: the names are NUL-terminated, and `command` is a NUL-terminated string of the
    // length given in bytes.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            REG_SZ,
            command.as_ptr().cast::<c_void>(),
            (command.len() * 2) as u32,
        )
    };
    if status != 0 {
        return Err(format!("the login item could not be written (error {status})"));
    }
    // Task Manager's switch for an earlier entry of that name would keep a new one off; an entry
    // that was there keeps the switch the user set.
    if !existed {
        registry_delete(HKEY_CURRENT_USER, STARTUP_APPROVED, &name);
    }
    Ok(())
}

// --------------------------------------------------------------------------------- tray icon

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// Whether the taskbar is light: under a contrast theme the theme's window colour says, and
/// otherwise "Choose your default Windows mode" in Settings › Personalisation › Colours. Builds
/// without the value have a dark taskbar.
fn taskbar_is_light() -> bool {
    if let Some(light) = contrast_theme_is_light() {
        return light;
    }
    registry_dword(PERSONALIZE, "SystemUsesLightTheme") == Some(1)
}

/// Whether a contrast theme is on and light, or `None` without one.
fn contrast_theme_is_light() -> Option<bool> {
    let mut contrast = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        dwFlags: 0,
        lpszDefaultScheme: null_mut(),
    };
    // SAFETY: a HIGHCONTRASTW with its size set; the call fills in the flags.
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            (&mut contrast as *mut HIGHCONTRASTW).cast::<c_void>(),
            0,
        )
    };
    if read == 0 || contrast.dwFlags & HCF_HIGHCONTRASTON == 0 {
        return None;
    }
    // SAFETY: a plain query; the colour is 0x00BBGGRR.
    let colour = unsafe { GetSysColor(COLOR_WINDOW) };
    let channel = |shift: u32| f64::from((colour >> shift) & 0xFF);
    Some(0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(16) > 127.5)
}

/// The tray glyph that shows on the taskbar as it is now: black on a light one, white on a dark
/// one.
pub fn tray_glyph() -> &'static [u8] {
    if taskbar_is_light() {
        include_bytes!("../../icons/tray-on-light.png")
    } else {
        include_bytes!("../../icons/tray-on-dark.png")
    }
}

fn theme_check() {
    let light = if taskbar_is_light() { 2 } else { 1 };
    if TASKBAR_LIGHT.swap(light, Ordering::SeqCst) != light {
        if let Some(changed) = lock(&THEME_CHANGED).as_ref() {
            changed();
        }
    }
}

/// Calls `changed` whenever the taskbar turns light or dark, or a contrast theme comes or goes:
/// the watch window hears every setting and colour change.
pub fn watch_panel_theme(changed: Box<dyn Fn() + Send>) {
    TASKBAR_LIGHT.store(if taskbar_is_light() { 2 } else { 1 }, Ordering::SeqCst);
    *lock(&THEME_CHANGED) = Some(changed);
    if !system_watch() {
        trace::log("[glimpsfy] system watch: the thread did not start, so the tray glyph keeps its theme");
    }
}

// -------------------------------------------------------------------------------------- glass

/// Puts a native blur under each toast. Not written (`native_glass` is `false`), so the toast
/// page draws its own background: DWM's backdrops (`DWMWA_SYSTEMBACKDROP_TYPE`, Windows 11 22H2
/// and later) blur a whole top-level window and cannot follow toasts the page moves frame by
/// frame inside one transparent window, and the acrylic of `SetWindowCompositionAttribute` is
/// undocumented and lags behind moves on Windows 10.
///
/// Native: a blurred backdrop per toast (`glass` gives each toast's place from the window's
/// bottom-right corner, its corner radius and its opacity; a toast that is no longer listed
/// loses its blur), for example child windows with `DWMWA_SYSTEMBACKDROP_TYPE` on Windows 11 or
/// `SetWindowCompositionAttribute` acrylic on Windows 10. The page moves the toasts frame by
/// frame, so nothing here animates on its own.
pub fn toast_glass(_window: &WebviewWindow, _glass: &[ToastGlass]) {}

/// Puts a native blur and the kit's shadow under the recorder's bar. Not written, so the bar's
/// page draws its own background, for the reasons [`toast_glass`] gives.
///
/// Native: a click-through window under `bar` at `frame` grown by the shadow's room, with a
/// blurred piece of glass for each of `pieces` (points from the bar window's top-left corner,
/// its corner radius, a small or a floating shadow), kept out of captures.
pub fn backdrop_show(_bar: &WebviewWindow, _frame: LogicalRect, _pieces: &[GlassPiece]) {}

pub fn backdrop_hide() {}

// --------------------------------------------------------------------------------- the bubble

/// Shows the webcam bubble. Not written (`camera_bubble` is `false`): it needs a Media
/// Foundation camera pipeline drawn into a window of its own, which belongs with the recorder's
/// camera work rather than the app shell.
///
/// Native: a topmost, borderless window that is *not* excluded from captures (a recording shows
/// it), at `frame` in global logical points (place it with `super::to_physical`), clipped to a
/// circle when `round` and to corners of 22 % of its shorter side otherwise, showing the mirrored
/// preview of the camera `device` (a Media Foundation symbolic link, or the default camera), the
/// camera opened the first time and when the device changes. It never takes the focus.
pub fn bubble_show(_device: Option<&str>, _frame: LogicalRect, _round: bool) -> Result<(), String> {
    Err("the webcam bubble is not implemented on Windows yet".into())
}

/// Moves the bubble to `frame`, sliding when `animate`.
pub fn bubble_move(_frame: LogicalRect, _round: bool, _animate: bool) {}

/// Hides the bubble and turns the camera off.
pub fn bubble_hide() {}

/// The capture backend's id of the bubble's window, which a window recording must show.
pub fn bubble_window_id() -> Option<WindowId> {
    None
}
