//! The live side of a scrolling capture on macOS: the hole in the overlay that lets the wheel and
//! the trackpad through to the app under the selection, and the scroll events of automatic
//! scrolling, posted to that app alone.

use std::cell::RefCell;

use glimpsfy_capture::{LogicalPoint, LogicalRect, PermissionState};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSEventMask};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGEvent, CGEventField, CGScrollEventUnit};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use tauri::WebviewWindow;

thread_local! {
    /// The pointer monitors of the open hole (`scroll_hole`).
    static MONITORS: RefCell<Vec<Retained<AnyObject>>> = const { RefCell::new(Vec::new()) };
    /// The open hole and the page's own parts that lie over it (`scroll_cover`), in AppKit's
    /// global space.
    static HOLE: RefCell<Option<(NSRect, Vec<NSRect>)>> = const { RefCell::new(None) };
}

/// A window-local rect of logical points in AppKit's global space, whose y grows upwards from the
/// primary display's bottom.
fn to_cocoa(frame: NSRect, rect: LogicalRect) -> NSRect {
    NSRect::new(
        NSPoint::new(frame.origin.x + rect.x, frame.origin.y + frame.size.height - rect.y - rect.height),
        NSSize::new(rect.width, rect.height),
    )
}

fn contains(rect: &NSRect, at: NSPoint) -> bool {
    at.x >= rect.origin.x
        && at.x < rect.origin.x + rect.size.width
        && at.y > rect.origin.y
        && at.y <= rect.origin.y + rect.size.height
}

/// Lets the pointer through while it is over the hole and over none of the page's parts there.
fn retest(window: &WebviewWindow) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(ns_window) = super::ns_window(window, mtm) else { return };
    let at = NSEvent::mouseLocation();
    let through = HOLE.with_borrow(|hole| {
        hole.as_ref()
            .is_some_and(|(hole, covers)| contains(hole, at) && !covers.iter().any(|cover| contains(cover, at)))
    });
    if ns_window.ignoresMouseEvents() != through {
        ns_window.setIgnoresMouseEvents(through);
    }
}

/// While a scrolling capture runs, its display's overlay lets the pointer through wherever it is
/// over `hole` (the selection, window-local logical points), so a scroll there reaches the app
/// under it, and takes the pointer back everywhere else, for its bars and the dim. The overlay
/// keeps the keyboard either way: a scroll never activates the app it reaches, so ↩ and Esc stay
/// with Glimpsfy. The page's own parts over the hole keep the pointer (`scroll_cover`). `None`
/// closes the hole. Like the toast's monitors, these exist only while the
/// hole is open, so no pointer move wakes the main thread the rest of the time.
pub fn scroll_hole(window: &WebviewWindow, hole: Option<LogicalRect>, mtm: MainThreadMarker) {
    MONITORS.with_borrow_mut(|monitors| {
        for monitor in monitors.drain(..) {
            // SAFETY: each one is a monitor that `add…MonitorForEventsMatchingMask:` returned.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    });
    let Some(ns_window) = super::ns_window(window, mtm) else {
        HOLE.with_borrow_mut(|open| *open = None);
        return;
    };
    let Some(hole) = hole else {
        HOLE.with_borrow_mut(|open| *open = None);
        // A hidden overlay stays click-through (`hide_overlay`); only one still on screen, back
        // at the frozen selection, takes the pointer again.
        if ns_window.isVisible() && ns_window.alphaValue() > 0.0 {
            ns_window.setIgnoresMouseEvents(false);
        }
        return;
    };
    HOLE.with_borrow_mut(|open| *open = Some((to_cocoa(ns_window.frame(), hole), Vec::new())));
    let hit = {
        let window = window.clone();
        move || retest(&window)
    };
    hit();
    let mask = NSEventMask::MouseMoved
        | NSEventMask::LeftMouseDragged
        | NSEventMask::RightMouseDragged
        | NSEventMask::ScrollWheel;
    // Moves the app under the hole receives, and the ones the overlay receives outside it.
    let outside = {
        let hit = hit.clone();
        block2::RcBlock::new(move |_: std::ptr::NonNull<NSEvent>| hit())
    };
    let inside = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        hit();
        event.as_ptr()
    });
    MONITORS.with_borrow_mut(|monitors| {
        monitors.extend(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &outside));
        // SAFETY: the block hands back the event it was given, a valid pointer.
        monitors.extend(unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &inside) });
    });
}

/// The page's own parts that lie over the open hole (window-local logical points): a pop, or the
/// strip and the bar when the selection leaves them no room outside it. They take the pointer
/// back, so their buttons can be clicked; the rest of the hole still lets it through. Nothing
/// happens while no hole is open.
pub fn scroll_cover(window: &WebviewWindow, covers: &[LogicalRect], mtm: MainThreadMarker) {
    let Some(ns_window) = super::ns_window(window, mtm) else { return };
    let frame = ns_window.frame();
    let open = HOLE.with_borrow_mut(|open| match open {
        Some((_, list)) => {
            *list = covers.iter().map(|cover| to_cocoa(frame, *cover)).collect();
            true
        }
        None => false,
    });
    if open {
        retest(window);
    }
}

/// Posts one scroll of `dy` points (negative moves the page down) to process `pid`, as if the
/// wheel turned with the pointer at `at` (global points) over `window`. The pointer does not move
/// and the app does not come forward. macOS takes such events only from an app with the
/// accessibility permission, so nothing is posted without it; `false` then.
pub fn post_scroll(pid: i32, window: Option<u32>, at: LogicalPoint, dy: i32) -> bool {
    if crate::permissions::accessibility() != PermissionState::Granted {
        return false;
    }
    let Some(event) = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 1, dy, 0, 0) else {
        return false;
    };
    // Core Graphics' global space has its origin at the primary display's top-left corner, as the
    // app's logical points do.
    CGEvent::set_location(Some(&event), CGPoint::new(at.x, at.y));
    CGEvent::set_integer_value_field(Some(&event), CGEventField::ScrollWheelEventIsContinuous, 1);
    if let Some(window) = window {
        // The app hands the scroll to this window, not to whichever of its windows the location
        // would hit first.
        for field in [
            CGEventField::MouseEventWindowUnderMousePointer,
            CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
        ] {
            CGEvent::set_integer_value_field(Some(&event), field, window as i64);
        }
    }
    CGEvent::post_to_pid(pid, Some(&event));
    true
}
