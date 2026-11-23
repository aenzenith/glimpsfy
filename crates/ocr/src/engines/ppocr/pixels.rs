//! Pixel access for the PP-OCR stages: a borrowed view of a frame's bytes in the channel order the
//! models were trained on (blue, green, red), and bilinear sampling at fractional positions.

use glimpsfy_capture::{Frame, PixelFormat};

use crate::{OcrError, Result};

/// A frame's pixels, read as blue, green, red whatever the frame's own byte order is.
#[derive(Clone, Copy)]
pub struct Pixels<'a> {
    data: &'a [u8],
    pub width: usize,
    pub height: usize,
    stride: usize,
    /// The byte offsets of blue and red inside a pixel; green is always the second byte.
    blue: usize,
    red: usize,
}

impl<'a> Pixels<'a> {
    /// The view of a frame, or [`OcrError::InvalidImage`] when its buffer is shorter than its
    /// size says, which a frame built without `Frame::new` can be.
    pub fn of(frame: &'a Frame) -> Result<Pixels<'a>> {
        let (width, height) = (frame.size.width as usize, frame.size.height as usize);
        let needed = if height == 0 { 0 } else { frame.bytes_per_row * (height - 1) + width * 4 };
        if frame.bytes_per_row < width * 4 || frame.data.len() < needed {
            return Err(OcrError::InvalidImage("the pixel buffer is shorter than its size".into()));
        }
        let (blue, red) = match frame.format {
            PixelFormat::Bgra8 => (0, 2),
            PixelFormat::Rgba8 => (2, 0),
        };
        Ok(Pixels { data: &frame.data, width, height, stride: frame.bytes_per_row, blue, red })
    }

    /// The pixel at a position inside the image, as blue, green, red.
    #[inline]
    pub fn bgr(&self, x: usize, y: usize) -> [f32; 3] {
        let at = y * self.stride + x * 4;
        let p = &self.data[at..at + 4];
        [p[self.blue] as f32, p[1] as f32, p[self.red] as f32]
    }

    /// The colour at a fractional position, where a pixel's centre lies at its index plus one
    /// half. Positions outside the image read the nearest edge pixel, so a box that reaches past
    /// the border is padded with the border's own colour and not with a hard edge.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let clamp = |v: f32, n: usize| (v.max(0.0) as usize).min(n - 1);
        let (xa, xb) = (clamp(x0, self.width), clamp(x0 + 1.0, self.width));
        let (ya, yb) = (clamp(y0, self.height), clamp(y0 + 1.0, self.height));
        let (p00, p10, p01, p11) = (self.bgr(xa, ya), self.bgr(xb, ya), self.bgr(xa, yb), self.bgr(xb, yb));
        let mut out = [0.0; 3];
        for c in 0..3 {
            let top = p00[c] + (p10[c] - p00[c]) * tx;
            let bottom = p01[c] + (p11[c] - p01[c]) * tx;
            out[c] = top + (bottom - top) * ty;
        }
        out
    }
}
