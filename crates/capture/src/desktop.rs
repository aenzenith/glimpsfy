//! The mapping between logical points and physical pixels on a desktop whose displays share no
//! logical space, which is Windows with monitors of different scale factors.
//!
//! Windows lays its monitors out in one grid of physical pixels (the virtual screen), and a point
//! means a different number of pixels on each of them. The Windows backend therefore defines the
//! global logical space per display:
//!
//! - `Display.frame.x` and `.y` are the monitor's top-left corner in virtual-screen pixels, and
//!   `Display.frame.width` and `.height` are its pixel size divided by its scale. Every display's
//!   logical frame lies inside its own physical rectangle, so the frames of two displays never
//!   overlap.
//! - On one display, `physical = origin + (logical − origin) × scale`, which is invertible.
//! - A global value (a window's bounds, the rectangle of `capture_rect`, the pointer) is
//!   expressed through one display: the one that holds most of a rectangle, or the one whose
//!   rectangle contains a point, and the nearest one when none does.
//!
//! The functions here are that mapping in both directions. They touch no system API, so the app
//! shell uses them to place its windows and to read the pointer, and they run on every host. On
//! a desktop with one logical space (macOS) they are not needed: there a display's frame is
//! already in points of that space.

use crate::display::Display;
use crate::geometry::{LogicalPoint, LogicalRect, PhysicalPoint, PhysicalRect, ScaleFactor};

/// Pixels per point of `display` on each axis, read from its two sizes the way
/// `selection_to_frame` reads them, so a rectangle mapped here and a selection cropped there
/// land on the same pixels.
fn scales(display: &Display) -> (f64, f64) {
    let (x, y) = ScaleFactor::between(display.frame.size(), display.pixel_size);
    let usable = |scale: f64| if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    (usable(x.0), usable(y.0))
}

/// The display's rectangle in virtual-screen pixels.
pub fn physical_frame(display: &Display) -> PhysicalRect {
    PhysicalRect::new(
        display.frame.x.round() as i32,
        display.frame.y.round() as i32,
        display.pixel_size.width,
        display.pixel_size.height,
    )
}

/// A virtual-screen pixel position as a logical point of `display`'s part of the global space.
pub fn logical_point_on(display: &Display, point: PhysicalPoint) -> LogicalPoint {
    let (sx, sy) = scales(display);
    LogicalPoint {
        x: display.frame.x + (point.x as f64 - display.frame.x) / sx,
        y: display.frame.y + (point.y as f64 - display.frame.y) / sy,
    }
}

/// A logical point of `display`'s part of the global space as a virtual-screen pixel position.
pub fn physical_point_on(display: &Display, point: LogicalPoint) -> PhysicalPoint {
    let (sx, sy) = scales(display);
    PhysicalPoint {
        x: (display.frame.x + (point.x - display.frame.x) * sx).round() as i32,
        y: (display.frame.y + (point.y - display.frame.y) * sy).round() as i32,
    }
}

/// A rectangle of virtual-screen pixels as a logical rectangle through `display`.
pub fn logical_rect_on(display: &Display, rect: PhysicalRect) -> LogicalRect {
    let (sx, sy) = scales(display);
    let origin = logical_point_on(display, PhysicalPoint { x: rect.x, y: rect.y });
    LogicalRect::new(origin.x, origin.y, rect.width as f64 / sx, rect.height as f64 / sy)
}

/// A logical rectangle as virtual-screen pixels through `display`. Each edge is rounded to the
/// nearest pixel on its own, as `selection_to_frame` rounds, so two rectangles that share an edge
/// share the pixel row between them. The result is not clipped to the display.
pub fn physical_rect_on(display: &Display, rect: LogicalRect) -> PhysicalRect {
    let (sx, sy) = scales(display);
    let (ox, oy) = (display.frame.x, display.frame.y);
    let left = (ox + (rect.x - ox) * sx).round();
    let top = (oy + (rect.y - oy) * sy).round();
    let right = (ox + (rect.right() - ox) * sx).round();
    let bottom = (oy + (rect.bottom() - oy) * sy).round();
    PhysicalRect::new(left as i32, top as i32, (right - left).max(0.0) as u32, (bottom - top).max(0.0) as u32)
}

/// How far `(x, y)` lies outside the rectangle from `(left, top)` to `(right, bottom)`, squared;
/// 0 inside it.
fn distance_squared(x: f64, y: f64, left: f64, top: f64, right: f64, bottom: f64) -> f64 {
    let dx = (left - x).max(x - right).max(0.0);
    let dy = (top - y).max(y - bottom).max(0.0);
    dx * dx + dy * dy
}

/// The display with the smallest `distance`, the earlier one on a tie (the primary comes first).
fn nearest(displays: &[Display], distance: impl Fn(&Display) -> f64) -> Option<&Display> {
    let mut best: Option<(&Display, f64)> = None;
    for display in displays {
        let d = distance(display);
        if best.is_none_or(|(_, least)| d < least) {
            best = Some((display, d));
        }
    }
    best.map(|(display, _)| display)
}

/// The display with the largest `overlap` above 0, the earlier one on a tie.
fn most(displays: &[Display], overlap: impl Fn(&Display) -> f64) -> Option<&Display> {
    let mut best: Option<(&Display, f64)> = None;
    for display in displays {
        let area = overlap(display);
        if area > 0.0 && best.is_none_or(|(_, most)| area > most) {
            best = Some((display, area));
        }
    }
    best.map(|(display, _)| display)
}

/// The display whose pixels hold `point`, or the nearest one when the point lies on none (a
/// pointer position reported while a monitor is being unplugged).
pub fn display_at_physical_point(displays: &[Display], point: PhysicalPoint) -> Option<&Display> {
    let (x, y) = (point.x as f64, point.y as f64);
    nearest(displays, |display| {
        let frame = physical_frame(display);
        // The right and bottom edges belong to the next display, so the last pixel inside is one
        // short of them.
        let (right, bottom) = (frame.x as f64 + frame.width as f64 - 1.0, frame.y as f64 + frame.height as f64 - 1.0);
        distance_squared(x, y, frame.x as f64, frame.y as f64, right, bottom)
    })
}

/// The display whose logical frame holds `point`, or the nearest one when the point lies in the
/// space between two frames.
pub fn display_at_logical_point(displays: &[Display], point: LogicalPoint) -> Option<&Display> {
    if let Some(display) = displays.iter().find(|display| display.frame.contains(point)) {
        return Some(display);
    }
    nearest(displays, |display| {
        let frame = display.frame;
        distance_squared(point.x, point.y, frame.x, frame.y, frame.right(), frame.bottom())
    })
}

/// The display that holds most of `rect` (virtual-screen pixels), or the nearest one to its
/// centre when it lies on none.
pub fn display_of_physical_rect(displays: &[Display], rect: PhysicalRect) -> Option<&Display> {
    let (left, top) = (rect.x as f64, rect.y as f64);
    let (right, bottom) = (left + rect.width as f64, top + rect.height as f64);
    let held = most(displays, |display| {
        let frame = physical_frame(display);
        let w = right.min(frame.x as f64 + frame.width as f64) - left.max(frame.x as f64);
        let h = bottom.min(frame.y as f64 + frame.height as f64) - top.max(frame.y as f64);
        if w > 0.0 && h > 0.0 {
            w * h
        } else {
            0.0
        }
    });
    held.or_else(|| {
        let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
        nearest(displays, |display| {
            let frame = physical_frame(display);
            let (r, b) = (frame.x as f64 + frame.width as f64, frame.y as f64 + frame.height as f64);
            distance_squared(cx, cy, frame.x as f64, frame.y as f64, r, b)
        })
    })
}

/// The display whose logical frame holds most of `rect`, or the nearest one to its centre when it
/// lies on none. `capture_rect` picks its display the same way.
pub fn display_of_logical_rect(displays: &[Display], rect: LogicalRect) -> Option<&Display> {
    let held =
        most(displays, |display| display.frame.intersection(&rect).map_or(0.0, |common| common.width * common.height));
    held.or_else(|| {
        let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        nearest(displays, |display| {
            let frame = display.frame;
            distance_squared(cx, cy, frame.x, frame.y, frame.right(), frame.bottom())
        })
    })
}

/// A virtual-screen pixel position (the pointer, a window's corner) in the global logical space;
/// `None` when no display is connected.
pub fn physical_to_logical_point(displays: &[Display], point: PhysicalPoint) -> Option<LogicalPoint> {
    display_at_physical_point(displays, point).map(|display| logical_point_on(display, point))
}

/// A point of the global logical space as a virtual-screen pixel position; `None` when no display
/// is connected.
pub fn logical_to_physical_point(displays: &[Display], point: LogicalPoint) -> Option<PhysicalPoint> {
    display_at_logical_point(displays, point).map(|display| physical_point_on(display, point))
}

/// A rectangle of virtual-screen pixels (a window's frame) in the global logical space, through
/// the display that holds most of it; `None` when no display is connected.
pub fn physical_to_logical_rect(displays: &[Display], rect: PhysicalRect) -> Option<LogicalRect> {
    display_of_physical_rect(displays, rect).map(|display| logical_rect_on(display, rect))
}

/// A rectangle of the global logical space as virtual-screen pixels, through the display that
/// holds most of it; `None` when no display is connected.
pub fn logical_to_physical_rect(displays: &[Display], rect: LogicalRect) -> Option<PhysicalRect> {
    display_of_logical_rect(displays, rect).map(|display| physical_rect_on(display, rect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayId;
    use crate::frame::ColorSpace;
    use crate::geometry::{selection_to_frame, PhysicalSize};

    fn display(id: u32, x: i32, y: i32, width: u32, height: u32, scale: f64) -> Display {
        Display {
            id: DisplayId(id),
            name: format!("Display {id}"),
            frame: LogicalRect::new(x as f64, y as f64, width as f64 / scale, height as f64 / scale),
            scale: ScaleFactor(scale),
            pixel_size: PhysicalSize::new(width, height),
            is_primary: id == 1,
            is_builtin: false,
            color_space: ColorSpace::Srgb,
            refresh_rate: None,
        }
    }

    /// On monitors of different scales, a window's frame and a drawn rectangle must come back as
    /// the pixels they went in as, and a rectangle given to `capture_rect` must name the pixels an
    /// area selection of it crops. A slip here places a window on the wrong monitor or saves a
    /// neighbouring strip of the screen, and nothing reports it.
    #[test]
    fn rectangles_keep_their_pixels_across_monitors_of_different_scales() {
        // A 150 % laptop panel, a 100 % monitor to its right and a 125 % one above and to the left.
        let displays = [
            display(1, 0, 0, 2880, 1800, 1.5),
            display(2, 2880, 0, 1920, 1080, 1.0),
            display(3, -2560, -400, 2560, 1440, 1.25),
        ];
        for d in &displays {
            assert_eq!(
                physical_frame(d),
                PhysicalRect::new(d.frame.x as i32, d.frame.y as i32, d.pixel_size.width, d.pixel_size.height)
            );
            for other in &displays {
                assert!(d.id == other.id || d.frame.intersection(&other.frame).is_none(), "logical frames overlap");
            }
        }

        // (pixels, the display that holds most of them)
        let windows = [
            (PhysicalRect::new(300, 150, 1200, 900), 1),
            (PhysicalRect::new(2880, 0, 1920, 1080), 2),
            (PhysicalRect::new(-2560, -400, 2560, 1440), 3),
            (PhysicalRect::new(-1001, -333, 777, 555), 3),
            // Straddling two monitors: mostly on the right one, then mostly on the left one.
            (PhysicalRect::new(2700, 100, 900, 600), 2),
            (PhysicalRect::new(2000, 100, 1000, 600), 1),
        ];
        for (pixels, id) in windows {
            let owner = display_of_physical_rect(&displays, pixels).unwrap();
            assert_eq!(owner.id, DisplayId(id), "{pixels:?}");
            let logical = physical_to_logical_rect(&displays, pixels).unwrap();
            assert_eq!(physical_rect_on(owner, logical), pixels, "{pixels:?} through {logical:?}");
            // A rectangle inside one display also finds that display again from its points.
            if pixels.x >= physical_frame(owner).x && logical.right() <= owner.frame.right() {
                assert_eq!(logical_to_physical_rect(&displays, logical), Some(pixels), "{pixels:?}");
            }
        }

        // What `capture_rect` crops from the display's frame is what the mapping names.
        for d in &displays {
            let rect = LogicalRect::new(d.frame.x + 10.4, d.frame.y + 20.6, 333.3, 111.1);
            let crop = selection_to_frame(rect.relative_to(d.frame.origin()), d.frame.size(), d.pixel_size).unwrap();
            let pixels = logical_to_physical_rect(&displays, rect).unwrap();
            let frame = physical_frame(d);
            assert_eq!(
                (pixels.x - frame.x, pixels.y - frame.y, pixels.width, pixels.height),
                (crop.x, crop.y, crop.width, crop.height)
            );
        }

        // Points: every corner pixel of every display maps there and back, and a point on no
        // display goes to the nearest one.
        for d in &displays {
            let frame = physical_frame(d);
            let corners = [
                PhysicalPoint { x: frame.x, y: frame.y },
                PhysicalPoint { x: frame.x + frame.width as i32 - 1, y: frame.y + frame.height as i32 - 1 },
            ];
            for corner in corners {
                assert_eq!(display_at_physical_point(&displays, corner).unwrap().id, d.id, "{corner:?}");
                let logical = physical_to_logical_point(&displays, corner).unwrap();
                assert!(d.frame.contains(logical), "{corner:?} left its display as {logical:?}");
                assert_eq!(logical_to_physical_point(&displays, logical), Some(corner));
            }
        }
        let beyond = PhysicalPoint { x: 5000, y: 500 };
        assert_eq!(display_at_physical_point(&displays, beyond).unwrap().id, DisplayId(2));
        assert_eq!(physical_to_logical_point(&[], beyond), None);
    }
}
