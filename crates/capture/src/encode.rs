//! Image files from frames: PNG, JPEG and WebP, each written with pure-Rust encoders.
//!
//! **Colour.** A frame is never converted: each format carries the frame's ICC profile, so a
//! Display P3 capture shows the same colours in every viewer that reads profiles.
//!
//! - PNG embeds the profile as `iCCP`, or the `sRGB` chunk for an sRGB frame without a profile.
//! - JPEG embeds the profile as `APP2 ICC_PROFILE` segments.
//! - WebP uses the extended container (`VP8X`) with an `ICCP` chunk.
//!
//! An sRGB frame without a profile is written untagged in JPEG and WebP, which every decoder reads
//! as sRGB.
//!
//! **Alpha.** PNG and WebP keep it. JPEG has none, so a frame with transparent pixels (a window
//! with its shadow) is composited over the background colour first.

use crate::error::{CaptureError, Result};
use crate::frame::{Frame, PixelFormat, PngEffort};

/// JPEG stores its dimensions in 16 bits.
const JPEG_MAX_SIDE: u32 = u16::MAX as u32;
/// WebP stores its dimensions in 14 bits.
const WEBP_MAX_SIDE: u32 = 16_384;

/// How to write a frame to a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageEncoding {
    Png(PngEffort),
    /// Lossy JPEG at `quality` (1 to 100; the app clamps Settings › Output › Quality to 50–100).
    /// Below 90 the colour is subsampled 4:2:0, at 90 and above it keeps every colour sample
    /// (4:4:4), which keeps coloured text sharp.
    Jpeg {
        quality: u8,
        /// What transparent pixels are composited over, as `[r, g, b]`.
        background: [u8; 3],
    },
    /// Lossless WebP (VP8L). The quality setting does not apply.
    WebpLossless,
}

impl ImageEncoding {
    /// The file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ImageEncoding::Png(_) => "png",
            ImageEncoding::Jpeg { .. } => "jpg",
            ImageEncoding::WebpLossless => "webp",
        }
    }
}

impl Frame {
    /// The frame as an image file in `encoding`, tagged with its colour space.
    pub fn encode(&self, encoding: ImageEncoding) -> Result<Vec<u8>> {
        if self.size.width == 0 || self.size.height == 0 {
            return Err(CaptureError::InvalidArgument("an empty image cannot be written".into()));
        }
        match encoding {
            ImageEncoding::Png(effort) => self.encode_png(effort),
            ImageEncoding::Jpeg { quality, background } => self.encode_jpeg(quality, background),
            ImageEncoding::WebpLossless => self.encode_webp(),
        }
    }

    fn encode_jpeg(&self, quality: u8, background: [u8; 3]) -> Result<Vec<u8>> {
        let (width, height) = (self.size.width, self.size.height);
        if width > JPEG_MAX_SIDE || height > JPEG_MAX_SIDE {
            return Err(too_large("JPEG", JPEG_MAX_SIDE, width, height));
        }
        let flat;
        let source = if self.is_opaque() {
            self
        } else {
            flat = self.over(background);
            &flat
        };
        let pixels = source.tight_bytes();
        let color = match source.format {
            PixelFormat::Bgra8 => jpeg_encoder::ColorType::Bgra,
            PixelFormat::Rgba8 => jpeg_encoder::ColorType::Rgba,
        };
        let mut out = Vec::with_capacity(pixels.len() / 8);
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100));
        encoder.set_optimized_huffman_tables(true);
        // 144 dpi at 2×, as in the PNG (`encode_png`).
        let dpi = (source.scale.0.max(0.25) * 72.0).round() as u16;
        encoder.set_density(jpeg_encoder::Density::Inch { x: dpi, y: dpi });
        if let Some(profile) = &self.color.icc_profile {
            encoder.add_icc_profile(profile).map_err(|e| CaptureError::Encode(e.to_string()))?;
        }
        encoder.encode(&pixels, width as u16, height as u16, color).map_err(|e| CaptureError::Encode(e.to_string()))?;
        Ok(out)
    }

    fn encode_webp(&self) -> Result<Vec<u8>> {
        let (width, height) = (self.size.width, self.size.height);
        if width > WEBP_MAX_SIDE || height > WEBP_MAX_SIDE {
            return Err(too_large("WebP", WEBP_MAX_SIDE, width, height));
        }
        let rgba = self.to_rgba();
        let opaque = self.is_opaque();
        let (pixels, color) = if opaque {
            // Without alpha the encoder writes three channels and the file is smaller.
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            (rgb, image_webp::ColorType::Rgb8)
        } else {
            (rgba, image_webp::ColorType::Rgba8)
        };
        let mut out = Vec::with_capacity(pixels.len() / 3);
        let mut encoder = image_webp::WebPEncoder::new(&mut out);
        if let Some(profile) = &self.color.icc_profile {
            encoder.set_icc_profile(profile.to_vec());
        }
        encoder.encode(&pixels, width, height, color).map_err(|e| CaptureError::Encode(e.to_string()))?;
        Ok(out)
    }
}

fn too_large(format: &str, max: u32, width: u32, height: u32) -> CaptureError {
    CaptureError::InvalidArgument(format!(
        "a {width}×{height} image is too large for {format}, which allows at most {max} pixels per side"
    ))
}
