//! A captured frame: a pixel buffer with its format, colour space and scale, and the few
//! operations every output needs (crop, conversion, PNG).

use std::borrow::Cow;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{CaptureError, Result};
use crate::geometry::{PhysicalRect, PhysicalSize, ScaleFactor};

/// Byte order of one 8-bit pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PixelFormat {
    /// Blue, green, red, alpha: what ScreenCaptureKit and Windows.Graphics.Capture deliver.
    Bgra8,
    /// Red, green, blue, alpha: what PNG, the clipboard and `ImageData` take.
    Rgba8,
}

/// The colour space a frame's values are in. A capture is always requested in one of these
/// explicitly, never in the display's native profile, so a frame can be tagged without guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorSpace {
    Srgb,
    DisplayP3,
}

impl ColorSpace {
    /// The name the web platform uses for it (`ImageData`, canvas `colorSpace`).
    pub fn css_name(self) -> &'static str {
        match self {
            ColorSpace::Srgb => "srgb",
            ColorSpace::DisplayP3 => "display-p3",
        }
    }
}

/// A frame's colour space and, when the platform supplies it, the ICC profile that describes it.
/// PNG output embeds the profile (`iCCP`) so the file shows the same colours elsewhere; without a
/// profile an sRGB frame is tagged with the `sRGB` chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    pub space: ColorSpace,
    pub icc_profile: Option<Arc<[u8]>>,
}

impl ColorInfo {
    pub fn srgb() -> Self {
        ColorInfo { space: ColorSpace::Srgb, icc_profile: None }
    }

    /// The colour info of `space` with the platform's ICC profile for it, for pixels that come
    /// from somewhere other than a capture (the overlay's flattened canvas, a file).
    pub fn for_space(space: ColorSpace) -> Self {
        ColorInfo { space, icc_profile: crate::platform::icc_profile(space) }
    }
}

/// One still image of a display, a window or a rectangle.
///
/// Rows may be padded: `bytes_per_row` can exceed `width × 4`. Every pixel is 8-bit with alpha,
/// in `format` order, and the alpha is straight (not premultiplied): a backend that receives
/// premultiplied pixels divides them out once, so every encoder and the clipboard can take them
/// as they are. Captures of screens are opaque; window captures with a shadow carry real alpha.
#[derive(Clone, Debug)]
pub struct Frame {
    pub size: PhysicalSize,
    pub bytes_per_row: usize,
    pub format: PixelFormat,
    pub color: ColorInfo,
    /// Pixels per point of the source, so a frame can be shown at its true logical size.
    pub scale: ScaleFactor,
    pub data: Vec<u8>,
}

/// How hard the PNG encoder works. `Fast` suits the clipboard, `Balanced` suits files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngEffort {
    Fastest,
    Fast,
    Balanced,
}

impl Frame {
    /// Checks the buffer against its stated geometry, so a backend bug surfaces as an error and
    /// never as a read past the end of the buffer.
    pub fn new(
        size: PhysicalSize,
        bytes_per_row: usize,
        format: PixelFormat,
        color: ColorInfo,
        scale: ScaleFactor,
        data: Vec<u8>,
    ) -> Result<Frame> {
        let row = size.width as usize * 4;
        let needed = if size.height == 0 { 0 } else { bytes_per_row * (size.height as usize - 1) + row };
        if bytes_per_row < row || data.len() < needed {
            return Err(CaptureError::Backend(format!(
                "frame buffer of {} bytes does not hold {}×{} pixels at {} bytes per row",
                data.len(),
                size.width,
                size.height,
                bytes_per_row
            )));
        }
        Ok(Frame { size, bytes_per_row, format, color, scale, data })
    }

    /// The pixel at (`x`, `y`) as red, green, blue, alpha, or `None` outside the frame.
    pub fn rgba_at(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.size.width || y >= self.size.height {
            return None;
        }
        let at = y as usize * self.bytes_per_row + x as usize * 4;
        let p = &self.data[at..at + 4];
        Some(match self.format {
            PixelFormat::Bgra8 => [p[2], p[1], p[0], p[3]],
            PixelFormat::Rgba8 => [p[0], p[1], p[2], p[3]],
        })
    }

    /// A new frame holding only `rect`, with tight rows. The rectangle must lie inside the frame.
    pub fn crop(&self, rect: PhysicalRect) -> Result<Frame> {
        let fits = rect.x >= 0
            && rect.y >= 0
            && rect.width > 0
            && rect.height > 0
            && rect.x as u64 + rect.width as u64 <= self.size.width as u64
            && rect.y as u64 + rect.height as u64 <= self.size.height as u64;
        if !fits {
            return Err(CaptureError::InvalidArgument(format!(
                "crop {}×{} at {},{} does not fit in a {}×{} frame",
                rect.width, rect.height, rect.x, rect.y, self.size.width, self.size.height
            )));
        }
        let row = rect.width as usize * 4;
        let mut data = Vec::with_capacity(row * rect.height as usize);
        for y in 0..rect.height as usize {
            let start = (rect.y as usize + y) * self.bytes_per_row + rect.x as usize * 4;
            data.extend_from_slice(&self.data[start..start + row]);
        }
        Frame::new(rect.size(), row, self.format, self.color.clone(), self.scale, data)
    }

    /// The pixels as tight RGBA rows, converting from BGRA when needed.
    pub fn to_rgba(&self) -> Vec<u8> {
        let row = self.size.width as usize * 4;
        let mut out = vec![0u8; row * self.size.height as usize];
        for (y, dst) in out.chunks_exact_mut(row).enumerate() {
            let src = &self.data[y * self.bytes_per_row..y * self.bytes_per_row + row];
            match self.format {
                PixelFormat::Rgba8 => dst.copy_from_slice(src),
                PixelFormat::Bgra8 => swizzle_bgra_rgba(src, dst),
            }
        }
        out
    }

    /// The pixels in the frame's own byte order without row padding, borrowed when the rows are
    /// already tight.
    pub fn tight_bytes(&self) -> Cow<'_, [u8]> {
        let row = self.size.width as usize * 4;
        if self.bytes_per_row == row {
            return Cow::Borrowed(&self.data[..row * self.size.height as usize]);
        }
        let mut out = Vec::with_capacity(row * self.size.height as usize);
        for y in 0..self.size.height as usize {
            out.extend_from_slice(&self.data[y * self.bytes_per_row..y * self.bytes_per_row + row]);
        }
        Cow::Owned(out)
    }

    /// Whether every pixel is fully opaque, which every screen capture is.
    pub fn is_opaque(&self) -> bool {
        let row = self.size.width as usize * 4;
        (0..self.size.height as usize).all(|y| {
            let start = y * self.bytes_per_row;
            self.data[start..start + row].chunks_exact(4).all(|p| p[3] == 255)
        })
    }

    /// The frame composited over an opaque colour (`[r, g, b]` in the frame's colour space), for
    /// formats without alpha and for Settings › Output › Window background. Opaque pixels keep
    /// their values exactly.
    pub fn over(&self, background: [u8; 3]) -> Frame {
        let mut out = Frame {
            size: self.size,
            bytes_per_row: self.size.width as usize * 4,
            format: self.format,
            color: self.color.clone(),
            scale: self.scale,
            data: self.tight_bytes().into_owned(),
        };
        let bg = match self.format {
            PixelFormat::Bgra8 => [background[2], background[1], background[0]],
            PixelFormat::Rgba8 => background,
        };
        for p in out.data.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a == 255 {
                continue;
            }
            for c in 0..3 {
                p[c] = ((p[c] as u32 * a + bg[c] as u32 * (255 - a) + 127) / 255) as u8;
            }
            p[3] = 255;
        }
        out
    }

    /// A PNG of the frame, tagged with its colour space: the ICC profile when there is one, the
    /// sRGB chunk for an sRGB frame without one.
    pub fn encode_png(&self, effort: PngEffort) -> Result<Vec<u8>> {
        let rgba = self.to_rgba();
        let mut out = Vec::with_capacity(rgba.len() / 3);
        let mut info = png::Info::with_size(self.size.width, self.size.height);
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = png::BitDepth::Eight;
        if let Some(profile) = &self.color.icc_profile {
            info.icc_profile = Some(Cow::Borrowed(profile));
        }
        // The resolution says how many pixels make a point, the way macOS's own screenshots do
        // (144 dpi at 2×), so Preview, Pages and the editor show a Retina capture at its size.
        let per_metre = (self.scale.0.max(0.25) * 72.0 / 0.0254).round() as u32;
        info.pixel_dims = Some(png::PixelDimensions { xppu: per_metre, yppu: per_metre, unit: png::Unit::Meter });
        let mut encoder = png::Encoder::with_info(&mut out, info).map_err(|e| CaptureError::Encode(e.to_string()))?;
        if self.color.icc_profile.is_none() && self.color.space == ColorSpace::Srgb {
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        }
        encoder.set_compression(match effort {
            PngEffort::Fastest => png::Compression::Fastest,
            PngEffort::Fast => png::Compression::Fast,
            PngEffort::Balanced => png::Compression::Balanced,
        });
        let mut writer = encoder.write_header().map_err(|e| CaptureError::Encode(e.to_string()))?;
        writer.write_image_data(&rgba).map_err(|e| CaptureError::Encode(e.to_string()))?;
        writer.finish().map_err(|e| CaptureError::Encode(e.to_string()))?;
        Ok(out)
    }

    /// The frame as a top-down 32-bit BMP (`BITMAPINFOHEADER`, `BI_RGB`), whose B, G, R, X byte
    /// order is the BGRA frame as it is. macOS's ImageIO, which WebKit decodes with, refuses the
    /// `BITMAPV4HEADER` and `BI_BITFIELDS` variant, so the header carries no colour space and the
    /// browser reads the pixels as sRGB; a Display P3 frame is drawn with the wrong colours.
    pub fn to_bmp(&self) -> Vec<u8> {
        const HEADER: usize = 14 + 40;
        let row = self.size.width as usize * 4;
        let pixels = row * self.size.height as usize;
        let mut out = Vec::with_capacity(HEADER + pixels);
        let total = (HEADER + pixels) as u32;
        // BITMAPFILEHEADER
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&total.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(HEADER as u32).to_le_bytes());
        // BITMAPINFOHEADER
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(self.size.width as i32).to_le_bytes());
        out.extend_from_slice(&(-(self.size.height as i32)).to_le_bytes()); // negative: top-down rows
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        out.extend_from_slice(&(pixels as u32).to_le_bytes());
        out.extend_from_slice(&2835u32.to_le_bytes());
        out.extend_from_slice(&2835u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for y in 0..self.size.height as usize {
            let src = &self.data[y * self.bytes_per_row..y * self.bytes_per_row + row];
            match self.format {
                PixelFormat::Bgra8 => out.extend_from_slice(src),
                PixelFormat::Rgba8 => {
                    let at = out.len();
                    out.resize(at + row, 0);
                    swizzle_bgra_rgba(src, &mut out[at..]);
                }
            }
        }
        out
    }
}

/// BGRA to RGBA for one row; both slices hold whole pixels.
pub fn swizzle_bgra_rgba(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = s[3];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, padding: usize) -> Frame {
        let row = width as usize * 4 + padding;
        let mut data = vec![0u8; row * height as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let at = y * row + x * 4;
                data[at..at + 4].copy_from_slice(&[x as u8, y as u8, 7, 255]); // B, G, R, A
            }
        }
        let size = PhysicalSize::new(width, height);
        Frame::new(size, row, PixelFormat::Bgra8, ColorInfo::srgb(), ScaleFactor(2.0), data).unwrap()
    }

    /// Cropping a padded BGRA frame must take the right rows and columns: the output image is
    /// exactly what the user selected, and a stride mistake would shear it silently.
    #[test]
    fn a_crop_of_a_padded_frame_takes_exactly_the_selected_pixels() {
        let source = frame(10, 8, 16);
        let crop = source.crop(PhysicalRect::new(3, 2, 4, 5)).unwrap();
        assert_eq!(crop.size, PhysicalSize::new(4, 5));
        assert_eq!(crop.bytes_per_row, 16);
        assert_eq!(crop.rgba_at(0, 0), Some([7, 2, 3, 255]));
        assert_eq!(crop.rgba_at(3, 4), Some([7, 6, 6, 255]));
        assert!(source.crop(PhysicalRect::new(8, 0, 4, 1)).is_err());
    }
}
