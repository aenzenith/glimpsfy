//! The macOS side of Send to app: finding an app by its bundle identifier, its icon, bringing it
//! forward (starting it when it is not running), waiting for its window and pressing ⌘V in it.
//!
//! The paste is a synthetic ⌘V posted to the HID event tap, which needs Accessibility. The key is
//! the one that types "v" with ⌘ held in the user's keyboard layout, so a Dvorak or Turkish F
//! keyboard pastes too; Return is never pressed.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use glimpsfy_capture::WindowListOptions;
use objc2::AllocAnyThread;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSBitmapImageFileType, NSBitmapImageRep, NSRunningApplication, NSWorkspace,
    NSWorkspaceOpenConfiguration,
};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};
use objc2_foundation::{NSBundle, NSDictionary, NSFileManager, NSPoint, NSRect, NSSize, NSString, NSURL};
use tauri::{AppHandle, Manager};

use crate::commands::send::SendPhase;
use crate::AppState;

/// How long a send waits for the app's window.
const WINDOW_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);
/// A running app that is not in front with a window by then is opened through Launch Services,
/// which activates it even when the system declined a direct activation, and asks an app that
/// runs without a window for one.
const REOPEN_AFTER: Duration = Duration::from_millis(600);
/// The pause between the window showing and ⌘V, so its text field has the keyboard: a moment
/// for an app that ran already, longer for one that just started.
const SETTLE_RUNNING: Duration = Duration::from_millis(150);
const SETTLE_LAUNCHED: Duration = Duration::from_millis(500);
/// `kVK_ANSI_V`, the key of "v" on a US keyboard, when the layout cannot be read.
const ANSI_V: u16 = 9;

/// The installed app of `bundle_id`: its bundle's path and the name Finder shows for it.
pub fn installed(bundle_id: &str) -> Option<(PathBuf, String)> {
    let url = NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
    let path = PathBuf::from(url.path()?.to_string());
    let name = display_name(&path);
    Some((path, name))
}

/// Whether an app of `bundle_id` runs now.
pub fn running(bundle_id: &str) -> bool {
    running_app(bundle_id).is_some()
}

fn running_app(bundle_id: &str) -> Option<objc2::rc::Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id))
        .iter()
        .find(|app| !app.isTerminated())
}

/// The name Finder shows for an app bundle, without ".app".
fn display_name(path: &Path) -> String {
    let shown = NSFileManager::defaultManager().displayNameAtPath(&NSString::from_str(&path.to_string_lossy()));
    let shown = shown.to_string();
    shown.strip_suffix(".app").map(str::to_string).unwrap_or(shown)
}

/// The bundle identifier and the shown name of the app bundle at `path`.
pub fn bundle_of(path: &Path) -> Option<(String, String)> {
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(&path.to_string_lossy()))?;
    let id = bundle.bundleIdentifier()?.to_string();
    Some((id, display_name(path)))
}

/// The icon of the file at `path` as a PNG data URL, drawn for 64 points. Kept per path, since
/// the send list asks again every time it opens.
pub fn icon(path: &Path) -> Option<String> {
    static ICONS: Mutex<Option<HashMap<PathBuf, Option<String>>>> = Mutex::new(None);
    let mut icons = ICONS.lock().unwrap_or_else(|p| p.into_inner());
    let icons = icons.get_or_insert_with(HashMap::new);
    if let Some(known) = icons.get(path) {
        return known.clone();
    }
    let made = draw_icon(path);
    icons.insert(path.to_path_buf(), made.clone());
    made
}

fn draw_icon(path: &Path) -> Option<String> {
    let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&path.to_string_lossy()));
    let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(64.0, 64.0));
    // SAFETY: `rect` is a valid rectangle for the call to read and adjust; no context or hints.
    let cg = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
    // SAFETY: an empty dictionary is a valid set of properties.
    let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
    Some(format!("data:image/png;base64,{}", crate::output::base64(&png.to_vec())))
}

/// Brings the app of `bundle_id` forward, starting it when it is not running (and calling
/// `opening` then), waits for its first ordinary window, and presses ⌘V when `paste` allows it.
/// Answers the phase the send ended in.
pub fn bring_forward_and_paste(
    app: &AppHandle,
    bundle_id: &str,
    cancel: &AtomicBool,
    paste: bool,
    mut opening: impl FnMut(),
) -> SendPhase {
    let workspace = NSWorkspace::sharedWorkspace();
    let Some(url) = workspace.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id)) else {
        return SendPhase::Failed;
    };
    let launched = match running_app(bundle_id) {
        Some(running) => {
            running.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
            false
        }
        None => {
            opening();
            open(&workspace, &url);
            true
        }
    };

    let started = Instant::now();
    let mut reopened = launched;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return SendPhase::Cancelled;
        }
        if let Some(running) = running_app(bundle_id) {
            let pid = running.processIdentifier();
            let front = crate::macos::frontmost_pid() == Some(pid);
            if running.isFinishedLaunching() && front && has_window(app, pid) {
                break;
            }
            if !reopened && started.elapsed() >= REOPEN_AFTER {
                open(&workspace, &url);
                reopened = true;
            }
        }
        if started.elapsed() >= WINDOW_WAIT {
            return SendPhase::TimedOut;
        }
        std::thread::sleep(POLL);
    }

    std::thread::sleep(if launched { SETTLE_LAUNCHED } else { SETTLE_RUNNING });
    if cancel.load(Ordering::SeqCst) {
        return SendPhase::Cancelled;
    }
    if !paste {
        return SendPhase::NoPermission;
    }
    if press_paste(app) {
        SendPhase::Pasted
    } else {
        SendPhase::Failed
    }
}

/// Opens (or activates) the app at `url` through Launch Services.
fn open(workspace: &NSWorkspace, url: &NSURL) {
    let configuration = NSWorkspaceOpenConfiguration::configuration();
    configuration.setActivates(true);
    workspace.openApplicationAtURL_configuration_completionHandler(url, &configuration, None);
}

/// Whether the app of `pid` shows an ordinary window (layer 0) of some size on screen.
fn has_window(app: &AppHandle, pid: i32) -> bool {
    let Ok(windows) = app.state::<AppState>().backend.windows(WindowListOptions::default()) else {
        return false;
    };
    windows.iter().any(|w| w.pid == pid && w.layer == 0 && w.bounds.width >= 40.0 && w.bounds.height >= 40.0)
}

/// Presses ⌘V (down, then up) with only ⌘ held, whatever modifiers the user still holds.
fn press_paste(app: &AppHandle) -> bool {
    let key = paste_key(app);
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    for down in [true, false] {
        let Some(event) = CGEvent::new_keyboard_event(source.as_deref(), key, down) else {
            return false;
        };
        CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        std::thread::sleep(Duration::from_millis(12));
    }
    true
}

/// The key that types "v" with ⌘ held in the current keyboard layout. The Text Input Sources
/// calls belong to the main thread.
fn paste_key(app: &AppHandle) -> u16 {
    let (sender, receiver) = std::sync::mpsc::channel();
    crate::macos::on_main(app, move |_| {
        let _ = sender.send(layout_key('v'));
    });
    receiver.recv_timeout(Duration::from_secs(1)).ok().flatten().unwrap_or(ANSI_V)
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const c_void,
        key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: usize,
        actual_length: *mut usize,
        unicode: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFRelease(object: *const c_void);
}

/// The virtual key that types `wanted` with ⌘ held in the current layout, or `None` when the
/// layout has no such key or cannot be read (an input method without a key layout).
fn layout_key(wanted: char) -> Option<u16> {
    /// `kUCKeyActionDown`, `(cmdKey >> 8) & 0xFF` and `1 << kUCKeyTranslateNoDeadKeysBit`.
    const KEY_DOWN: u16 = 0;
    const COMMAND: u32 = 1;
    const NO_DEAD_KEYS: u32 = 1;
    // SAFETY: the source is released below; the layout data belongs to it and is read only
    // while it lives; every out-pointer points at a local of the right type and size.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let found = if data.is_null() {
            None
        } else {
            let layout = CFDataGetBytePtr(data).cast::<c_void>();
            let keyboard = LMGetKbdType() as u32;
            (0u16..128).find(|&code| {
                let (mut dead, mut length, mut chars) = (0u32, 0usize, [0u16; 4]);
                let status = UCKeyTranslate(
                    layout,
                    code,
                    KEY_DOWN,
                    COMMAND,
                    keyboard,
                    NO_DEAD_KEYS,
                    &mut dead,
                    chars.len(),
                    &mut length,
                    chars.as_mut_ptr(),
                );
                status == 0
                    && length == 1
                    && char::from_u32(chars[0] as u32).is_some_and(|c| c.eq_ignore_ascii_case(&wanted))
            })
        };
        CFRelease(source);
        found
    }
}
