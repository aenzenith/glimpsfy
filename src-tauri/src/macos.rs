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

// ------------------------------------------------------------------------------------ spaces

/// Puts a window that belongs on every Space back on every Space before it is ordered in.
///
/// After the displays changed, the window server kept the overlay a member of one Space only
/// while the window still carried the flag: the next capture opened on that Space, out of sight,
/// and waited there until the user swiped to it. Ordering the window in does not repair it;
/// clearing the flag and setting it again does, once the first change has been committed on its
/// own, because the two in one transaction reach the window server as no change at all. Only a
/// window that is ordered out is checked, where AppKit answers for the Space it would open on.
fn join_every_space(window: &NSWindow) {
    let behaviour = window.collectionBehavior();
    if window.isVisible()
        || !behaviour.contains(NSWindowCollectionBehavior::CanJoinAllSpaces)
        || window.isOnActiveSpace()
    {
        return;
    }
    window.setCollectionBehavior(behaviour & !NSWindowCollectionBehavior::CanJoinAllSpaces);
    if let Some(transaction) = AnyClass::get(c"CATransaction") {
        // SAFETY: `+[CATransaction flush]` takes no arguments and returns nothing; main thread.
        let _: () = unsafe { msg_send![transaction, flush] };
    }
    window.setCollectionBehavior(behaviour);
    crate::trace::log(format!(
        "[glimpsfy] window {} was on another Space and joins every Space again",
        window.windowNumber()
    ));
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
        join_every_space(ns_window);
        ns_window.orderFrontRegardless();
    }
}

/// Shows an overlay at full opacity; `key` gives it the keyboard.
pub fn show_overlay(window: &WebviewWindow, key: bool, mtm: MainThreadMarker) {
    forward_pointer(true);
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

/// Gives a shown overlay the keyboard when anything else has it: another overlay (the pointer
/// moved onto its display), another app, or a window of Glimpsfy's own that a Space switch brought
/// forward (Settings or History left open on that Space). One that waits invisible for its frame,
/// or stepped aside, is left alone, and so is a window in `keep`, which took the keyboard on
/// purpose: ⌘R shows the recording setup with the keyboard before the capture closes, and the
/// setup's Target panel sits at the overlay's own level, so the level could not tell them apart.
pub fn key_overlay(window: &WebviewWindow, keep: &[WebviewWindow], mtm: MainThreadMarker) {
    let Some(ns_window) = ns_window(window, mtm) else {
        return;
    };
    if !ns_window.isVisible() || ns_window.alphaValue() == 0.0 || ns_window.isKeyWindow() {
        return;
    }
    if let Some(key) = objc2_app_kit::NSApplication::sharedApplication(mtm).keyWindow() {
        if keep.iter().filter_map(|w| self::ns_window(w, mtm)).any(|w| std::ptr::eq(w, &*key)) {
            return;
        }
    }
    ns_window.makeKeyAndOrderFront(None);
}

pub fn hide_overlay(window: &WebviewWindow, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        ns_window.orderOut(None);
        ns_window.setAlphaValue(0.0);
        ns_window.setIgnoresMouseEvents(true);
    }
}

thread_local! {
    /// The monitor that hands the overlay the pointer moves meant for the app in front, while a
    /// capture shows (`forward_pointer`).
    static POINTER_FEED: std::cell::RefCell<Option<objc2::rc::Retained<AnyObject>>> =
        const { std::cell::RefCell::new(None) };
}

/// Hands the key overlay every pointer move while a capture shows (`on`), or stops.
///
/// macOS sends pointer moves to the app in front. The overlay is a panel of an app that never
/// comes to the front, so it heard only the moves its tracking area was given, and how many
/// depended on the app in front: at 120 posted moves a second its page saw about 61 while
/// Glimpsfy had been in front, 26 with Finder and 12 with Visual Studio Code. The crosshair,
/// the loupe and the window hover then trailed the pointer, and a Space switch, which changes
/// the app in front, made every later capture feel slower. A global monitor sees the moves the
/// app in front receives, without any permission, and each one goes back through the overlay's
/// own `sendEvent:`, which routes it as AppKit routes a move of its own; with it the page sees
/// about 61 a second whichever app is in front. A global monitor never sees the moves AppKit
/// already delivers to Glimpsfy, so none arrives twice.
pub fn forward_pointer(on: bool) {
    use objc2_app_kit::NSEventMask;

    POINTER_FEED.with_borrow_mut(|feed| {
        if !on {
            if let Some(monitor) = feed.take() {
                // SAFETY: a monitor that `addGlobalMonitorForEventsMatchingMask:` returned.
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
            return;
        }
        if feed.is_some() {
            return;
        }
        let block = block2::RcBlock::new(|event: std::ptr::NonNull<NSEvent>| {
            if let Some(mtm) = MainThreadMarker::new() {
                // SAFETY: AppKit hands the monitor a valid event for the duration of the call.
                forward_move(unsafe { event.as_ref() }, mtm);
            }
        });
        *feed = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::MouseMoved, &block);
    });
}

/// Gives one pointer move to the overlay that has the keyboard, when the pointer is over it: the
/// move AppKit would have delivered had Glimpsfy been the app in front.
fn forward_move(event: &NSEvent, mtm: MainThreadMarker) {
    let Some(window) = objc2_app_kit::NSApplication::sharedApplication(mtm).keyWindow() else {
        return;
    };
    if window.level() != NSScreenSaverWindowLevel || window.alphaValue() == 0.0 || window.ignoresMouseEvents() {
        return;
    }
    let at = NSEvent::mouseLocation();
    let frame = window.frame();
    let inside = at.x >= frame.origin.x
        && at.x < frame.origin.x + frame.size.width
        && at.y >= frame.origin.y
        && at.y < frame.origin.y + frame.size.height;
    if !inside {
        return;
    }
    let local = objc2_foundation::NSPoint::new(at.x - frame.origin.x, at.y - frame.origin.y);
    let null: *const AnyObject = std::ptr::null();
    // SAFETY: the documented factory of a mouse event; its arguments have the declared types, and
    // the graphics context must be nil.
    let moved: Option<objc2::rc::Retained<NSEvent>> = unsafe {
        msg_send![
            NSEvent::class(),
            mouseEventWithType: objc2_app_kit::NSEventType::MouseMoved,
            location: local,
            modifierFlags: event.modifierFlags(),
            timestamp: event.timestamp(),
            windowNumber: window.windowNumber(),
            context: null,
            eventNumber: 0isize,
            clickCount: 0isize,
            pressure: 0.0f32
        ]
    };
    // Calling the web view's `mouseMoved:` directly reached the page only now and then; through
    // the window, every move does.
    if let Some(moved) = moved {
        window.sendEvent(&moved);
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

thread_local! {
    /// The toast's pointer monitors while toasts show (`toast_monitors`).
    static TOAST_MONITORS: std::cell::RefCell<Vec<objc2::rc::Retained<AnyObject>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The toast's window reaches past the toasts for their shadows, so while toasts show a pointer
/// move anywhere sets whether it takes clicks (`glass::toast_hit`). The monitors exist only then:
/// kept for good, they woke the main thread on every pointer move in every app, all day long.
fn toast_monitors(window: &WebviewWindow, on: bool) {
    use objc2_app_kit::NSEventMask;

    TOAST_MONITORS.with_borrow_mut(|monitors| {
        if !on {
            for monitor in monitors.drain(..) {
                // SAFETY: each one is a monitor that `add…MonitorForEventsMatchingMask:` returned.
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
            return;
        }
        if !monitors.is_empty() {
            return;
        }
        let moves = NSEventMask::MouseMoved | NSEventMask::LeftMouseDragged | NSEventMask::RightMouseDragged;
        let hit = {
            let window = window.clone();
            move || {
                if let Some(mtm) = MainThreadMarker::new() {
                    if let Some(ns_window) = ns_window(&window, mtm) {
                        glass::toast_hit(ns_window);
                    }
                }
            }
        };
        // Moves over other apps, and over Glimpsfy's own windows: the pointer reaches a toast
        // through either, depending on whether the window lets clicks through just then.
        let outside = {
            let hit = hit.clone();
            block2::RcBlock::new(move |_: std::ptr::NonNull<NSEvent>| hit())
        };
        let inside = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
            hit();
            event.as_ptr()
        });
        monitors.extend(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(moves, &outside));
        // SAFETY: the block hands back the event it was given, a valid pointer.
        monitors.extend(unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(moves, &inside) });
    });
}

/// Puts the toast window at the bottom-right corner of `screen`'s visible area (above the Dock
/// when it sits at the bottom), reaching `frame.past_right` and `frame.past_below` past it, as
/// tall as the visible area, on screen without the keyboard: opaque while it shows toasts, and
/// invisible and click-through otherwise, so it never leaves the screen and its page never
/// sleeps. The page calls this on every frame of a move, so what is already right is left alone.
pub fn place_toast(window: &WebviewWindow, screen: &Screen, frame: crate::windows::ToastFrame, mtm: MainThreadMarker) {
    if let Some(ns_window) = ns_window(window, mtm) {
        let visible = screen.cocoa_visible;
        let rect = NSRect::new(
            objc2_foundation::NSPoint::new(
                visible.origin.x + visible.size.width + frame.past_right - frame.width,
                visible.origin.y - frame.past_below,
            ),
            objc2_foundation::NSSize::new(frame.width, visible.size.height + frame.past_below),
        );
        if ns_window.frame() != rect {
            ns_window.setFrame_display(rect, true);
        }
        let alpha = if frame.shown { 1.0 } else { 0.0 };
        if ns_window.alphaValue() != alpha {
            ns_window.setAlphaValue(alpha);
        }
        if !frame.shown && !ns_window.ignoresMouseEvents() {
            ns_window.setIgnoresMouseEvents(true);
        }
        if !ns_window.isVisible() {
            join_every_space(ns_window);
            ns_window.orderFrontRegardless();
        }
    }
    toast_monitors(window, frame.shown);
}

/// The pointer in `window`'s own space (points from its top-left corner), or `None` while the
/// window is off screen or invisible. The page asks often, so the window's click-through is
/// checked here too.
pub fn pointer_in(window: &WebviewWindow, mtm: MainThreadMarker) -> Option<LogicalPoint> {
    let ns_window = ns_window(window, mtm)?;
    glass::toast_hit(ns_window);
    if !ns_window.isVisible() || ns_window.alphaValue() == 0.0 {
        return None;
    }
    let point = NSEvent::mouseLocation();
    let frame = ns_window.frame();
    Some(LogicalPoint { x: point.x - frame.origin.x, y: frame.origin.y + frame.size.height - point.y })
}

pub use glass::toast_glass;

/// The toast's native blur. A web page cannot blur what lies behind its transparent window, so
/// AppKit does it: one behind-window `NSVisualEffectView` under each toast, masked to the
/// toast's rounded corners and placed under the web view. The desktop shows plain in the gaps
/// between toasts, and the page draws the tint, the edges and the shadow over the blur.
mod glass {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSAutoresizingMaskOptions, NSBezierPath,
        NSColor, NSEvent, NSImage, NSImageResizingMode, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
        NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowOrderingMode,
    };
    use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};
    use tauri::WebviewWindow;

    use super::ns_window;
    use crate::windows::ToastGlass;

    thread_local! {
        /// The blur under each toast, by the toast's key in the page. AppKit views belong to the
        /// main thread, and so does this map.
        static VIEWS: RefCell<HashMap<String, Retained<NSVisualEffectView>>> = RefCell::default();
    }

    /// Puts a blur under each toast in `glass`, where the toast is drawn at this frame and as
    /// opaque as it is, and takes away the blur of every toast that is no longer listed. The page
    /// drives every move and fade frame by frame, so nothing here animates on its own and the
    /// blur cannot drift from the toast it sits under.
    pub fn toast_glass(window: &WebviewWindow, glass: &[ToastGlass], mtm: MainThreadMarker) {
        let Some(ns_window) = ns_window(window, mtm) else {
            return;
        };
        let Some(content) = ns_window.contentView() else {
            return;
        };
        let size = content.bounds().size;
        let flipped = content.isFlipped();
        VIEWS.with_borrow_mut(|views| {
            views.retain(|key, view| {
                let stays = glass.iter().any(|g| &g.key == key);
                if !stays {
                    view.removeFromSuperview();
                }
                stays
            });
            for toast in glass {
                let y = if flipped { size.height - toast.bottom - toast.height } else { toast.bottom };
                let frame = NSRect::new(
                    NSPoint::new(size.width - toast.right - toast.width, y),
                    NSSize::new(toast.width, toast.height),
                );
                let alpha = toast.alpha.clamp(0.0, 1.0);
                if let Some(view) = views.get(&toast.key) {
                    if view.frame() != frame {
                        view.setFrame(frame);
                    }
                    if view.alphaValue() != alpha {
                        view.setAlphaValue(alpha);
                    }
                    continue;
                }
                let view = blur(frame, toast.radius, flipped, mtm);
                view.setAlphaValue(alpha);
                content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
                views.insert(toast.key.clone(), view);
            }
        });
        // A toast moved under a pointer that stays put.
        toast_hit(ns_window);
    }

    /// Lets clicks through the toast window everywhere but on a toast that is shown (its blur is
    /// the toast's shape, where it is drawn): the window reaches past the toasts for their
    /// shadows, and that margin, over the Dock and other apps, must not catch their clicks.
    pub fn toast_hit(window: &NSWindow) {
        // The window waits on screen, invisible, while no toast is shown.
        if !window.isVisible() || window.alphaValue() == 0.0 {
            if !window.ignoresMouseEvents() {
                window.setIgnoresMouseEvents(true);
            }
            return;
        }
        let Some(content) = window.contentView() else {
            return;
        };
        let point = content.convertPoint_fromView(window.convertPointFromScreen(NSEvent::mouseLocation()), None);
        let on_toast = VIEWS.with_borrow(|views| {
            views.values().any(|view| {
                let frame = view.frame();
                view.alphaValue() > 0.1
                    && point.x >= frame.origin.x
                    && point.x <= frame.origin.x + frame.size.width
                    && point.y >= frame.origin.y
                    && point.y <= frame.origin.y + frame.size.height
            })
        });
        if window.ignoresMouseEvents() == on_toast {
            window.setIgnoresMouseEvents(!on_toast);
        }
    }

    fn blur(frame: NSRect, radius: f64, flipped: bool, mtm: MainThreadMarker) -> Retained<NSVisualEffectView> {
        let view = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        view.setMaterial(NSVisualEffectMaterial::HUDWindow);
        view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        // The toast's panel never becomes key, and an inactive window's material turns flat grey.
        view.setState(NSVisualEffectState::Active);
        // Toasts are dark glass in both system themes.
        // SAFETY: an AppKit constant, valid for the life of the process.
        if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
            view.setAppearance(Some(&dark));
        }
        view.setMaskImage(Some(&rounded(radius)));
        // The window grows and shrinks at its top, and each blur keeps its place from the bottom.
        view.setAutoresizingMask(if flipped {
            NSAutoresizingMaskOptions::ViewMinYMargin
        } else {
            NSAutoresizingMaskOptions::ViewMaxYMargin
        });
        view
    }

    /// A mask with `radius` corners that stretches to any size: its middle row and column grow,
    /// the corners stay.
    fn rounded(radius: f64) -> Retained<NSImage> {
        let side = radius * 2.0 + 1.0;
        let draw = block2::RcBlock::new(move |rect: NSRect| -> Bool {
            NSColor::blackColor().set();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius).fill();
            Bool::YES
        });
        let image = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(side, side), false, &draw);
        image.setCapInsets(NSEdgeInsets { top: radius, left: radius, bottom: radius, right: radius });
        image.setResizingMode(NSImageResizingMode::Stretch);
        image
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

/// The image the general pasteboard holds (`clipboard.rs`): the first image file among the files
/// it names, or else its PNG, JPEG or TIFF data, a TIFF as a PNG. When it names any file its data
/// is never read, because Finder puts the file's icon there. Callable from any thread.
pub fn read_image() -> Option<crate::clipboard::Pasted> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let files = pasteboard_files(&pasteboard);
    if !files.is_empty() {
        return crate::clipboard::first_image(files);
    }
    for kind in ["public.png", "public.jpeg", "public.tiff"] {
        let Some(data) = pasteboard.dataForType(&NSString::from_str(kind)) else {
            continue;
        };
        let bytes = data.to_vec();
        if bytes.is_empty() {
            continue;
        }
        // The web view decodes TIFF only through an image element, and a pin not at all.
        let bytes = if kind == "public.tiff" { png_of(&bytes)? } else { bytes };
        return Some(crate::clipboard::Pasted::Image(bytes));
    }
    None
}

/// The files the pasteboard names, in their order. Finder names them by file reference URLs
/// (`file:///.file/id=…`), which only the system turns into paths.
fn pasteboard_files(pasteboard: &NSPasteboard) -> Vec<std::path::PathBuf> {
    // SAFETY: an AppKit constant, valid for the life of the process.
    let file_url = unsafe { objc2_app_kit::NSPasteboardTypeFileURL };
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let text = item.stringForType(file_url)?;
            let url = objc2_foundation::NSURL::URLWithString(&text)?;
            url.filePathURL()?.to_file_path()
        })
        .collect()
}

/// An image in any format ImageIO reads (TIFF, HEIC, WebP…) as a PNG; the PNG keeps the colour
/// profile and the resolution. Callable from any thread.
pub fn png_of(bytes: &[u8]) -> Option<Vec<u8>> {
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    let rep = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(bytes))?;
    let properties = objc2_foundation::NSDictionary::new();
    // SAFETY: an empty dictionary is a valid property list for every file type.
    let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties) }?;
    Some(png.to_vec())
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
    let cocoa = match point {
        Some((x, y)) => objc2_foundation::NSPoint::new(x, primary_height() - y),
        None => NSEvent::mouseLocation(),
    };
    let screen = screen_at(cocoa, mtm)?;
    let v = screen.visibleFrame();
    let rect = glimpsfy_capture::LogicalRect {
        x: v.origin.x,
        y: primary_height() - v.origin.y - v.size.height,
        width: v.size.width,
        height: v.size.height,
    };
    Some((rect, screen.backingScaleFactor()))
}

/// The screen whose frame holds `point`, in AppKit's coordinates, or the first screen when the
/// point lies on none. The top edge counts as inside: the pointer at the very top of a screen
/// (on its menu bar) reports that screen's height.
fn screen_at(point: objc2_foundation::NSPoint, mtm: MainThreadMarker) -> Option<objc2::rc::Retained<NSScreen>> {
    let all = NSScreen::screens(mtm);
    let found = all.iter().find(|s| {
        let f = s.frame();
        point.x >= f.origin.x
            && point.x < f.origin.x + f.size.width
            && point.y >= f.origin.y
            && point.y <= f.origin.y + f.size.height
    });
    found.or_else(|| all.iter().next())
}

/// Whether two AppKit rectangles are the same, as two frames of one screen are.
fn same_rect(a: NSRect, b: NSRect) -> bool {
    a.origin.x == b.origin.x
        && a.origin.y == b.origin.y
        && a.size.width == b.size.width
        && a.size.height == b.size.height
}

/// The process id of the app in front: the one that has the keyboard and owns the menu bar.
pub fn frontmost_pid() -> Option<i32> {
    let front = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication()?;
    Some(front.processIdentifier()).filter(|pid| *pid > 0)
}

/// Whether an app on this Mac opens `url`, as Launch Services would pick it for a click. Callable
/// from any thread; it only reads the handler table.
pub fn can_open_url(url: &str) -> bool {
    objc2::rc::autoreleasepool(|_| {
        let Some(url) = objc2_foundation::NSURL::URLWithString(&NSString::from_str(url)) else {
            return false;
        };
        objc2_app_kit::NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&url).is_some()
    })
}

/// Puts an ordinary window in the middle of the visible area of the screen that holds `at` (a
/// global top-left point), or of the screen under the pointer without one. With
/// `only_if_elsewhere`, a window whose middle already lies on that screen stays where the user
/// left it. A window wider or taller than the area shrinks to it, since a small external screen
/// may not hold the window's first size. In AppKit's points throughout, so screens of mixed
/// scale place it right.
pub fn center_on_screen(
    window: &WebviewWindow,
    at: Option<LogicalPoint>,
    only_if_elsewhere: bool,
    mtm: MainThreadMarker,
) {
    let Some(ns_window) = ns_window(window, mtm) else {
        return;
    };
    let point = match at {
        Some(p) => objc2_foundation::NSPoint::new(p.x, primary_height() - p.y),
        None => NSEvent::mouseLocation(),
    };
    let Some(screen) = screen_at(point, mtm) else {
        return;
    };
    let frame = ns_window.frame();
    if only_if_elsewhere {
        let whole = screen.frame();
        let middle_x = frame.origin.x + frame.size.width / 2.0;
        let middle_y = frame.origin.y + frame.size.height / 2.0;
        let here = middle_x >= whole.origin.x
            && middle_x < whole.origin.x + whole.size.width
            && middle_y >= whole.origin.y
            && middle_y < whole.origin.y + whole.size.height;
        if here {
            return;
        }
    }
    let area = screen.visibleFrame();
    let width = frame.size.width.min(area.size.width);
    let height = frame.size.height.min(area.size.height);
    let origin = objc2_foundation::NSPoint::new(
        (area.origin.x + (area.size.width - width) / 2.0).round(),
        (area.origin.y + (area.size.height - height) / 2.0).round(),
    );
    ns_window.setFrame_display(NSRect::new(origin, objc2_foundation::NSSize::new(width, height)), true);
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
/// screen, and gives it the keyboard. When the pointer is on another screen, the icon was clicked
/// on that screen's menu bar: with "Displays have separate Spaces" every display shows the icon,
/// while AppKit reports one place for it. The popover then hangs under the pointer on the
/// pointer's screen, and without an icon (it is hidden) it goes to that screen's top-right
/// corner. In AppKit's points throughout: a position in pixels would be read at the scale of the
/// screen the popover was last on.
pub fn show_popover(window: &WebviewWindow, tray: Option<&tauri::tray::TrayIcon>, gap: f64, mtm: MainThreadMarker) {
    let Some(panel) = ns_window(window, mtm) else {
        return;
    };
    let width = panel.frame().size.width;
    let pointer = NSEvent::mouseLocation();
    let here = screen_at(pointer, mtm).map(|s| (s.frame(), s.visibleFrame()));
    let here_frame = here.map(|(frame, _)| frame);
    let placed = tray.and_then(|tray| {
        tray.with_inner_tray_icon(move |inner| {
            let mtm = MainThreadMarker::new()?;
            let item = inner.ns_status_item()?.button(mtm)?.window()?;
            let screen = item.screen()?;
            if here_frame.is_some_and(|frame| !same_rect(frame, screen.frame())) {
                return None;
            }
            let (icon, area) = (item.frame(), screen.visibleFrame());
            let left = (icon.origin.x + (icon.size.width - width) / 2.0)
                .min(area.origin.x + area.size.width - width - gap)
                .max(area.origin.x + gap);
            let top = icon.origin.y.min(area.origin.y + area.size.height) - gap;
            Some((left.round(), top))
        })
        .ok()
        .flatten()
    });
    let (left, top) = placed.unwrap_or_else(|| match here {
        Some((_, area)) => {
            let top = area.origin.y + area.size.height;
            let right = area.origin.x + area.size.width - width - gap;
            // Above the visible area is the menu bar, where the icon was clicked.
            let left = if pointer.y >= top { (pointer.x - width / 2.0).round() } else { right };
            (left.min(right).max(area.origin.x + gap), top - gap)
        }
        None => (0.0, primary_height()),
    });
    panel.setFrameTopLeftPoint(objc2_foundation::NSPoint::new(left, top));
    join_every_space(panel);
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

pub mod scroll;
pub mod send;

pub mod recording {
    use std::cell::RefCell;
    use std::path::Path;
    use std::ptr::NonNull;

    use glimpsfy_capture::{DisplayId, LogicalPoint, LogicalRect};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
    use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBackingStoreType, NSEvent, NSEventMask,
        NSEventModifierFlags, NSPanel, NSPasteboard, NSScreenSaverWindowLevel, NSView, NSVisualEffectBlendingMode,
        NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
    };
    use objc2_core_graphics::CGPath;
    use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL};
    use tauri::{AppHandle, WebviewWindow};

    use super::{join_every_space, make_panel, ns_window, primary_height, to_cocoa};
    use crate::recording::setup::GlassPiece;

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
        join_every_space(w);
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
        resign_key(window, mtm);
    }

    /// Gives the keyboard back to the app in front when `window` holds it (a click on a bar's
    /// button made the bar the key panel). Ordering the panel out and in again hands it back
    /// without activating anything; drawing mode leaves the keyboard to the app being recorded.
    pub fn resign_key(window: &WebviewWindow, mtm: MainThreadMarker) {
        let Some(w) = ns_window(window, mtm) else {
            return;
        };
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
            join_every_space(&bubble.panel);
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

    // ------------------------------------------------------------------ the bar's backdrop
    //
    // The bar window is exactly as large as its glass and sits over the live screen, where the
    // page's `backdrop-filter` finds nothing to blur and its shadow falls outside the window. A
    // native panel under it does both, as the overlay's frozen frame does for the capture
    // toolbar: the dark HUD material blurs and dims the screen (white arrives at 165, black at
    // 24, close to the kit's 158 and 0), the page's glass tint lies over it, and the kit's
    // shadow and outer ring are drawn around it. The panel lets every click through, so the
    // room it takes for the shadow takes no clicks from the app below.

    /// Room around the bar window for the shadows: `--shadow-float` reaches its 12-point offset
    /// and its 32-point blur below the glass.
    const BACKDROP_ROOM: f64 = 48.0;
    /// The kit's glass shadows (`--shadow-float` and `--shadow-small` in `src/styles/tokens.css`),
    /// each as a downward offset, a CSS blur radius and an opacity.
    const SHADOW_FLOAT: [(f64, f64, f32); 2] = [(12.0, 32.0, 0.34), (2.0, 8.0, 0.22)];
    const SHADOW_SMALL: [(f64, f64, f32); 2] = [(4.0, 14.0, 0.30), (1.0, 3.0, 0.20)];
    /// The opacity of the edge's outer ring (`--edge-outer`), which draws the outline on white.
    const EDGE_OUTER: f32 = 0.55;

    thread_local! {
        /// The glass under the bar window, made on the main thread the first time the bar shows.
        static BACKDROP: RefCell<Option<Retained<NSPanel>>> = const { RefCell::new(None) };
        /// The glass under the drawing rail (`rec-rail`), which shows next to the bar's.
        static RAIL_BACKDROP: RefCell<Option<Retained<NSPanel>>> = const { RefCell::new(None) };
    }

    fn make_backdrop(mtm: MainThreadMarker) -> Retained<NSPanel> {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: the panel is owned by `Retained`, so closing must not release it as well.
        unsafe { panel.setReleasedWhenClosed(false) };
        // One level under the bars, so the backdrop is always under its bar and over the target.
        panel.setLevel(NSScreenSaverWindowLevel + 1);
        panel.setCollectionBehavior(behaviour());
        panel.setOpaque(false);
        // SAFETY: NSColor's clear colour, set as the panel's background so only the glass shows.
        unsafe {
            if let Ok(colors) = class(c"NSColor") {
                let clear: Retained<AnyObject> = msg_send![colors, clearColor];
                let _: () = msg_send![&*panel, setBackgroundColor: &*clear];
            }
        }
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);
        // The kit's glass is dark in either system appearance, so the material is too.
        // SAFETY: AppKit's constant for the dark appearance's name.
        if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
            panel.setAppearance(Some(&dark));
        }
        if let Some(view) = panel.contentView() {
            view.setWantsLayer(true);
        }
        panel
    }

    /// The kit's shadow and the edge's outer ring for a piece of glass at `rect` (in `host`'s
    /// bottom-up coordinates), as layers of `host`. Each shadow is cast by its path alone, so it
    /// needs no visible layer; the material over the piece covers the part under the glass.
    ///
    /// # Safety
    ///
    /// `host` must be a CALayer, used on the main thread.
    unsafe fn cast_shadow(host: &AnyObject, bounds: NSRect, rect: NSRect, radius: f64, small: bool, hairline: f64) {
        let Ok(layers) = class(c"CALayer") else {
            return;
        };
        // SAFETY: `radius` is at most half the shorter side (`setup::glass_pieces`), which
        // CGPathCreateWithRoundedRect asks for; the path is retained by every layer it is set on.
        let path = unsafe { CGPath::with_rounded_rect(rect, radius, radius, std::ptr::null()) };
        for (dy, blur, opacity) in if small { SHADOW_SMALL } else { SHADOW_FLOAT } {
            // SAFETY: a new CALayer and its own setters; a CSS blur radius is twice the layer's.
            unsafe {
                let layer: Retained<AnyObject> = msg_send![layers, layer];
                let _: () = msg_send![&*layer, setFrame: bounds];
                let _: () = msg_send![&*layer, setShadowPath: &*path];
                let _: () = msg_send![&*layer, setShadowOpacity: opacity];
                let _: () = msg_send![&*layer, setShadowRadius: blur / 2.0];
                let _: () = msg_send![&*layer, setShadowOffset: NSSize::new(0.0, -dy)];
                let _: () = msg_send![host, addSublayer: &*layer];
            }
        }
        let ring = NSRect::new(
            NSPoint::new(rect.origin.x - hairline, rect.origin.y - hairline),
            NSSize::new(rect.size.width + 2.0 * hairline, rect.size.height + 2.0 * hairline),
        );
        // SAFETY: a new CALayer whose border (black by default) is the ring.
        unsafe {
            let layer: Retained<AnyObject> = msg_send![layers, layer];
            let _: () = msg_send![&*layer, setFrame: ring];
            let _: () = msg_send![&*layer, setCornerRadius: radius + hairline];
            let _: () = msg_send![&*layer, setBorderWidth: hairline];
            let _: () = msg_send![&*layer, setOpacity: EDGE_OUTER];
            let _: () = msg_send![host, addSublayer: &*layer];
        }
    }

    /// Puts the glass under the bar window at `frame` (global top-left points): for each piece
    /// (in points from the window's top-left corner) the material blurs the screen behind it and
    /// the kit's shadow and outer ring sit around it.
    pub fn backdrop_show(bar: &WebviewWindow, frame: LogicalRect, pieces: &[GlassPiece], mtm: MainThreadMarker) {
        glass_show(&BACKDROP, bar, frame, pieces, mtm);
    }

    /// Puts the glass under the drawing rail, as `backdrop_show` does under the bar; the rail
    /// has its own panel because both show at once.
    pub fn rail_backdrop_show(rail: &WebviewWindow, frame: LogicalRect, pieces: &[GlassPiece], mtm: MainThreadMarker) {
        glass_show(&RAIL_BACKDROP, rail, frame, pieces, mtm);
    }

    pub fn rail_backdrop_hide(_mtm: MainThreadMarker) {
        RAIL_BACKDROP.with(|slot| {
            if let Some(panel) = slot.borrow().as_ref() {
                panel.orderOut(None);
            }
        });
    }

    fn glass_show(
        slot: &'static std::thread::LocalKey<RefCell<Option<Retained<NSPanel>>>>,
        window: &WebviewWindow,
        frame: LogicalRect,
        pieces: &[GlassPiece],
        mtm: MainThreadMarker,
    ) {
        let Some(host_window) = ns_window(window, mtm) else {
            return;
        };
        let hairline = 1.0 / host_window.backingScaleFactor().max(1.0);
        slot.with(|slot| {
            let mut slot = slot.borrow_mut();
            let panel = slot.get_or_insert_with(|| make_backdrop(mtm));
            let room = BACKDROP_ROOM;
            let outer = LogicalRect {
                x: frame.x - room,
                y: frame.y - room,
                width: frame.width + 2.0 * room,
                height: frame.height + 2.0 * room,
            };
            panel.setFrame_display(to_cocoa(outer), false);
            let Some(view) = panel.contentView() else {
                return;
            };
            for old in view.subviews().iter() {
                old.removeFromSuperview();
            }
            let bounds = view.bounds();
            let shadows = NSView::initWithFrame(NSView::alloc(mtm), bounds);
            shadows.setWantsLayer(true);
            view.addSubview(&shadows);
            // SAFETY: a layer-backed view's layer is a CALayer.
            let host: Option<Retained<AnyObject>> = unsafe { msg_send![&*shadows, layer] };
            for piece in pieces {
                let r = piece.rect;
                let rect = NSRect::new(
                    NSPoint::new(room + r.x, outer.height - room - r.y - r.height),
                    NSSize::new(r.width, r.height),
                );
                if let Some(host) = &host {
                    // SAFETY: `host` is the shadow view's CALayer, on the main thread.
                    unsafe { cast_shadow(host, bounds, rect, piece.radius, piece.small_shadow, hairline) };
                }
                let glass = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), rect);
                glass.setMaterial(NSVisualEffectMaterial::HUDWindow);
                glass.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
                // Active even though the app never is: a bar over another app keeps its blur.
                glass.setState(NSVisualEffectState::Active);
                glass.setWantsLayer(true);
                // SAFETY: the effect view's CALayer; its corner radius clips the blur as well.
                unsafe {
                    let layer: Option<Retained<AnyObject>> = msg_send![&*glass, layer];
                    if let Some(layer) = layer {
                        let _: () = msg_send![&*layer, setCornerRadius: piece.radius];
                        let _: () = msg_send![&*layer, setMasksToBounds: Bool::YES];
                    }
                }
                view.addSubview(&glass);
            }
            join_every_space(panel);
            panel.orderFrontRegardless();
        });
    }

    /// Lets the bar's window take the mouse (the pointer is over its glass) or pass it to what
    /// lies below (the pointer is over the stage's empty part).
    pub fn take_mouse(window: &WebviewWindow, takes: bool, mtm: MainThreadMarker) {
        if let Some(w) = ns_window(window, mtm) {
            w.setIgnoresMouseEvents(!takes);
        }
    }

    pub fn backdrop_hide(_mtm: MainThreadMarker) {
        BACKDROP.with(|slot| {
            if let Some(panel) = slot.borrow().as_ref() {
                panel.orderOut(None);
            }
        });
    }

    pub fn backdrop_window_number(_mtm: MainThreadMarker) -> Option<u32> {
        BACKDROP.with(|slot| {
            let number = slot.borrow().as_ref()?.windowNumber();
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

    thread_local! {
        /// The pointer monitors while drawing or the spotlight is on (`follow_pointer`).
        static POINTER_MONITORS: RefCell<Vec<Retained<AnyObject>>> = const { RefCell::new(Vec::new()) };
    }

    /// Hands every pointer move to `on_move` while drawing or the spotlight is on, as
    /// `toast_monitors` does: the watch thread's pace would make the spot and the brush ring lag
    /// behind the pointer, and the monitors exist only then because kept for good they would
    /// wake the main thread on every move in every app.
    pub fn follow_pointer(app: &AppHandle, on: bool, on_move: fn(&AppHandle, LogicalPoint), _mtm: MainThreadMarker) {
        POINTER_MONITORS.with_borrow_mut(|monitors| {
            if !on {
                for monitor in monitors.drain(..) {
                    // SAFETY: each one is a monitor that `add…MonitorForEventsMatchingMask:` returned.
                    unsafe { NSEvent::removeMonitor(&monitor) };
                }
                return;
            }
            if !monitors.is_empty() {
                return;
            }
            let moves = NSEventMask::MouseMoved | NSEventMask::LeftMouseDragged | NSEventMask::RightMouseDragged;
            // Moves over other apps, and over the canvas once it takes the mouse to draw.
            let outside = {
                let app = app.clone();
                block2::RcBlock::new(move |_: NonNull<NSEvent>| on_move(&app, super::pointer()))
            };
            let inside = {
                let app = app.clone();
                block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
                    on_move(&app, super::pointer());
                    event.as_ptr()
                })
            };
            monitors.extend(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(moves, &outside));
            // SAFETY: the block hands back the event it was given, a valid pointer.
            monitors.extend(unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(moves, &inside) });
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

// ------------------------------------------------------------------------ agents' approval card

/// What the approval card trusts: which process made a click or a key press, whether the keyboard
/// really holds a chord, and when the screen locks or the Mac sleeps (`agents`).
///
/// Another process of the same user can post clicks and keys into the session (`CGEventPost`,
/// `osascript`). The window server stamps a posted event with its sender's pid, while an event
/// from the hardware carries 0; and the HID system's key state holds only what a keyboard holds
/// down. An agent therefore cannot answer its own request, whatever it posts.
pub mod approval {
    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::sync::Mutex;
    use std::time::Instant;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSEventMask, NSWorkspace};
    use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID};
    use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSOperationQueue, NSString};
    use tauri::{AppHandle, WebviewWindow};

    /// The last click or key press one of Glimpsfy's own windows received while the watch ran.
    #[derive(Clone, Copy, Debug)]
    pub struct Input {
        pub at: Instant,
        /// The event's source process was 0: the hardware made it.
        pub hardware: bool,
        /// The window server's number of the window it went to.
        pub window: isize,
    }

    static LAST: Mutex<Option<Input>> = Mutex::new(None);

    thread_local! {
        static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }

    /// Watches the clicks and key presses Glimpsfy's own windows receive, while a request waits or
    /// an agent's area is being selected. A local monitor sees only events AppKit hands this app,
    /// so it costs nothing while the user works elsewhere.
    pub fn watch_input(on: bool, _mtm: MainThreadMarker) {
        MONITOR.with_borrow_mut(|monitor| {
            if !on {
                if let Some(old) = monitor.take() {
                    // SAFETY: a monitor that `addLocalMonitorForEventsMatchingMask:` returned.
                    unsafe { NSEvent::removeMonitor(&old) };
                }
                *LAST.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return;
            }
            if monitor.is_some() {
                return;
            }
            let mask = NSEventMask::LeftMouseDown | NSEventMask::LeftMouseUp | NSEventMask::KeyDown;
            let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
                // SAFETY: AppKit hands the monitor a valid event for the length of the call.
                let ns_event = unsafe { event.as_ref() };
                let pid = ns_event
                    .CGEvent()
                    .map(|cg| CGEvent::integer_value_field(Some(&cg), CGEventField::EventSourceUnixProcessID))
                    .unwrap_or(-1);
                let input = Input { at: Instant::now(), hardware: pid == 0, window: ns_event.windowNumber() };
                *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(input);
                event.as_ptr()
            });
            // SAFETY: the block hands back the event it was given, a valid pointer.
            *monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        });
    }

    pub fn last_input() -> Option<Input> {
        *LAST.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The window server's number of one of the app's windows.
    pub fn window_number(window: &WebviewWindow, mtm: MainThreadMarker) -> Option<isize> {
        super::ns_window(window, mtm).map(|w| w.windowNumber())
    }

    /// Virtual key codes of the chords that answer (`kVK_ANSI_Y`, `kVK_ANSI_N`, `kVK_ANSI_T`); the
    /// arrows only move through the queue and need no proof.
    pub const KEY_Y: u16 = 0x10;
    pub const KEY_N: u16 = 0x2D;
    pub const KEY_T: u16 = 0x11;

    /// Whether the keyboard holds `key` down with ⌥⇧⌘ now, in the HID system's own state, which a
    /// posted key press does not change. Callable from any thread.
    pub fn chord_held(key: u16) -> bool {
        let state = CGEventSourceStateID::HIDSystemState;
        let chord = CGEventFlags::MaskCommand | CGEventFlags::MaskShift | CGEventFlags::MaskAlternate;
        CGEventSource::flags_state(state).contains(chord) && CGEventSource::key_state(state, key)
    }

    /// Whether the login window covers the session now (the screen is locked).
    pub fn screen_locked() -> bool {
        let Some(session) = objc2_core_graphics::CGSessionCopyCurrentDictionary() else {
            return false;
        };
        let key = objc2_core_foundation::CFString::from_static_str("CGSSessionScreenIsLocked");
        // SAFETY: the key is a CFString and the dictionary holds CF objects.
        let value = unsafe { session.value((&*key as *const objc2_core_foundation::CFString).cast()) };
        !value.is_null()
    }

    /// A change of the session that ends what the card waits for.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SessionChange {
        Locked,
        Unlocked,
        /// The Mac goes to sleep, or another user's session comes forward.
        Away,
    }

    /// Calls `on` on the main thread when the screen locks or unlocks, the Mac goes to sleep, or
    /// another user's session takes the screen. Once, at start; the observers live as long as the
    /// app.
    pub fn watch_session(app: &AppHandle, on: fn(&AppHandle, SessionChange), _mtm: MainThreadMarker) {
        let queue = NSOperationQueue::mainQueue();
        let observe = |center: &objc2_foundation::NSNotificationCenter, name: &NSString, change: SessionChange| {
            let app = app.clone();
            let block = RcBlock::new(move |_: NonNull<NSNotification>| on(&app, change));
            // SAFETY: the block lives as long as the observer, which is kept for good.
            let token =
                unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, Some(&queue), &block) };
            std::mem::forget(token);
        };
        let distributed = NSDistributedNotificationCenter::defaultCenter();
        observe(&distributed, &NSString::from_str("com.apple.screenIsLocked"), SessionChange::Locked);
        observe(&distributed, &NSString::from_str("com.apple.screenIsUnlocked"), SessionChange::Unlocked);
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: the names are AppKit's own constants.
        unsafe {
            observe(&workspace, objc2_app_kit::NSWorkspaceWillSleepNotification, SessionChange::Away);
            observe(&workspace, objc2_app_kit::NSWorkspaceSessionDidResignActiveNotification, SessionChange::Away);
        }
    }
}
