//! Points, sizes and rectangles in the two coordinate spaces of a capture.
//!
//! - **Logical** values are in points (macOS points, Windows DIPs). The desktop, windows, the
//!   overlay and every selection the user draws use them. The global desktop space has its origin
//!   at the top-left corner of the primary display, and y grows downwards, as Core Graphics'
//!   global display coordinates do. A display-local value has its origin at the top-left corner
//!   of that display.
//! - **Physical** values are in device pixels of one frame. A frame's origin is its top-left
//!   pixel, and nothing physical is ever global, because displays with different scale factors
//!   do not share a pixel grid.
//!
//! [`ScaleFactor`] converts between the two for one display. It is explicit everywhere, so no
//! function silently assumes 1× or 2×.

use serde::{Deserialize, Serialize};

/// Physical pixels per logical point on one display: 2.0 on a Retina display, 1.0 on most
/// external ones, and 1.25 or 1.5 on many Windows laptops.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScaleFactor(pub f64);

impl ScaleFactor {
    pub const ONE: ScaleFactor = ScaleFactor(1.0);

    /// The scale that maps a logical size onto the pixel size of a frame, per axis. A frame of a
    /// scaled display mode is not always exactly `logical × integer`, so the crop maths reads the
    /// real ratio instead of trusting the nominal factor.
    pub fn between(logical: LogicalSize, physical: PhysicalSize) -> (ScaleFactor, ScaleFactor) {
        let x = if logical.width > 0.0 { physical.width as f64 / logical.width } else { 1.0 };
        let y = if logical.height > 0.0 { physical.height as f64 / logical.height } else { 1.0 };
        (ScaleFactor(x), ScaleFactor(y))
    }
}

impl Default for ScaleFactor {
    fn default() -> Self {
        ScaleFactor::ONE
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogicalPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogicalSize {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl LogicalSize {
    pub fn new(width: f64, height: f64) -> Self {
        LogicalSize { width, height }
    }
}

impl PhysicalSize {
    pub fn new(width: u32, height: u32) -> Self {
        PhysicalSize { width, height }
    }

    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

impl LogicalRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        LogicalRect { x, y, width, height }
    }

    pub fn origin(&self) -> LogicalPoint {
        LogicalPoint { x: self.x, y: self.y }
    }

    pub fn size(&self) -> LogicalSize {
        LogicalSize { width: self.width, height: self.height }
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn is_empty(&self) -> bool {
        !(self.width > 0.0 && self.height > 0.0)
    }

    pub fn contains(&self, point: LogicalPoint) -> bool {
        point.x >= self.x && point.y >= self.y && point.x < self.right() && point.y < self.bottom()
    }

    /// The overlap of two rectangles in the same space, or `None` when they do not touch.
    pub fn intersection(&self, other: &LogicalRect) -> Option<LogicalRect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > x && bottom > y).then(|| LogicalRect::new(x, y, right - x, bottom - y))
    }

    /// The same rectangle moved by `-origin`: a global rectangle becomes local to a display whose
    /// frame starts at `origin`.
    pub fn relative_to(&self, origin: LogicalPoint) -> LogicalRect {
        LogicalRect::new(self.x - origin.x, self.y - origin.y, self.width, self.height)
    }
}

impl PhysicalRect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        PhysicalRect { x, y, width, height }
    }

    pub fn size(&self) -> PhysicalSize {
        PhysicalSize { width: self.width, height: self.height }
    }
}

/// Maps a selection drawn on one display to the pixels of that display's frame.
///
/// `selection` is display-local and logical (what the overlay reports), `display` is the
/// display's logical size, and `frame` is the pixel size of the frame captured from it. Each edge
/// is rounded to the nearest pixel on its own, so two selections that share an edge share the
/// pixel row between them, and the result is clamped to the frame. Returns `None` when nothing of
/// the selection lies on the frame.
pub fn selection_to_frame(selection: LogicalRect, display: LogicalSize, frame: PhysicalSize) -> Option<PhysicalRect> {
    if selection.is_empty() || frame.width == 0 || frame.height == 0 {
        return None;
    }
    let (sx, sy) = ScaleFactor::between(display, frame);
    let edge = |value: f64, scale: f64, max: u32| (value * scale).round().clamp(0.0, max as f64) as u32;
    let left = edge(selection.x, sx.0, frame.width);
    let top = edge(selection.y, sy.0, frame.height);
    let right = edge(selection.right(), sx.0, frame.width);
    let bottom = edge(selection.bottom(), sy.0, frame.height);
    (right > left && bottom > top).then(|| PhysicalRect::new(left as i32, top as i32, right - left, bottom - top))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{ColorInfo, Frame, PixelFormat};

    /// A frame whose every pixel names its own position: blue and green hold the low bytes of x
    /// and y, red holds their high nibbles.
    fn coordinates(size: PhysicalSize, scale: f64) -> Frame {
        let mut data = Vec::with_capacity(size.area() as usize * 4);
        for y in 0..size.height {
            for x in 0..size.width {
                data.extend_from_slice(&[x as u8, y as u8, ((x >> 8) | ((y >> 8) << 4)) as u8, 255]);
            }
        }
        Frame::new(size, size.width as usize * 4, PixelFormat::Bgra8, ColorInfo::srgb(), ScaleFactor(scale), data)
            .unwrap()
    }

    fn position(frame: &Frame, x: u32, y: u32) -> (u32, u32) {
        let [r, g, b, _] = frame.rgba_at(x, y).unwrap();
        (b as u32 | ((r as u32 & 15) << 8), g as u32 | ((r as u32 >> 4) << 8))
    }

    /// A frame of flat 4 × 4-point blocks, each with its own colour, so a 1× pixel has one right
    /// value at every scale whose blocks fall on whole pixels.
    fn blocks(size: PhysicalSize, scale: f64) -> Frame {
        let block = |px: u32| (px as f64 / scale / 4.0).floor() as u8;
        let mut data = Vec::with_capacity(size.area() as usize * 4);
        for y in 0..size.height {
            for x in 0..size.width {
                data.extend_from_slice(&[block(x), block(y), 99, 255]);
            }
        }
        Frame::new(size, size.width as usize * 4, PixelFormat::Bgra8, ColorInfo::srgb(), ScaleFactor(scale), data)
            .unwrap()
    }

    /// Whatever the display's scale, a selection must come out of the frame as exactly the pixels
    /// under it, also where it touches or overhangs the display's edges, and "scale to 1×" must
    /// give one pixel per point with each pixel from its own place. An off-by-one here crops or
    /// smears a slightly wrong image, and nobody notices until the file is gone.
    #[test]
    fn selections_crop_and_scale_to_1x_the_right_pixels_at_every_scale() {
        let display = LogicalSize::new(800.0, 500.0);
        for scale in [1.0, 2.0, 1.5, 1.25] {
            let frame_size = PhysicalSize::new((800.0 * scale) as u32, (500.0 * scale) as u32);
            let px = |points: f64| (points * scale).round() as u32;

            // (selection, the part of it on the display)
            let selections = [
                (LogicalRect::new(0.0, 0.0, 120.0, 80.0), LogicalRect::new(0.0, 0.0, 120.0, 80.0)),
                (LogicalRect::new(680.0, 420.0, 120.0, 80.0), LogicalRect::new(680.0, 420.0, 120.0, 80.0)),
                (LogicalRect::new(0.0, 0.0, 800.0, 500.0), LogicalRect::new(0.0, 0.0, 800.0, 500.0)),
                (LogicalRect::new(760.0, -20.0, 100.0, 60.0), LogicalRect::new(760.0, 0.0, 40.0, 40.0)),
                (LogicalRect::new(10.5, 20.5, 33.25, 7.75), LogicalRect::new(10.5, 20.5, 33.25, 7.75)),
            ];
            let source = coordinates(frame_size, scale);
            for (selection, on_display) in selections {
                let crop = selection_to_frame(selection, display, frame_size).unwrap();
                let (left, top) = (px(on_display.x), px(on_display.y));
                let (right, bottom) = (px(on_display.right()), px(on_display.bottom()));
                assert_eq!(
                    crop,
                    PhysicalRect::new(left as i32, top as i32, right - left, bottom - top),
                    "{scale}× {selection:?}"
                );
                assert!(right <= frame_size.width && bottom <= frame_size.height);
                let image = source.crop(crop).unwrap();
                assert_eq!(position(&image, 0, 0), (left, top), "{scale}× {selection:?}");
                assert_eq!(position(&image, image.size.width - 1, image.size.height - 1), (right - 1, bottom - 1));
            }

            // A selection on a secondary display left of the primary one is made local first.
            let secondary = LogicalRect::new(-800.0, 0.0, 800.0, 500.0);
            let global = LogicalRect::new(-120.0, 420.0, 120.0, 80.0);
            let crop = selection_to_frame(global.relative_to(secondary.origin()), display, frame_size).unwrap();
            assert_eq!(
                crop,
                PhysicalRect::new(px(680.0) as i32, px(420.0) as i32, px(800.0) - px(680.0), px(500.0) - px(420.0))
            );

            // Scaling to 1×, on block-aligned selections at the top-left and bottom-right edges.
            let source = blocks(frame_size, scale);
            for selection in [LogicalRect::new(0.0, 0.0, 120.0, 80.0), LogicalRect::new(680.0, 420.0, 120.0, 80.0)] {
                let crop = source.crop(selection_to_frame(selection, display, frame_size).unwrap()).unwrap();
                let one = match crop.at_1x() {
                    Some(scaled) => scaled,
                    None => {
                        assert_eq!(scale, 1.0, "only a 1× capture skips the scaling");
                        crop
                    }
                };
                assert_eq!(one.size, PhysicalSize::new(120, 80), "{scale}× {selection:?}");
                assert_eq!(one.scale, ScaleFactor::ONE);
                for y in 0..80 {
                    for x in 0..120 {
                        let want = (((selection.x as u32 + x) / 4) as u8, ((selection.y as u32 + y) / 4) as u8);
                        let [_, g, b, _] = one.rgba_at(x, y).unwrap();
                        assert_eq!((b, g), want, "{scale}× {selection:?} at {x},{y}");
                    }
                }
            }
        }
    }
}
