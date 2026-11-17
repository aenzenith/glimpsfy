//! AppKit glue: panels that take the keyboard without activating Glimpsfy, the overlay windows'
//! level and visibility, the screens, the pointer, the pasteboard and display changes.
//!
//! Every function that takes a `MainThreadMarker` touches AppKit and runs on the main thread
//! only; `on_main` gets there from anywhere.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use glimpsfy_capture::{DisplayId, LogicalPoint};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Imp, NSObjectProtocol, Sel};
use objc2::{msg_send, sel, ClassType, MainThreadMarker};
use objc2_app_kit::{
    NSEvent, NSPanel, NSPasteboard, NSPasteboardTypePNG, NSScreen, NSScreenSaverWindowLevel, NSWindow,
    NSWindowAnimationBehavior, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSData, NSNumber, NSRect, NSString};
use tauri::{AppHandle, WebviewWindow};

// ------------------------------------------------------------------------------- main thread

/// Runs `f` on the main thread: at once when called there, otherwise queued on the event loop.
pub fn on_main(app: &AppHandle, f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    match MainThreadMarker::new() {
        Some(mtm) => f(mtm),
        None => {
            let _ = app.run_on_main_thread(move || {
                if let Some(mtm) = MainThreadMarker::new() {
                    f(mtm);
                }
            });
        }
    }
}

/// What AppKit says about a window, for the dev driver's window list.
#[cfg(debug_assertions)]
pub fn window_facts(window: &WebviewWindow, mtm: MainThreadMarker) -> Option<serde_json::Value> {
    let ns = ns_window(window, mtm)?;
    let frame = ns.frame();
    let occlusion: usize = unsafe { msg_send![ns, occlusionState] };
    let responder = ns.firstResponder().map(|r| r.class().name().to_string_lossy().into_owned());
    Some(serde_json::json!({
        "firstResponder": responder,
        "number": ns.windowNumber(),
        "alpha": ns.alphaValue(),
        "level": ns.level(),
        "key": ns.isKeyWindow(),
        "onScreen": ns.isVisible(),
        "ignoresMouse": ns.ignoresMouseEvents(),
        // NSWindowOcclusionStateVisible is 1 << 1.
        "occlusionVisible": occlusion & 2 != 0,
        "frame": [frame.origin.x, frame.origin.y, frame.size.width, frame.size.height],
    }))
}

/// This window's NSWindow. Only on the main thread, while the window exists.
fn ns_window<'a>(window: &WebviewWindow, _mtm: MainThreadMarker) -> Option<&'a NSWindow> {
    let pointer = window.ns_window().ok()?;
    // SAFETY: tao's NSWindow for this window, alive while the window is; read on the main thread.
    (!pointer.is_null()).then(|| unsafe { &*(pointer as *const NSWindow) })
}

// ------------------------------------------------------------------------------------ panels

/// Turns a window into a non-activating panel, as a menu bar extra's and Spotlight's are: it can
/// take the keyboard while the app the user is in stays in front, so the overlay never steals the
/// front from the app being captured and focus goes back to it when the overlay closes.
///
/// The window's class becomes a subclass of NSPanel. NSPanel adds no fields to NSWindow, and the
/// one field of tao's window class, `focusable`, is declared again in the same place, so tao's
/// window object stays valid. `keyable: false` makes a panel that never takes the keyboard (the
/// toast), so a click in it leaves the keyboard where it was.
pub fn make_panel(window: &WebviewWindow, keyable: bool, mtm: MainThreadMarker) {
    let Some(ns_window) = ns_window(window, mtm) else {
        return;
    };

    extern "C-unwind" fn yes(_: &AnyObject, _: Sel) -> Bool {
        Bool::YES
    }
    extern "C-unwind" fn no(_: &AnyObject, _: Sel) -> Bool {
        Bool::NO
    }

    let name = if keyable { c"GlimpsfyPanel" } else { c"GlimpsfyQuietPanel" };
    let key = if keyable { yes as extern "C-unwind" fn(_, _) -> _ } else { no as extern "C-unwind" fn(_, _) -> _ };
    let class = AnyClass::get(name).or_else(|| {
        let mut builder = ClassBuilder::new(name, NSPanel::class())?;
        builder.add_ivar::<Bool>(c"focusable");
        // SAFETY: both methods match their selectors' signatures (no arguments, BOOL).
        unsafe {
            builder.add_method(sel!(canBecomeKeyWindow), key);
            builder.add_method(sel!(canBecomeMainWindow), no as extern "C-unwind" fn(_, _) -> _);
        }
        Some(builder.register())
    });
    let Some(class) = class else {
        return;
    };

    let object = ns_window as *const NSWindow as *mut AnyObject;
    // SAFETY: this window's NSWindow on the main thread.
    let original = unsafe { objc2::ffi::object_getClass(object) };
    original_classes().insert(object as usize, original as usize);
    // SAFETY: this window's NSWindow on the main thread; the new class has the same layout.
    unsafe { objc2::ffi::object_setClass(object, class) };
    ns_window.setStyleMask(ns_window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    // AppKit tells the window server about that flag only when a window is made. Set later, a
    // click in the panel would still activate Glimpsfy; this private call tells it now.
    if ns_window.respondsToSelector(sel!(_setPreventsActivation:)) {
        let _: () = unsafe { msg_send![ns_window, _setPreventsActivation: true] };
    }
    ns_window.setHidesOnDeactivate(false);
    let _: () = unsafe { msg_send![ns_window, setBecomesKeyOnlyIfNeeded: false] };
    ns_window.setAnimationBehavior(NSWindowAnimationBehavior::None);
}

/// The class each panel had before `make_panel`, by NSWindow address.
fn original_classes() -> std::sync::MutexGuard<'static, std::collections::HashMap<usize, usize>> {
    static CLASSES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<usize, usize>>> =
        std::sync::OnceLock::new();
    CLASSES.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Destroys a window made into a panel. WKWebView observes its window's `contentLayoutRect`
/// through key-value observing, which had already given the window a class of its own before
/// `make_panel` replaced it; when the web view goes, removing that observer from a window of
/// another class throws an Objective-C exception that Rust cannot catch, and the app aborts. So
/// the window gets its original class back first, unless something replaced the panel's class
/// since (then it is left alone).
pub fn destroy_panel(app: &AppHandle, window: WebviewWindow) {
    on_main(app, move |mtm| {
        if let Some(ns_window) = ns_window(&window, mtm) {
            let object = ns_window as *const NSWindow as *mut AnyObject;
            let original = original_classes().remove(&(object as usize));
            // SAFETY: this window's NSWindow on the main thread.
            let current = unsafe { objc2::ffi::object_getClass(object) };
            let ours = [c"GlimpsfyPanel", c"GlimpsfyQuietPanel"]
                .iter()
                .filter_map(|name| AnyClass::get(name))
                .any(|class| std::ptr::eq(class, current));
            if let (Some(original), true) = (original, ours) {
                // SAFETY: the class this same object had before; the layout is unchanged.
                unsafe { objc2::ffi::object_setClass(object, original as *const AnyClass) };
            }
        }
        let _ = window.destroy();
    });
}

/// Set while a panel's window is being made (`without_activating`).
static STAY_BEHIND: AtomicBool = AtomicBool::new(false);

/// Makes a panel's window without Glimpsfy coming to the front for it: wry activates the app for
/// every webview it makes, which would take the front from the app the user is in.
pub fn without_activating<T>(make: impl FnOnce() -> T) -> T {
    STAY_BEHIND.store(true, Ordering::SeqCst);
    let made = make();
    STAY_BEHIND.store(false, Ordering::SeqCst);
    made
}

/// Lets `without_activating` hold Glimpsfy back: AppKit's `activate` and
/// `activateIgnoringOtherApps:` get a wrapper that does nothing while a panel is being made and
/// calls AppKit's own method otherwise. Once, at start.
pub fn guard_activation() {
    use std::mem::transmute;

    type Activate = unsafe extern "C-unwind" fn(&AnyObject, Sel);
    type ActivateIgnoring = unsafe extern "C-unwind" fn(&AnyObject, Sel, Bool);

    static ACTIVATE: OnceLock<Activate> = OnceLock::new();
    static ACTIVATE_IGNORING: OnceLock<ActivateIgnoring> = OnceLock::new();

    unsafe extern "C-unwind" fn activate(this: &AnyObject, cmd: Sel) {
        if let (false, Some(appkit)) = (STAY_BEHIND.load(Ordering::SeqCst), ACTIVATE.get()) {
            unsafe { appkit(this, cmd) };
        }
    }
    unsafe extern "C-unwind" fn activate_ignoring(this: &AnyObject, cmd: Sel, others: Bool) {
        if let (false, Some(appkit)) = (STAY_BEHIND.load(Ordering::SeqCst), ACTIVATE_IGNORING.get()) {
            unsafe { appkit(this, cmd, others) };
        }
    }

    let class = objc2_app_kit::NSApplication::class();
    // SAFETY: each wrapper has the signature of the method it stands in for, and AppKit's method
    // is kept before the wrapper goes in.
    unsafe {
        if let Some(method) = class.instance_method(sel!(activate)) {
            let _ = ACTIVATE.set(transmute::<Imp, Activate>(method.implementation()));
            method.set_implementation(transmute::<Activate, Imp>(activate));
        }
        if let Some(method) = class.instance_method(sel!(activateIgnoringOtherApps:)) {
            let _ = ACTIVATE_IGNORING.set(transmute::<Imp, ActivateIgnoring>(method.implementation()));
            method.set_implementation(transmute::<ActivateIgnoring, Imp>(activate_ignoring));
        }
    }
}

// ----------------------------------------------------------------------------------- screens

/// A screen as AppKit sees it.
#[derive(Clone, Debug)]
pub struct Screen {
    pub id: DisplayId,
    /// AppKit's frame: origin at the bottom-left of the primary screen, y up.
    pub cocoa: NSRect,
    /// The part not covered by the menu bar and the Dock, in AppKit's coordinates.
    pub cocoa_visible: NSRect,
}

pub fn screens(mtm: MainThreadMarker) -> Vec<Screen> {
    let all = NSScreen::screens(mtm);
    let key = NSString::from_str("NSScreenNumber");
    all.iter()
        .filter_map(|screen| {
            let description = screen.deviceDescription();
            let number = description.objectForKey(&key)?.downcast::<NSNumber>().ok()?;
            Some(Screen {
                id: DisplayId(number.unsignedIntValue()),
                cocoa: screen.frame(),
                cocoa_visible: screen.visibleFrame(),
            })
        })
        .collect()
}

/// The pointer in the global desktop space. Callable from any thread.
pub fn pointer() -> LogicalPoint {
    let point = NSEvent::mouseLocation();
    let primary = objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID());
    LogicalPoint { x: point.x, y: primary.size.height - point.y }
}

// ---------------------------------------------------------------------------------- overlays

/// What makes a window an overlay: a keyable non-activating panel at the screen saver level,
/// above the menu bar and the Dock, on every Space and beside full-screen apps, transparent and
/// click-through while it waits.
pub fn setup_overlay(window: &WebviewWindow, screen: &Screen, mtm: MainThreadMarker) {
    make_panel(window, true, mtm);
    let Some(ns_window) = ns_window(window, mtm) else {
        return;
    };
    ns_window.setLevel(NSScreenSaverWindowLevel);
    ns_window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    ns_window.setHasShadow(false);
    ns_window.setAcceptsMouseMovedEvents(true);
    ns_window.setAlphaValue(0.0);
    ns_window.setIgnoresMouseEvents(true);
    ns_window.setFrame_display(screen.cocoa, false);
}

/// Moves an overlay onto its screen again after the screen moved or changed resolution.
pub fn fit_overlay(window: &WebviewWindow, screen: &Screen, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.setFrame_display(screen.cocoa, false);
    }
}

/// Orders an overlay in, invisible and click-through, so its page counts as visible and draws at
/// full speed while the capture runs. ScreenCaptureKit leaves Glimpsfy's windows out of the
/// frame, so the waiting overlay is never in it.
pub fn prewarm_overlay(window: &WebviewWindow, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.setAlphaValue(0.0);
        ns_window.setIgnoresMouseEvents(true);
        ns_window.orderFrontRegardless();
    }
}

/// Shows an overlay at full opacity; `key` gives it the keyboard.
pub fn show_overlay(window: &WebviewWindow, key: bool, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.setIgnoresMouseEvents(false);
        ns_window.setAlphaValue(1.0);
        if key {
            ns_window.makeKeyAndOrderFront(None);
        } else {
            ns_window.orderFrontRegardless();
        }
    }
}

/// Gives a shown overlay the keyboard, when the pointer moves onto its display.
pub fn key_overlay(window: &WebviewWindow, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        if ns_window.isVisible() && !ns_window.isKeyWindow() {
            ns_window.makeKeyAndOrderFront(None);
        }
    }
}

pub fn hide_overlay(window: &WebviewWindow, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.orderOut(None);
        ns_window.setAlphaValue(0.0);
        ns_window.setIgnoresMouseEvents(true);
    }
}

/// The toast: a panel that never takes the keyboard, over full-screen apps too.
pub fn setup_toast(window: &WebviewWindow, mtm: MainThreadMarker) {
    make_panel(window, false, mtm);
    if let Some(ns_window) = ns_window(window, mtm) {
        // Above the overlay's level minus one: over every app, under a running capture.
        ns_window.setLevel(NSScreenSaverWindowLevel - 1);
        ns_window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        ns_window.setHasShadow(false);
    }
}

/// Puts the toast window at the top-right corner of `screen`'s visible area (under the menu bar,
/// clear of the Dock), `size` points large, and shows it without the keyboard.
pub fn place_toast(window: &WebviewWindow, screen: &Screen, width: f64, height: f64, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        let visible = screen.cocoa_visible;
        let frame = NSRect::new(
            objc2_foundation::NSPoint::new(
                visible.origin.x + visible.size.width - width,
                visible.origin.y + visible.size.height - height,
            ),
            objc2_foundation::NSSize::new(width, height),
        );
        ns_window.setFrame_display(frame, true);
        ns_window.orderFrontRegardless();
    }
}

pub fn order_out(window: &WebviewWindow, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.orderOut(None);
    }
}

// -------------------------------------------------------------------------------- pasteboard

/// Puts a PNG on the general pasteboard as `public.png`. The PNG carries its colour profile, so
/// the pasted image keeps its colours.
pub fn copy_png(png: &[u8]) -> Result<(), String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    let data = NSData::with_bytes(png);
    // SAFETY: an AppKit constant, valid for the life of the process.
    let png_type = unsafe { NSPasteboardTypePNG };
    if pasteboard.setData_forType(Some(&data), png_type) {
        Ok(())
    } else {
        Err("the pasteboard refused the image".into())
    }
}

// ---------------------------------------------------------------------------- display changes

static DISPLAY_APP: OnceLock<AppHandle> = OnceLock::new();
static DISPLAY_GENERATION: AtomicU64 = AtomicU64::new(0);

unsafe extern "C-unwind" fn display_changed(
    _display: objc2_core_graphics::CGDirectDisplayID,
    flags: objc2_core_graphics::CGDisplayChangeSummaryFlags,
    _: *mut c_void,
) {
    if flags.contains(objc2_core_graphics::CGDisplayChangeSummaryFlags::BeginConfigurationFlag) {
        return;
    }
    // One change arrives as several calls (one per display and flag); act once they settle.
    let generation = DISPLAY_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(app) = DISPLAY_APP.get().cloned() else {
        return;
    };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        if DISPLAY_GENERATION.load(Ordering::SeqCst) == generation {
            crate::windows::displays_changed(&app);
        }
    });
}

/// Calls `windows::displays_changed` after a display is connected, removed, moved or rescaled.
pub fn watch_displays(app: &AppHandle) {
    if DISPLAY_APP.set(app.clone()).is_ok() {
        // SAFETY: the callback has the declared signature and needs no user data.
        unsafe {
            objc2_core_graphics::CGDisplayRegisterReconfigurationCallback(Some(display_changed), std::ptr::null_mut());
        }
    }
}

// ===================================================================== app shell (popover.rs, pins.rs)
//
// Owner: app shell. The popover under the menu bar icon, the pin windows, the outside-click watch,
// dragging a capture out as a file and the plain pasteboard writes.

/// The primary display's height in points: AppKit's y runs up from its bottom edge, while
/// Glimpsfy's global points run down from its top edge.
fn primary_height() -> f64 {
    objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID()).size.height
}

/// A global top-left rectangle in AppKit's coordinates.
fn to_cocoa(rect: glimpsfy_capture::LogicalRect) -> NSRect {
    NSRect::new(
        objc2_foundation::NSPoint::new(rect.x, primary_height() - rect.y - rect.height),
        objc2_foundation::NSSize::new(rect.width, rect.height),
    )
}

/// The visible area (without the menu bar and the Dock) of the screen that holds `point`, or of
/// the screen under the pointer, in global top-left points, and that screen's scale.
pub fn screen_area(point: Option<(f64, f64)>, mtm: MainThreadMarker) -> Option<(glimpsfy_capture::LogicalRect, f64)> {
    let (x, y) = point.unwrap_or_else(|| {
        let p = pointer();
        (p.x, p.y)
    });
    let cocoa = objc2_foundation::NSPoint::new(x, primary_height() - y);
    let all = NSScreen::screens(mtm);
    let screen = all
        .iter()
        .find(|s| {
            let f = s.frame();
            cocoa.x >= f.origin.x
                && cocoa.x < f.origin.x + f.size.width
                && cocoa.y >= f.origin.y
                && cocoa.y <= f.origin.y + f.size.height
        })
        .or_else(|| all.iter().next())?;
    let v = screen.visibleFrame();
    let rect = glimpsfy_capture::LogicalRect {
        x: v.origin.x,
        y: primary_height() - v.origin.y - v.size.height,
        width: v.size.width,
        height: v.size.height,
    };
    Some((rect, screen.backingScaleFactor()))
}

/// The popover: a keyable non-activating panel at the menu level, on every Space and beside
/// full-screen apps, so it opens over whatever the user is in without bringing Glimpsfy forward.
pub fn setup_popover(window: &WebviewWindow, mtm: MainThreadMarker) {
    make_panel(window, true, mtm);
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.setLevel(objc2_app_kit::NSPopUpMenuWindowLevel);
        ns_window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Transient
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
    }
}

/// Puts the popover `gap` points under the menu bar icon, centred on it and inside the icon's
/// screen, and gives it the keyboard. Without an icon (it is hidden) the popover goes to the
/// top-right corner of the screen under the pointer. In AppKit's points throughout: a position in
/// pixels would be read at the scale of the screen the popover was last on.
pub fn show_popover(window: &WebviewWindow, tray: Option<&tauri::tray::TrayIcon>, gap: f64, mtm: MainThreadMarker) {
    let Some(panel) = ns_window(window, mtm) else {
        return;
    };
    let width = panel.frame().size.width;
    let placed = tray.and_then(|tray| {
        tray.with_inner_tray_icon(move |inner| {
            let mtm = MainThreadMarker::new()?;
            let item = inner.ns_status_item()?.button(mtm)?.window()?;
            let (icon, area) = (item.frame(), item.screen()?.visibleFrame());
            let left = (icon.origin.x + (icon.size.width - width) / 2.0)
                .min(area.origin.x + area.size.width - width - gap)
                .max(area.origin.x + gap);
            let top = icon.origin.y.min(area.origin.y + area.size.height) - gap;
            Some((left.round(), top))
        })
        .ok()
        .flatten()
    });
    let (left, top) = placed.unwrap_or_else(|| match screen_area(None, mtm) {
        Some((visible, _)) => (visible.x + visible.width - width - gap, primary_height() - visible.y - gap),
        None => (0.0, primary_height()),
    });
    panel.setFrameTopLeftPoint(objc2_foundation::NSPoint::new(left, top));
    panel.makeKeyAndOrderFront(None);
    panel.invalidateShadow();
}

/// The popover's page measured itself: the new height, with the top edge kept under the icon.
pub fn fit_popover(window: &WebviewWindow, height: f64, mtm: MainThreadMarker) {
    if let Some(panel) = ns_window(window, mtm) {
        let frame = panel.frame();
        let top = frame.origin.y + frame.size.height;
        let next = NSRect::new(
            objc2_foundation::NSPoint::new(frame.origin.x, top - height),
            objc2_foundation::NSSize::new(frame.size.width, height),
        );
        panel.setFrame_display(next, true);
        panel.invalidateShadow();
    }
}

/// Calls `f` when a mouse button goes down in another app, the menu bar or the Dock: a click
/// that takes the keyboard from no one, so the popover's blur never comes. Clicks in Glimpsfy's
/// own windows are not reported. Once, at start; the monitor stays for good.
pub fn on_outside_click(app: &AppHandle, f: fn(&AppHandle)) {
    use objc2_app_kit::NSEventMask;

    let app = app.clone();
    let block = block2::RcBlock::new(move |_: std::ptr::NonNull<NSEvent>| f(&app));
    let buttons = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
    // The monitor is kept by AppKit until removed, which never happens.
    std::mem::forget(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(buttons, &block));
}

/// A pin: a keyable non-activating panel above other apps' windows, on every Space, without the
/// window's own shadow (the page draws one that fades with the image), at `frame`.
pub fn setup_pin(window: &WebviewWindow, frame: glimpsfy_capture::LogicalRect, mtm: MainThreadMarker) {
    make_panel(window, true, mtm);
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.setLevel(objc2_app_kit::NSFloatingWindowLevel);
        ns_window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        ns_window.setHasShadow(false);
        ns_window.setFrame_display(to_cocoa(frame), false);
        ns_window.orderFrontRegardless();
    }
}

/// Moves and sizes a window to `frame`, in global top-left points.
pub fn set_frame(window: &WebviewWindow, frame: glimpsfy_capture::LogicalRect, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.setFrame_display(to_cocoa(frame), true);
    }
}

/// A window's frame in global top-left points, read from AppKit, so it holds on screens of any
/// scale.
pub fn frame_of(window: &WebviewWindow, mtm: MainThreadMarker) -> Option<glimpsfy_capture::LogicalRect> {
    let frame = ns_window(window, mtm)?.frame();
    Some(glimpsfy_capture::LogicalRect {
        x: frame.origin.x,
        y: primary_height() - frame.origin.y - frame.size.height,
        width: frame.size.width,
        height: frame.size.height,
    })
}

/// A locked pin lets every click through to the app below.
pub fn set_click_through(window: &WebviewWindow, through: bool, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.setIgnoresMouseEvents(through);
    }
}

/// Whether ⌥ is held now. Callable from any thread.
pub fn option_key_down() -> bool {
    NSEvent::modifierFlags_class().contains(objc2_app_kit::NSEventModifierFlags::Option)
}

/// Puts plain text on the general pasteboard.
pub fn copy_text(text: &str) -> Result<(), String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: an AppKit constant, valid for the life of the process.
    let string_type = unsafe { objc2_app_kit::NSPasteboardTypeString };
    if pasteboard.setString_forType(&NSString::from_str(text), string_type) {
        Ok(())
    } else {
        Err("the pasteboard refused the text".into())
    }
}

/// Puts a PNG or a JPEG on the general pasteboard under its own type.
pub fn copy_image(bytes: &[u8], jpeg: bool) -> Result<(), String> {
    if !jpeg {
        return copy_png(bytes);
    }
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    let data = NSData::with_bytes(bytes);
    if pasteboard.setData_forType(Some(&data), &NSString::from_str("public.jpeg")) {
        Ok(())
    } else {
        Err("the pasteboard refused the image".into())
    }
}

use objc2_app_kit::NSDraggingSource;

objc2::define_class!(
    /// The source of a drag out of a Glimpsfy window: it offers a copy of the file, so Finder,
    /// Mail or a chat app makes its own copy and the history keeps the original.
    #[unsafe(super(objc2_foundation::NSObject))]
    #[name = "GlimpsfyDragSource"]
    #[thread_kind = objc2::MainThreadOnly]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(
            &self,
            _session: &objc2_app_kit::NSDraggingSession,
            _context: objc2_app_kit::NSDraggingContext,
        ) -> objc2_app_kit::NSDragOperation {
            objc2_app_kit::NSDragOperation::Copy
        }
    }
);

thread_local! {
    /// The source of the drag in progress; AppKit does not keep it alive on its own.
    static DRAG_SOURCE: std::cell::RefCell<Option<objc2::rc::Retained<DragSource>>> = const { std::cell::RefCell::new(None) };
}

/// Starts dragging `file` out of `window`, with `image` (a file too) under the pointer at
/// `size` points. The page calls it while the mouse button is still down after a drag began.
pub fn drag_out(
    window: &WebviewWindow,
    file: &std::path::Path,
    image: &std::path::Path,
    size: (f64, f64),
    mtm: MainThreadMarker,
) -> Result<(), String> {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{AllocAnyThread, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSDraggingItem, NSEventType, NSImage};
    use objc2_foundation::{NSArray, NSPoint, NSSize, NSURL};

    let ns_window = ns_window(window, mtm).ok_or("the window is gone")?;
    let view = ns_window.contentView().ok_or("the window has no view")?;
    let url = NSURL::from_file_path(file).ok_or("the file has no URL")?;

    // The pointer in the view, for the image's place and for the event that starts the drag.
    let in_window = ns_window.mouseLocationOutsideOfEventStream();
    let in_view = view.convertPoint_fromView(in_window, None);

    let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
    let image_url = NSURL::from_file_path(image).ok_or("the image has no URL")?;
    let picture = NSImage::initByReferencingURL(NSImage::alloc(), &image_url);
    let frame =
        NSRect::new(NSPoint::new(in_view.x - size.0 / 2.0, in_view.y - size.1 / 2.0), NSSize::new(size.0, size.1));
    // SAFETY: the contents of a dragging frame is an NSImage, as the documentation asks.
    unsafe { item.setDraggingFrame_contents(frame, Some(&picture)) };

    // AppKit starts a drag only from a mouse-down or mouse-dragged event. The command arrives
    // after the page saw the drag, so the current event may be another one; a dragged event at
    // the pointer stands in for it then.
    let app = NSApplication::sharedApplication(mtm);
    let current =
        app.currentEvent().filter(|e| matches!(e.r#type(), NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged));
    let event = match current {
        Some(event) => event,
        None => NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDragged,
            in_window,
            objc2_app_kit::NSEventModifierFlags::empty(),
            objc2_foundation::NSProcessInfo::processInfo().systemUptime(),
            ns_window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .ok_or("AppKit made no event to start the drag")?,
    };

    let source: Retained<DragSource> = unsafe { objc2::msg_send![DragSource::alloc(mtm), init] };
    let items = NSArray::from_retained_slice(&[item]);
    let _session = view.beginDraggingSessionWithItems_event_source(&items, &event, ProtocolObject::from_ref(&*source));
    DRAG_SOURCE.with(|slot| *slot.borrow_mut() = Some(source));
    Ok(())
}

// ===================================================================== screen recording (recording/)
//
// Owner: screen recording. The `rec-*` panels' levels and visibility, the webcam bubble (a native
// panel with the camera's live preview), the click and key monitors of a take, the window numbers
// the capture filter needs, and an exported file on the pasteboard.

pub mod recording {
    use std::cell::RefCell;
    use std::path::Path;
    use std::ptr::NonNull;

    use glimpsfy_capture::{DisplayId, LogicalPoint, LogicalRect};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
    use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSBackingStoreType, NSEvent, NSEventMask, NSEventModifierFlags, NSPanel, NSPasteboard,
        NSScreenSaverWindowLevel, NSWindowCollectionBehavior, NSWindowStyleMask,
    };
    use objc2_foundation::{NSArray, NSString, NSURL};
    use tauri::{AppHandle, WebviewWindow};

    use super::{make_panel, ns_window, primary_height, to_cocoa};

    /// The recorder's webview panels.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Panel {
        /// The target over a whole display: the setup, the countdown and the red frame.
        Target,
        /// The setup and control bars.
        Bar,
        /// The recorded canvas: click rings and key caps, click-through.
        Canvas,
        /// The webcam bubble's hover controls.
        BubbleControls,
    }

    fn behaviour() -> NSWindowCollectionBehavior {
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
    }

    /// Makes a `rec-*` window a non-activating panel over every app, on every Space and beside
    /// full-screen apps. The target and the canvas sit at the overlay's level, the bubble one
    /// above them and the bars and the bubble's controls above that, so a bar is never under the
    /// area it belongs to. Only the target and the bars take the keyboard.
    pub fn setup_panel(window: &WebviewWindow, panel: Panel, mtm: MainThreadMarker) {
        make_panel(window, matches!(panel, Panel::Target | Panel::Bar), mtm);
        let Some(w) = ns_window(window, mtm) else {
            return;
        };
        let level = match panel {
            Panel::Target | Panel::Canvas => NSScreenSaverWindowLevel,
            Panel::Bar | Panel::BubbleControls => NSScreenSaverWindowLevel + 2,
        };
        w.setLevel(level);
        w.setCollectionBehavior(behaviour());
        w.setHasShadow(false);
        w.setAcceptsMouseMovedEvents(true);
        if panel == Panel::Canvas {
            w.setIgnoresMouseEvents(true);
        }
    }

    /// Puts a panel at `frame` (global top-left points) and shows it; `key` gives it the keyboard,
    /// `through` lets every click pass to the app below.
    pub fn show(window: &WebviewWindow, frame: LogicalRect, key: bool, through: bool, mtm: MainThreadMarker) {
        let Some(w) = ns_window(window, mtm) else {
            return;
        };
        w.setFrame_display(to_cocoa(frame), true);
        w.setIgnoresMouseEvents(through);
        w.setAlphaValue(1.0);
        if key {
            w.makeKeyAndOrderFront(None);
        } else {
            w.orderFrontRegardless();
        }
    }

    pub fn hide(window: &WebviewWindow, mtm: MainThreadMarker) {
        if let Some(w) = ns_window(window, mtm) {
            w.orderOut(None);
        }
    }

    /// Lets every click through a shown panel (the target while a take runs) and hands the
    /// keyboard back to the app the user records, which never stopped being the active one.
    pub fn pass_through(window: &WebviewWindow, mtm: MainThreadMarker) {
        let Some(w) = ns_window(window, mtm) else {
            return;
        };
        w.setIgnoresMouseEvents(true);
        if w.isKeyWindow() {
            w.orderOut(None);
            w.orderFrontRegardless();
        }
    }

    /// The window server's number of a window, which is the window id ScreenCaptureKit filters by.
    pub fn window_number(window: &WebviewWindow, mtm: MainThreadMarker) -> Option<u32> {
        let number = ns_window(window, mtm)?.windowNumber();
        u32::try_from(number).ok().filter(|n| *n > 0)
    }

    /// The part of a display without the menu bar and the Dock, in global top-left points.
    pub fn visible_frame(display: DisplayId, mtm: MainThreadMarker) -> Option<LogicalRect> {
        let screen = super::screens(mtm).into_iter().find(|s| s.id == display)?;
        let v = screen.cocoa_visible;
        Some(LogicalRect {
            x: v.origin.x,
            y: primary_height() - v.origin.y - v.size.height,
            width: v.size.width,
            height: v.size.height,
        })
    }

    /// Whether the left mouse button is down now. Callable from any thread.
    pub fn left_button_down() -> bool {
        NSEvent::pressedMouseButtons() & 1 != 0
    }

    // ------------------------------------------------------------------------- the bubble

    /// The camera session, started and stopped off the main thread because both calls block.
    struct Session(Retained<AnyObject>);
    // SAFETY: AVCaptureSession is documented as safe to start and stop from any thread; nothing
    // else touches it off the main thread.
    unsafe impl Send for Session {}

    struct Bubble {
        panel: Retained<NSPanel>,
        preview: Option<Retained<AnyObject>>,
        session: Option<Retained<AnyObject>>,
        device: Option<String>,
    }

    thread_local! {
        /// The webcam bubble, made and changed on the main thread only.
        static BUBBLE: RefCell<Option<Bubble>> = const { RefCell::new(None) };
    }

    fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass, String> {
        AnyClass::get(name).ok_or_else(|| format!("{} is not available on this Mac", name.to_string_lossy()))
    }

    /// A session that reads the camera `device` (its unique id), or the default camera.
    fn camera_session(device: Option<&str>) -> Result<Retained<AnyObject>, String> {
        let devices = class(c"AVCaptureDevice")?;
        let found: Option<Retained<AnyObject>> = match device {
            // SAFETY: `deviceWithUniqueID:` takes an NSString and returns a device or nil.
            Some(id) => unsafe { msg_send![devices, deviceWithUniqueID: &*NSString::from_str(id)] },
            None => None,
        };
        let camera = match found {
            Some(camera) => camera,
            None => {
                // SAFETY: `defaultDeviceWithMediaType:` takes AVMediaTypeVideo, whose value is "vide".
                let default: Option<Retained<AnyObject>> =
                    unsafe { msg_send![devices, defaultDeviceWithMediaType: &*NSString::from_str("vide")] };
                default.ok_or("no camera is connected")?
            }
        };
        let error: *mut *mut AnyObject = std::ptr::null_mut();
        // SAFETY: `deviceInputWithDevice:error:` takes a device and an optional error pointer.
        let input: Option<Retained<AnyObject>> =
            unsafe { msg_send![class(c"AVCaptureDeviceInput")?, deviceInputWithDevice: &*camera, error: error] };
        let input = input.ok_or("the camera could not be opened")?;
        // SAFETY: a new, empty capture session.
        let session: Retained<AnyObject> = unsafe { msg_send![class(c"AVCaptureSession")?, new] };
        // SAFETY: `canAddInput:` and `addInput:` take the input made above.
        let can: bool = unsafe { msg_send![&*session, canAddInput: &*input] };
        if !can {
            return Err("the camera is in use by another session".into());
        }
        let _: () = unsafe { msg_send![&*session, addInput: &*input] };
        Ok(session)
    }

    fn run_session(session: &Retained<AnyObject>, start: bool) {
        let session = Session(session.clone());
        std::thread::spawn(move || {
            let session = session;
            // SAFETY: AVCaptureSession's own start and stop, called off the main thread as asked.
            unsafe {
                if start {
                    let _: () = msg_send![&*session.0, startRunning];
                } else {
                    let _: () = msg_send![&*session.0, stopRunning];
                }
            }
        });
    }

    fn shape(bubble: &Bubble, frame: LogicalRect, round: bool) {
        let Some(view) = bubble.panel.contentView() else {
            return;
        };
        // SAFETY: a layer-backed view's layer is a CALayer; these setters take a CGFloat and a BOOL.
        unsafe {
            let layer: Option<Retained<AnyObject>> = msg_send![&*view, layer];
            if let Some(layer) = layer {
                let radius =
                    if round { frame.width.min(frame.height) / 2.0 } else { frame.width.min(frame.height) * 0.22 };
                let _: () = msg_send![&*layer, setCornerRadius: radius];
                let _: () = msg_send![&*layer, setMasksToBounds: Bool::YES];
            }
            if let Some(preview) = &bubble.preview {
                let _: () = msg_send![&**preview, setFrame: view.bounds()];
            }
        }
    }

    fn make_bubble(frame: LogicalRect, mtm: MainThreadMarker) -> Bubble {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            to_cocoa(frame),
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: the panel is owned by `Retained`, so closing must not release it as well.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setLevel(NSScreenSaverWindowLevel + 1);
        panel.setCollectionBehavior(behaviour());
        panel.setOpaque(false);
        // SAFETY: NSColor's clear colour, set as the panel's background so its corners are clear.
        unsafe {
            if let Ok(colors) = class(c"NSColor") {
                let clear: Retained<AnyObject> = msg_send![colors, clearColor];
                let _: () = msg_send![&*panel, setBackgroundColor: &*clear];
            }
        }
        panel.setHasShadow(true);
        panel.setMovableByWindowBackground(true);
        panel.setHidesOnDeactivate(false);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
        if let Some(view) = panel.contentView() {
            view.setWantsLayer(true);
        }
        Bubble { panel, preview: None, session: None, device: None }
    }

    fn attach_camera(bubble: &mut Bubble, device: Option<&str>) -> Result<(), String> {
        if let Some(old) = bubble.session.take() {
            run_session(&old, false);
        }
        if let Some(preview) = bubble.preview.take() {
            // SAFETY: CALayer's own removal from its superlayer.
            unsafe {
                let _: () = msg_send![&*preview, removeFromSuperlayer];
            }
        }
        let session = camera_session(device)?;
        let view = bubble.panel.contentView().ok_or("the bubble has no view")?;
        // SAFETY: `layerWithSession:` makes a preview layer for the session; the gravity is the
        // value of AVLayerVideoGravityResizeAspectFill; 18 is width- and height-sizable.
        unsafe {
            let preview: Retained<AnyObject> =
                msg_send![class(c"AVCaptureVideoPreviewLayer")?, layerWithSession: &*session];
            let _: () =
                msg_send![&*preview, setVideoGravity: &*NSString::from_str("AVLayerVideoGravityResizeAspectFill")];
            let _: () = msg_send![&*preview, setFrame: view.bounds()];
            let _: () = msg_send![&*preview, setAutoresizingMask: 18u32];
            // A webcam reads as a mirror, as every video call shows it.
            let connection: Option<Retained<AnyObject>> = msg_send![&*preview, connection];
            if let Some(connection) = connection {
                let supported: bool = msg_send![&*connection, isVideoMirroringSupported];
                if supported {
                    let _: () = msg_send![&*connection, setAutomaticallyAdjustsVideoMirroring: false];
                    let _: () = msg_send![&*connection, setVideoMirrored: true];
                }
            }
            let layer: Option<Retained<AnyObject>> = msg_send![&*view, layer];
            if let Some(layer) = layer {
                let _: () = msg_send![&*layer, addSublayer: &*preview];
            }
            bubble.preview = Some(preview);
        }
        run_session(&session, true);
        bubble.session = Some(session);
        bubble.device = device.map(str::to_string);
        Ok(())
    }

    /// Shows the webcam bubble at `frame` (global points) with the camera `device`, round or with
    /// rounded corners. The camera opens the first time and when the device changes.
    pub fn bubble_show(
        device: Option<&str>,
        frame: LogicalRect,
        round: bool,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        BUBBLE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let bubble = slot.get_or_insert_with(|| make_bubble(frame, mtm));
            bubble.panel.setFrame_display_animate(to_cocoa(frame), true, false);
            let result = if bubble.session.is_none() || bubble.device.as_deref() != device {
                attach_camera(bubble, device)
            } else {
                Ok(())
            };
            shape(bubble, frame, round);
            bubble.panel.orderFrontRegardless();
            result
        })
    }

    /// Moves the bubble to `frame`, sliding when `animate`.
    pub fn bubble_move(frame: LogicalRect, round: bool, animate: bool, _mtm: MainThreadMarker) {
        BUBBLE.with(|slot| {
            if let Some(bubble) = slot.borrow().as_ref() {
                bubble.panel.setFrame_display_animate(to_cocoa(frame), true, animate);
                shape(bubble, frame, round);
            }
        });
    }

    /// Hides the bubble and turns the camera off.
    pub fn bubble_hide(_mtm: MainThreadMarker) {
        BUBBLE.with(|slot| {
            if let Some(mut bubble) = slot.borrow_mut().take() {
                if let Some(session) = bubble.session.take() {
                    run_session(&session, false);
                }
                bubble.panel.orderOut(None);
            }
        });
    }

    pub fn bubble_window_number(_mtm: MainThreadMarker) -> Option<u32> {
        BUBBLE.with(|slot| {
            let number = slot.borrow().as_ref()?.panel.windowNumber();
            u32::try_from(number).ok().filter(|n| *n > 0)
        })
    }

    // ----------------------------------------------------------------------- input monitors

    thread_local! {
        /// The take's global monitors, removed when it ends.
        static MONITORS: RefCell<Vec<Retained<AnyObject>>> = const { RefCell::new(Vec::new()) };
    }

    /// What a take's monitors report.
    pub struct InputWatch {
        pub clicks: bool,
        pub keys: bool,
        pub only_shortcuts: bool,
        pub on_click: fn(&AppHandle, LogicalPoint),
        pub on_keys: fn(&AppHandle, Vec<String>),
    }

    /// Watches clicks and keys in every other app while a take runs. A click needs no permission;
    /// keys reach a global monitor only with Accessibility, which the bar asks for when the user
    /// turns the overlay on. The keys are turned into key caps at once and handed on; nothing
    /// typed is kept or logged.
    pub fn watch_input(app: &AppHandle, watch: InputWatch, _mtm: MainThreadMarker) {
        unwatch_input(_mtm);
        let mut added = Vec::new();
        if watch.clicks {
            let app = app.clone();
            let on_click = watch.on_click;
            let block = block2::RcBlock::new(move |_: NonNull<NSEvent>| on_click(&app, super::pointer()));
            let mask = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown;
            if let Some(monitor) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block) {
                added.push(monitor);
            }
        }
        if watch.keys {
            let app = app.clone();
            let (on_keys, only) = (watch.on_keys, watch.only_shortcuts);
            let block = block2::RcBlock::new(move |event: NonNull<NSEvent>| {
                // SAFETY: AppKit hands the monitor a valid event for the length of the call.
                let event = unsafe { event.as_ref() };
                if let Some(caps) = key_caps(event, only) {
                    on_keys(&app, caps);
                }
            });
            if let Some(monitor) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
            {
                added.push(monitor);
            }
        }
        MONITORS.with(|m| *m.borrow_mut() = added);
    }

    pub fn unwatch_input(_mtm: MainThreadMarker) {
        MONITORS.with(|m| {
            for monitor in m.borrow_mut().drain(..) {
                // SAFETY: each one is a monitor `addGlobalMonitorForEventsMatchingMask:` returned.
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
        });
    }

    /// The key caps of a key press in the order macOS writes them (⌃⌥⇧⌘ and the key), or `None`
    /// for a repeat, or for a key without ⌘, ⌥ or ⌃ when only shortcuts show.
    fn key_caps(event: &NSEvent, only_shortcuts: bool) -> Option<Vec<String>> {
        if event.isARepeat() {
            return None;
        }
        let flags = event.modifierFlags();
        let ctrl = flags.contains(NSEventModifierFlags::Control);
        let alt = flags.contains(NSEventModifierFlags::Option);
        let shift = flags.contains(NSEventModifierFlags::Shift);
        let cmd = flags.contains(NSEventModifierFlags::Command);
        if only_shortcuts && !(ctrl || alt || cmd) {
            return None;
        }
        let named = match event.keyCode() {
            36 | 76 => "↩",
            48 => "⇥",
            49 => "Space",
            51 => "⌫",
            53 => "⎋",
            117 => "⌦",
            123 => "←",
            124 => "→",
            125 => "↓",
            126 => "↑",
            115 => "↖",
            119 => "↘",
            116 => "⇞",
            121 => "⇟",
            122 => "F1",
            120 => "F2",
            99 => "F3",
            118 => "F4",
            96 => "F5",
            97 => "F6",
            98 => "F7",
            100 => "F8",
            101 => "F9",
            109 => "F10",
            103 => "F11",
            111 => "F12",
            _ => "",
        };
        let key = if named.is_empty() {
            // The key as it reads without ⇧ and ⌥, so ⇧⌘4 shows 4 and not $.
            let plain = event.charactersByApplyingModifiers(NSEventModifierFlags::empty());
            let text = plain.or_else(|| event.charactersIgnoringModifiers())?.to_string();
            let c = text.chars().next().filter(|c| !c.is_control())?;
            c.to_uppercase().collect::<String>()
        } else {
            named.to_string()
        };
        let mut caps = Vec::with_capacity(5);
        for (on, cap) in [(ctrl, "⌃"), (alt, "⌥"), (shift, "⇧"), (cmd, "⌘")] {
            if on {
                caps.push(cap.to_string());
            }
        }
        caps.push(key);
        Some(caps)
    }

    // --------------------------------------------------------------------------- pasteboard

    /// Puts the file itself on the general pasteboard, so pasting into Finder, Mail or a chat app
    /// adds the video or the GIF.
    pub fn copy_file(path: &Path) -> Result<(), String> {
        let url = NSURL::from_file_path(path).ok_or("the file has no URL")?;
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let objects = NSArray::from_retained_slice(&[ProtocolObject::from_retained(url)]);
        if pasteboard.writeObjects(&objects) {
            Ok(())
        } else {
            Err("the pasteboard refused the file".into())
        }
    }
}
