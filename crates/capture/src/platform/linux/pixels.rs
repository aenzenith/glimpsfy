//! Pixel work the X11 and the portal paths share: X server images of any common layout become
//! tight BGRA, the pointer is drawn over them, and a recording's frames are cropped and scaled to
//! the even size an encoder needs.

use crate::error::{CaptureError, Result};
use crate::frame::{ColorInfo, Frame, PixelFormat};
use crate::geometry::{PhysicalRect, PhysicalSize, ScaleFactor};

/// How an X server lays out the pixels of one drawable depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ImageLayout {
    pub bits_per_pixel: u8,
    /// Rows are padded to a multiple of this many bits.
    pub scanline_pad: u8,
    pub msb_first: bool,
    pub red: u32,
    pub green: u32,
    pub blue: u32,
}

impl ImageLayout {
    /// The bytes one row of `width` pixels takes, padding included.
    pub fn stride(&self, width: usize) -> usize {
        let pad = (self.scanline_pad as usize).max(8);
        (width * self.bits_per_pixel as usize).div_ceil(pad) * pad / 8
    }

    /// Whether the server's bytes already are B, G, R, X in memory, so a row is copied as it is.
    fn is_bgrx(&self) -> bool {
        self.bits_per_pixel == 32
            && !self.msb_first
            && self.red == 0x00ff_0000
            && self.green == 0x0000_ff00
            && self.blue == 0x0000_00ff
    }
}

/// What the byte beside the three colours of a 32-bit pixel means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Alpha {
    /// Padding with no meaning (every 24-bit visual): the frame is opaque.
    Opaque,
    /// The alpha of an ARGB visual, kept as the server has it (premultiplied by convention).
    Keep,
}

/// One colour channel of a pixel value as 8 bits, whatever the mask's width (5, 6, 8 or 10 bits).
#[derive(Clone, Copy)]
struct Channel {
    mask: u32,
    shift: u32,
    max: u32,
}

impl Channel {
    fn new(mask: u32) -> Channel {
        let shift = if mask == 0 { 0 } else { mask.trailing_zeros() };
        Channel { mask, shift, max: (mask >> shift).max(1) }
    }

    fn read(&self, pixel: u32) -> u8 {
        let value = (pixel & self.mask) >> self.shift;
        if self.max == 255 {
            value as u8
        } else {
            ((value * 255 + self.max / 2) / self.max) as u8
        }
    }
}

/// Appends `rows` rows of an X image (`data`, in `layout`, `width` pixels wide) to `out` as tight
/// BGRA rows.
pub(super) fn append_bgra(
    out: &mut Vec<u8>,
    data: &[u8],
    width: usize,
    rows: usize,
    layout: &ImageLayout,
    alpha: Alpha,
) -> Result<()> {
    let stride = layout.stride(width);
    let bytes = layout.bits_per_pixel as usize / 8;
    if !matches!(layout.bits_per_pixel, 16 | 24 | 32) {
        return Err(CaptureError::Backend(format!(
            "the X server's {}-bit pixels are not a layout this app reads",
            layout.bits_per_pixel
        )));
    }
    if width == 0 || rows == 0 {
        return Ok(());
    }
    if data.len() < stride * (rows - 1) + width * bytes {
        return Err(CaptureError::Backend(format!(
            "the X server sent {} bytes for {width}×{rows} pixels at {stride} bytes per row",
            data.len()
        )));
    }
    out.reserve(width * rows * 4);
    if layout.is_bgrx() {
        for row in 0..rows {
            let start = out.len();
            out.extend_from_slice(&data[row * stride..row * stride + width * 4]);
            if alpha == Alpha::Opaque {
                for pixel in out[start..].chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
            }
        }
        return Ok(());
    }
    let (red, green, blue) = (Channel::new(layout.red), Channel::new(layout.green), Channel::new(layout.blue));
    let keep_alpha = alpha == Alpha::Keep && layout.bits_per_pixel == 32;
    for row in 0..rows {
        let line = &data[row * stride..row * stride + width * bytes];
        for raw in line.chunks_exact(bytes) {
            let mut pixel = 0u32;
            if layout.msb_first {
                for &byte in raw {
                    pixel = (pixel << 8) | byte as u32;
                }
            } else {
                for &byte in raw.iter().rev() {
                    pixel = (pixel << 8) | byte as u32;
                }
            }
            let a = if keep_alpha { (pixel >> 24) as u8 } else { 255 };
            out.extend_from_slice(&[blue.read(pixel), green.read(pixel), red.read(pixel), a]);
        }
    }
    Ok(())
}

/// Turns premultiplied BGRA into the straight alpha a [`Frame`] carries.
pub(super) fn unpremultiply(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        let a = pixel[3] as u32;
        if a == 255 {
            continue;
        }
        if a == 0 {
            pixel[..3].fill(0);
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

/// The pointer as XFixes reports it: premultiplied ARGB values, and where its top-left corner
/// sits in root pixels (the hotspot already taken off).
#[derive(Clone, Debug)]
pub(super) struct Pointer {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub argb: Vec<u32>,
}

/// Draws the pointer over tight BGRA pixels whose top-left corner is at (`origin_x`, `origin_y`)
/// in root pixels. The destination is opaque or premultiplied, for which one formula holds.
pub(super) fn draw_pointer(data: &mut [u8], size: PhysicalSize, origin_x: i32, origin_y: i32, pointer: &Pointer) {
    let (fw, fh) = (size.width as i64, size.height as i64);
    if pointer.argb.len() < pointer.width as usize * pointer.height as usize {
        return;
    }
    for py in 0..pointer.height as i64 {
        let y = pointer.y as i64 - origin_y as i64 + py;
        if y < 0 || y >= fh {
            continue;
        }
        for px in 0..pointer.width as i64 {
            let x = pointer.x as i64 - origin_x as i64 + px;
            if x < 0 || x >= fw {
                continue;
            }
            let source = pointer.argb[(py * pointer.width as i64 + px) as usize];
            let a = source >> 24;
            if a == 0 {
                continue;
            }
            let at = ((y * fw + x) * 4) as usize;
            let Some(pixel) = data.get_mut(at..at + 4) else { return };
            let over = [source & 255, (source >> 8) & 255, (source >> 16) & 255, a];
            for c in 0..4 {
                pixel[c] = (over[c] + (pixel[c] as u32 * (255 - a) + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// An even pixel count of at least 2, which video encoders need.
pub(super) fn even(value: u32) -> u32 {
    (value & !1).max(2)
}

/// The size a stream delivers for a source of `native` pixels: `output_size` when the
/// configuration names one, else the native size divided by `divisor`, both made even.
pub(super) fn stream_size(native: PhysicalSize, output: Option<PhysicalSize>, divisor: u32) -> PhysicalSize {
    match output {
        Some(size) => PhysicalSize::new(even(size.width), even(size.height)),
        None => {
            let divisor = divisor.max(1);
            PhysicalSize::new(even(native.width / divisor), even(native.height / divisor))
        }
    }
}

/// A recording frame of exactly `target` pixels from tight BGRA `data` of `size`.
///
/// A source that is at most one pixel larger per side (an odd size) loses that last row or
/// column instead of being resampled, so a native-size recording stays sharp. An exact half is
/// the 2 × 2 box average, and any other size goes through the area average of `resample` with
/// its proportions kept.
pub(super) fn fit_frame(data: Vec<u8>, size: PhysicalSize, target: PhysicalSize, scale: ScaleFactor) -> Result<Frame> {
    let row = size.width as usize * 4;
    let frame = |size: PhysicalSize, data: Vec<u8>| {
        Frame::new(size, size.width as usize * 4, PixelFormat::Bgra8, ColorInfo::srgb(), scale, data)
    };
    if size == target {
        return frame(size, data);
    }
    let trims = |source: u32, wanted: u32| source >= wanted && source - wanted <= 1;
    if trims(size.width, target.width) && trims(size.height, target.height) {
        return frame(size, data)?.crop(PhysicalRect::new(0, 0, target.width, target.height));
    }
    let halves = |source: u32, wanted: u32| source >= wanted * 2 && source - wanted * 2 <= 3;
    if halves(size.width, target.width) && halves(size.height, target.height) {
        let (tw, th) = (target.width as usize, target.height as usize);
        let mut out = vec![0u8; tw * th * 4];
        for (y, line) in out.chunks_exact_mut(tw * 4).enumerate() {
            let upper = &data[y * 2 * row..];
            let lower = &data[(y * 2 + 1) * row..];
            for (x, pixel) in line.chunks_exact_mut(4).enumerate() {
                let at = x * 8;
                for c in 0..4 {
                    let sum = upper[at + c] as u32
                        + upper[at + 4 + c] as u32
                        + lower[at + c] as u32
                        + lower[at + 4 + c] as u32;
                    pixel[c] = ((sum + 2) / 4) as u8;
                }
            }
        }
        return frame(target, out);
    }
    // Any other size keeps its proportions and is centred on black, as a player would show it:
    // this is a window that changed its size while it was recorded, or an explicit output size.
    let ratio = (target.width as f64 / size.width.max(1) as f64).min(target.height as f64 / size.height.max(1) as f64);
    let inner = PhysicalSize::new(
        ((size.width as f64 * ratio).round() as u32).clamp(1, target.width),
        ((size.height as f64 * ratio).round() as u32).clamp(1, target.height),
    );
    let picture = frame(size, data)?.resized(inner);
    let (left, top) = ((target.width - inner.width) as usize / 2, (target.height - inner.height) as usize / 2);
    let mut out = vec![0u8; target.width as usize * target.height as usize * 4];
    for pixel in out.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let inner_row = inner.width as usize * 4;
    for (y, line) in picture.data.chunks_exact(inner_row).enumerate() {
        let at = ((top + y) * target.width as usize + left) * 4;
        out[at..at + inner_row].copy_from_slice(line);
    }
    frame(target, out)
}

/// Copies `rect` out of tight BGRA `data` of `size`. Parts of the rectangle that lie outside the
/// source come out black, so a region that follows a window keeps its size at the screen's edge.
pub(super) fn crop_padded(data: &[u8], size: PhysicalSize, rect: PhysicalRect) -> Vec<u8> {
    let (w, h) = (rect.width as usize, rect.height as usize);
    let mut out = vec![0u8; w * h * 4];
    for pixel in out.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let left = rect.x.max(0) as i64;
    let top = rect.y.max(0) as i64;
    let right = (rect.x as i64 + rect.width as i64).min(size.width as i64);
    let bottom = (rect.y as i64 + rect.height as i64).min(size.height as i64);
    if right <= left || bottom <= top {
        return out;
    }
    let count = (right - left) as usize * 4;
    for y in top..bottom {
        let from = (y as usize * size.width as usize + left as usize) * 4;
        let to = ((y - rect.y as i64) as usize * w + (left - rect.x as i64) as usize) * 4;
        out[to..to + count].copy_from_slice(&data[from..from + count]);
    }
    out
}
