//! The windows on the desktop, front to back, as `WindowInfo`s: which ones show, their visible
//! frames, their titles and the apps they belong to.
//!
//! Every function here expects the calling thread to be per-monitor DPI aware (`DpiScope`).

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Mutex;

use windows::core::{w, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWMWCP_ROUNDSMALL, DWMWINDOWATTRIBUTE,
};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetClassNameW, GetLayeredWindowAttributes, GetWindowDisplayAffinity, GetWindowLongW,
    GetWindowRect, GetWindowThreadProcessId, InternalGetWindowText, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
    GWL_EXSTYLE, GWL_STYLE, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, WDA_NONE, WS_CAPTION, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_THICKFRAME,
};

use super::monitors;
use super::sys::{self, OwnedHandle};
use crate::desktop;
use crate::display::{Display, WindowInfo, WindowListOptions};
use crate::error::Result;

/// The layer of the desktop's own windows, below every app's.
const DESKTOP_LAYER: i32 = -1;
/// The layer of always-on-top tool windows (palettes, picture in picture), which float above
/// ordinary windows the way panels do on macOS; a capture can still pick them.
const FLOATING_LAYER: i32 = 3;
/// The layer of the taskbar, the Start menu and the shell's other surfaces, above what a capture
/// picks.
const SHELL_LAYER: i32 = 20;
/// The layer of click-through overlays (a graphics driver's overlay, a screen ruler): they cover
/// the screen without being a window anyone can reach.
const OVERLAY_LAYER: i32 = 25;

/// The radius Windows 11 rounds a window's corners with, in points, and the small one of menus.
const CORNER_RADIUS: f64 = 8.0;
const SMALL_CORNER_RADIUS: f64 = 4.0;

/// The window classes of the desktop itself: the wallpaper and the icons.
const DESKTOP_CLASSES: [&str; 2] = ["Progman", "WorkerW"];
/// The window classes of the shell's surfaces, of menus and of tooltips.
const SHELL_CLASSES: [&str; 12] = [
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "XamlExplorerHostIslandWindow",
    "Windows.UI.Core.CoreWindow",
    "MultitaskingViewFrame",
    "TaskListThumbnailWnd",
    "Shell_InputSwitchTopLevelWindow",
    "ForegroundStaging",
    "#32768",
    "tooltips_class32",
];
/// The class of the frame Windows puts around a Store app, whose process is not the app's.
const APP_FRAME_CLASS: &str = "ApplicationFrameWindow";

/// The names of apps by the path of their executable, kept by the backend: reading a version
/// resource opens the file, and the same few apps own the windows of every listing.
pub(super) type NameCache = Mutex<HashMap<String, String>>;

/// The windows `options` asks for, frontmost first (`EnumWindows` lists top-level windows in
/// their stacking order), with their bounds in the global logical space of `displays`.
pub(super) fn list(options: WindowListOptions, displays: &[Display], names: &NameCache) -> Result<Vec<WindowInfo>> {
    let mut handles: Vec<isize> = Vec::new();
    // SAFETY: the callback runs on this thread before the call returns, and the pointer in
    // `LPARAM` is to `handles`, which outlives the call.
    unsafe { EnumWindows(Some(collect), LPARAM(&mut handles as *mut Vec<isize> as isize)) }
        .map_err(|error| sys::api_error("EnumWindows", &error))?;

    // SAFETY: the call has no preconditions.
    let own = unsafe { GetCurrentProcessId() };
    let mut apps: HashMap<u32, String> = HashMap::new();
    let mut windows = Vec::new();
    for handle in handles {
        let window = HWND(handle as *mut c_void);
        // SAFETY: these calls read the state of a window and check the handle themselves; a
        // window that closed since the listing reads as not visible.
        let (visible, minimised) = unsafe { (IsWindowVisible(window).as_bool(), IsIconic(window).as_bool()) };
        if !visible {
            continue;
        }
        // A cloaked window is drawn nowhere: it sits on another virtual desktop, or it is a
        // Store app that is suspended.
        let is_on_screen = !minimised && dwm_value(window, DWMWA_CLOAKED).unwrap_or(0) == 0;
        if options.on_screen_only && !is_on_screen {
            continue;
        }
        let mut pid = 0u32;
        // SAFETY: the call writes one u32.
        unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        if options.exclude_own && pid == own {
            continue;
        }
        let class = class_name(window);
        let is_desktop = DESKTOP_CLASSES.contains(&class.as_str());
        if is_desktop && options.exclude_desktop {
            continue;
        }
        let Some(frame) = visible_frame(window) else { continue };
        let Some(bounds) = sys::physical(frame).and_then(|rect| desktop::physical_to_logical_rect(displays, rect))
        else {
            continue;
        };
        let title = window_title(window);
        // SAFETY: reading a window's style bits; a closed window reads as 0.
        let extended = unsafe { GetWindowLongW(window, GWL_EXSTYLE) } as u32;
        let has = |style: u32| extended & style != 0;

        let layer = if is_desktop {
            DESKTOP_LAYER
        } else if SHELL_CLASSES.contains(&class.as_str()) {
            SHELL_LAYER
        } else if has(WS_EX_LAYERED.0) && has(WS_EX_TRANSPARENT.0) {
            OVERLAY_LAYER
        } else if has(WS_EX_TOPMOST.0) && (has(WS_EX_TOOLWINDOW.0) || has(WS_EX_NOACTIVATE.0)) {
            FLOATING_LAYER
        } else {
            0
        };

        let (app_pid, app_name) = app_of(window, pid, &class, title.as_deref(), names, &mut apps);
        windows.push(WindowInfo {
            id: sys::window_id(window),
            title,
            app_name,
            bundle_id: None,
            pid: app_pid as i32,
            bounds,
            layer,
            z_index: windows.len() as u32,
            alpha: if has(WS_EX_LAYERED.0) { layered_alpha(window) } else { 1.0 },
            is_on_screen,
            corner_radius: Some(corner_radius(window)),
            capture_blocked: capture_blocked(window),
        });
    }
    Ok(windows)
}

/// Whether the window keeps itself out of captures with `SetWindowDisplayAffinity`: as a black
/// box (`WDA_MONITOR`) or not at all (`WDA_EXCLUDEFROMCAPTURE`).
fn capture_blocked(window: HWND) -> bool {
    let mut affinity = WDA_NONE.0;
    // SAFETY: the call writes one u32 and checks the handle itself; a closed window fails and
    // reads as not blocked.
    unsafe { GetWindowDisplayAffinity(window, &mut affinity) }.is_ok() && affinity != WDA_NONE.0
}

unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the pointer to the `Vec<isize>` that `list` passed to `EnumWindows`,
    // which calls this function only while `list` waits for it.
    let handles = unsafe { &mut *(data.0 as *mut Vec<isize>) };
    handles.push(window.0 as isize);
    true.into()
}

/// Whether the window still exists.
pub(super) fn exists(window: HWND) -> bool {
    // SAFETY: the call checks the handle itself.
    unsafe { IsWindow(Some(window)) }.as_bool()
}

pub(super) fn is_minimised(window: HWND) -> bool {
    // SAFETY: the call checks the handle itself.
    unsafe { IsIconic(window) }.as_bool()
}

/// One 32-bit attribute of a window from the compositor.
fn dwm_value(window: HWND, attribute: DWMWINDOWATTRIBUTE) -> Option<u32> {
    let mut value = 0u32;
    // SAFETY: the attributes read through here are 32 bits wide, and the call is told so.
    unsafe { DwmGetWindowAttribute(window, attribute, &mut value as *mut u32 as *mut c_void, 4) }.ok().map(|()| value)
}

/// The window's frame as the compositor draws it, in virtual-screen pixels: without the
/// invisible borders a window is resized by, which `GetWindowRect` includes. `None` when the
/// window is gone or has no area.
pub(super) fn frame_bounds(window: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    // SAFETY: the attribute is a `RECT`, and the call is told its size.
    let read = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if read.is_err() {
        // SAFETY: the call writes one `RECT` and checks the window handle itself.
        unsafe { GetWindowRect(window, &mut rect) }.ok()?;
    }
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

/// The window's whole rectangle in virtual-screen pixels, invisible borders included.
pub(super) fn outer_rect(window: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    // SAFETY: the call writes one `RECT` and checks the window handle itself.
    unsafe { GetWindowRect(window, &mut rect) }.ok().map(|()| rect)
}

/// The part of the window's frame a user sees: a maximised window's frame reaches a few pixels
/// past its monitor's edges, and that part is cut off.
pub(super) fn visible_frame(window: HWND) -> Option<RECT> {
    let frame = frame_bounds(window)?;
    // SAFETY: the calls check the handle themselves; the monitor handle is only read.
    let zoomed = unsafe { IsZoomed(window) }.as_bool();
    if zoomed {
        // SAFETY: as above.
        let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
        if let Some(on_monitor) = monitors::rect_of(monitor).and_then(|rect| sys::intersect(frame, rect)) {
            return Some(on_monitor);
        }
    }
    Some(frame)
}

/// The radius of the window's corners in points: 8 for a framed window on Windows 11 that is
/// neither maximised nor full screen, the window's own choice when it made one, 0 otherwise and
/// on Windows 10. It is a number and never `None`, so the overlay draws square corners where the
/// window has them instead of its default radius.
pub(super) fn corner_radius(window: HWND) -> f64 {
    if !sys::is_windows_11() {
        return 0.0;
    }
    // SAFETY: the calls read a window's state and check the handle themselves.
    let (zoomed, style) = unsafe { (IsZoomed(window).as_bool(), GetWindowLongW(window, GWL_STYLE) as u32) };
    if zoomed {
        return 0.0;
    }
    // The attribute is documented for setting only. Where the compositor answers a read, the
    // window's own choice is honoured; where it refuses, the system's rule below decides.
    match dwm_value(window, DWMWA_WINDOW_CORNER_PREFERENCE).map(|value| value as i32) {
        Some(value) if value == DWMWCP_DONOTROUND.0 => return 0.0,
        Some(value) if value == DWMWCP_ROUNDSMALL.0 => return SMALL_CORNER_RADIUS,
        _ => {}
    }
    // The system rounds windows that have a frame of its own; a bare popup stays square.
    let framed = style & WS_CAPTION.0 == WS_CAPTION.0 || style & WS_THICKFRAME.0 != 0;
    if !framed {
        return 0.0;
    }
    // A window that covers its whole monitor (a game, a video) is drawn square.
    // SAFETY: the monitor handle is only read.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let covers = frame_bounds(window).zip(monitors::rect_of(monitor)).is_some_and(|(frame, screen)| {
        frame.left <= screen.left
            && frame.top <= screen.top
            && frame.right >= screen.right
            && frame.bottom >= screen.bottom
    });
    if covers {
        0.0
    } else {
        CORNER_RADIUS
    }
}

fn class_name(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: the call writes at most `buffer.len()` UTF-16 units and returns how many.
    let length = unsafe { GetClassNameW(window, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..length.min(buffer.len())])
}

/// The window's title, read from the system's own copy. `GetWindowText` would send a message to
/// a window of this process and wait for its thread, which may be the one waiting for this
/// listing.
fn window_title(window: HWND) -> Option<String> {
    let mut buffer = [0u16; 512];
    // SAFETY: the call writes at most `buffer.len()` UTF-16 units and returns how many.
    let length = unsafe { InternalGetWindowText(window, &mut buffer) }.max(0) as usize;
    let title = String::from_utf16_lossy(&buffer[..length.min(buffer.len())]);
    (!title.trim().is_empty()).then_some(title)
}

/// The opacity of a layered window, when it has one for the whole window.
fn layered_alpha(window: HWND) -> f64 {
    let mut alpha = 255u8;
    let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
    // SAFETY: the call writes one byte and one flags value. It fails for windows that draw
    // themselves with per-pixel alpha, which then count as opaque.
    let read = unsafe { GetLayeredWindowAttributes(window, None, Some(&mut alpha), Some(&mut flags)) };
    if read.is_ok() && flags.0 & LWA_ALPHA.0 != 0 {
        alpha as f64 / 255.0
    } else {
        1.0
    }
}

/// The process and the name of the app a window belongs to. A Store app's window belongs to
/// `ApplicationFrameHost`; the app itself owns the core window inside it.
fn app_of(
    window: HWND,
    pid: u32,
    class: &str,
    title: Option<&str>,
    names: &NameCache,
    apps: &mut HashMap<u32, String>,
) -> (u32, String) {
    let mut app_pid = pid;
    if class == APP_FRAME_CLASS {
        // SAFETY: the class name is a NUL-terminated constant, and the call checks the handle.
        let core = unsafe { FindWindowExW(Some(window), None, w!("Windows.UI.Core.CoreWindow"), PCWSTR::null()) };
        if let Ok(core) = core {
            let mut core_pid = 0u32;
            // SAFETY: the call writes one u32.
            unsafe { GetWindowThreadProcessId(core, Some(&mut core_pid)) };
            if core_pid != 0 {
                app_pid = core_pid;
            }
        }
    }
    let name = apps.entry(app_pid).or_insert_with(|| process_name(app_pid, names)).clone();
    // A frame whose app could not be found (it is suspended) is named by its title, which is
    // the app's name, rather than "Application Frame Host".
    if class == APP_FRAME_CLASS && app_pid == pid {
        if let Some(title) = title {
            return (app_pid, title.to_string());
        }
    }
    (app_pid, name)
}

/// The name of the app that runs as `pid`: the description in its executable's version
/// resource, or the executable's name without its extension. Empty when the process may not be
/// read (a system process).
fn process_name(pid: u32, names: &NameCache) -> String {
    let Some(path) = process_path(pid) else { return String::new() };
    let cached = names.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&path).cloned();
    if let Some(name) = cached {
        return name;
    }
    let name = file_description(&path).unwrap_or_else(|| {
        Path::new(&path).file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default()
    });
    names.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(path, name.clone());
    name
}

fn process_path(pid: u32) -> Option<String> {
    // SAFETY: the call opens a handle with the least access that names a process; `OwnedHandle`
    // closes it.
    let process = OwnedHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: `length` tells the call how many UTF-16 units `buffer` holds, and the call sets it
    // to how many it wrote.
    unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length) }
        .ok()?;
    Some(String::from_utf16_lossy(&buffer[..(length as usize).min(buffer.len())]))
}

/// The `FileDescription` of an executable's version resource ("Google Chrome" for chrome.exe).
fn file_description(path: &str) -> Option<String> {
    let path = sys::wide(path);
    // SAFETY: `path` is NUL-terminated.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
    if size == 0 {
        return None;
    }
    // u16 storage keeps the block aligned for the UTF-16 strings and the code pairs read from it.
    let mut block = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: the block holds at least `size` bytes, which is all the call writes.
    unsafe { GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, block.as_mut_ptr().cast()) }.ok()?;

    let mut pointer: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    // The resource lists the languages it holds as pairs of a language and a code page.
    let mut languages: Vec<(u16, u16)> = Vec::new();
    // SAFETY: the block was filled by `GetFileVersionInfoW`; the call sets `pointer` to data
    // inside the block and `length` to its size in bytes.
    let listed =
        unsafe { VerQueryValueW(block.as_ptr().cast(), w!("\\VarFileInfo\\Translation"), &mut pointer, &mut length) };
    if listed.as_bool() && !pointer.is_null() {
        // SAFETY: `pointer` addresses `length` bytes inside `block`, which is u16-aligned and
        // alive; the value is an array of u16 pairs.
        let codes = unsafe { std::slice::from_raw_parts(pointer as *const u16, length as usize / 2) };
        languages.extend(codes.chunks_exact(2).map(|pair| (pair[0], pair[1])));
    }
    // US English in Unicode and in the Western code page, for resources without the list.
    languages.extend([(0x0409, 0x04B0), (0x0409, 0x04E4)]);
    for (language, code_page) in languages {
        let key = sys::wide(&format!("\\StringFileInfo\\{language:04x}{code_page:04x}\\FileDescription"));
        // SAFETY: `key` is NUL-terminated; the call sets `pointer` to a string inside the block
        // and `length` to its length in UTF-16 units.
        let found = unsafe { VerQueryValueW(block.as_ptr().cast(), PCWSTR(key.as_ptr()), &mut pointer, &mut length) };
        if !found.as_bool() || pointer.is_null() || length == 0 {
            continue;
        }
        // SAFETY: `pointer` addresses `length` UTF-16 units inside `block`, which is alive.
        let value = unsafe { std::slice::from_raw_parts(pointer as *const u16, length as usize) };
        let description = sys::text(value).trim().to_string();
        if !description.is_empty() {
            return Some(description);
        }
    }
    None
}
