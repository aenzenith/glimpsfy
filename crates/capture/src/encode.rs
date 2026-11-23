//! Image files from frames: PNG, JPEG and WebP, each written with pure-Rust encoders, and the
//! second pass that makes a saved PNG smaller ([`optimise_png`]: oxipng, whose libdeflate is C).
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
//!
//! **Smaller PNGs.** A file written once and kept (`PngEffort::Optimised`) goes through oxipng
//! after the balanced encoder: its filters are chosen again and the data is deflated by
//! libdeflate. Nothing that shows is touched: the pixels, `iCCP` (recompressed, the same
//! profile), `sRGB`, `pHYs` and `cICP` stay.

use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::error::{CaptureError, Result};
use crate::frame::{Frame, PixelFormat, PngEffort};

/// What a smaller PNG may take in all, from the balanced encoder to the end of the second pass.
/// oxipng's last deflate pass cannot be stopped once it runs, so [`optimise_png`] picks a level
/// that fits and stops waiting when the time is up.
pub const PNG_OPTIMISE_BUDGET: Duration = Duration::from_millis(2500);

/// The libdeflate levels of the second pass, best first, each with how many times the balanced
/// encoder's own time it may take. The balanced encoder deflates the same pixels, so its time says
/// how slow this image is on this machine at this moment: on screenshots from 2 to 15 megapixels
/// on an M4 Pro, level 10 took at most 3.8 times as long, filter trials included, and level 8 at
/// most 1.5 times.
const PNG_LEVELS: [(u8, f64); 2] = [(10, 4.0), (8, 1.6)];

/// The balanced encoder's time per byte of a fast PNG of the same pixels, at its slowest on the
/// same screenshots (97 ns): the estimate for a PNG that was not just encoded, such as a pin's.
const PNG_ENCODE_NS_PER_BYTE: u64 = 100;

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

/// `png` compressed again by oxipng, with the same pixels and colour chunks in fewer bytes, within
/// [`PNG_OPTIMISE_BUDGET`]. `encoded_in` is how long the balanced encoder took to make `png`, which
/// the budget already spent; without it (a PNG made earlier) the time is estimated from the size.
/// It answers `png` itself when no level fits the time left, when oxipng fails, finds nothing
/// smaller or is still at work when the time is up, so the caller always has a file to write.
pub fn optimise_png(png: Vec<u8>, encoded_in: Option<Duration>) -> Vec<u8> {
    let probe = encoded_in.unwrap_or_else(|| Duration::from_nanos(png.len() as u64 * PNG_ENCODE_NS_PER_BYTE));
    let left = PNG_OPTIMISE_BUDGET.saturating_sub(encoded_in.unwrap_or_default());
    // A small capture, where most of the gain is, gets level 10; a 5K screen of windows gets 10
    // or 8; one full of photographs, which loses only a few percent anyway, keeps its PNG.
    let Some(level) = PNG_LEVELS.iter().find(|(_, times)| probe.mul_f64(*times) <= left).map(|(level, _)| *level)
    else {
        return png;
    };
    // Preset 1 weighs the None and Bigrams filters, the two that win on screenshots; preset 2's
    // four filters and level 11 took a 5K screen of windows to 2.6 s for one percent more.
    let mut options = oxipng::Options::from_preset(1);
    options.deflater = oxipng::Deflater::Libdeflater { compression: level };
    // No new trial starts in the second half, which the chosen filter's last pass needs.
    options.timeout = Some(left / 2);
    // The colour chunks say how the pixels look and pHYs how large they are, so none is stripped.
    options.strip = oxipng::StripChunks::None;
    // A grey PNG cannot carry an RGB profile, so the reduction would drop iCCP and sRGB. oxipng 10
    // already refrains when either is present; this keeps it so under a later default.
    options.grayscale_reduction = false;
    // Transparent pixels keep their colour values, which a later edit with alpha may show again.
    options.optimize_alpha = false;

    // The pass runs on a thread of its own, so the save stops waiting when the time is up; a pass
    // that is late finishes there and its result is dropped.
    let png = Arc::new(png);
    let (done, result) = mpsc::channel();
    let input = Arc::clone(&png);
    let started = std::thread::Builder::new()
        .name("glimpsfy-png".into())
        .spawn(move || drop(done.send(oxipng::optimize_from_memory(&input, &options))));
    let smaller = match started {
        Ok(_) => result.recv_timeout(left).ok().and_then(Result::ok),
        Err(_) => None,
    };
    match smaller {
        Some(smaller) if smaller.len() < png.len() => smaller,
        _ => Arc::unwrap_or_clone(png),
    }
}

fn too_large(format: &str, max: u32, width: u32, height: u32) -> CaptureError {
    CaptureError::InvalidArgument(format!(
        "a {width}×{height} image is too large for {format}, which allows at most {max} pixels per side"
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::frame::{ColorInfo, ColorSpace};
    use crate::geometry::{PhysicalSize, ScaleFactor};

    /// Stands in for a Display P3 profile: the check is that these bytes come back, not what
    /// they describe.
    const PROFILE: &[u8] = b"glimpsfy test profile, Display P3, kept byte for byte by every PNG pass";

    /// A BGRA frame at 2× whose pixel at (x, y) is `rgba(x, y)`.
    fn frame(color: ColorInfo, rgba: impl Fn(u32, u32) -> [u8; 4]) -> Frame {
        let (width, height) = (96, 64);
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b, a] = rgba(x, y);
                data.extend_from_slice(&[b, g, r, a]);
            }
        }
        Frame::new(
            PhysicalSize::new(width, height),
            width as usize * 4,
            PixelFormat::Bgra8,
            color,
            ScaleFactor(2.0),
            data,
        )
        .unwrap()
    }

    /// A window's look: a title bar, text-like marks, a coloured button and a gradient.
    fn window(x: u32, y: u32) -> [u8; 4] {
        match (x, y) {
            (_, 0..=9) => [236, 236, 236, 255],
            (70..=85, 40..=50) => [20, 120, 230, 255],
            _ if (x / 3 + y / 5).is_multiple_of(7) => [30, 30, 30, 255],
            _ => [255, (200 + x / 2) as u8, 250, 255],
        }
    }

    /// What a PNG shows: its pixels as RGBA, its profile, its sRGB chunk and its resolution
    /// (pixels per unit across and down, and whether the unit is the metre).
    struct Decoded {
        rgba: Vec<u8>,
        profile: Option<Vec<u8>>,
        srgb: Option<png::SrgbRenderingIntent>,
        dims: Option<(u32, u32, bool)>,
    }

    fn decode(png: &[u8]) -> Decoded {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut buffer).unwrap();
        let rgba = buffer[..frame.buffer_size()]
            .chunks_exact(frame.color_type.samples())
            .flat_map(|p| match *p {
                [g] => [g, g, g, 255],
                [g, a] => [g, g, g, a],
                [r, g, b] => [r, g, b, 255],
                [r, g, b, a] => [r, g, b, a],
                _ => unreachable!(),
            })
            .collect();
        let info = reader.info();
        let dims = info.pixel_dims.map(|d| (d.xppu, d.yppu, d.unit == png::Unit::Meter));
        Decoded { rgba, profile: info.icc_profile.as_ref().map(|p| p.to_vec()), srgb: info.srgb, dims }
    }

    /// A saved PNG is written once and never again, so a pixel, a colour profile or a resolution
    /// that the second pass dropped could not be got back: a Display P3 capture would show
    /// washed out and a Retina one at twice its size, without a word. Each image must also come
    /// out smaller, or the check would only have seen oxipng hand its input back.
    #[test]
    fn optimised_png_keeps_pixels_and_colour() {
        let p3 = ColorInfo { space: ColorSpace::DisplayP3, icc_profile: Some(Arc::from(PROFILE)) };
        #[allow(unused_mut)]
        let mut cases = vec![
            ("a Display P3 screen", frame(p3.clone(), window)),
            ("an sRGB screen without a profile", frame(ColorInfo::srgb(), window)),
            // oxipng would make an all-grey image a grey PNG, which can carry no RGB profile.
            (
                "an all-grey Display P3 screen",
                frame(p3.clone(), |x, y| {
                    let [v, ..] = window(x, y);
                    let v = v.saturating_sub(((x * y) % 23) as u8);
                    [v, v, v, 255]
                }),
            ),
            // A window's shadow fades out, and the colour of its clear pixels stays as captured.
            (
                "a window with its shadow",
                frame(p3, |x, y| {
                    let edge = x.min(y).min(95 - x).min(63 - y);
                    match edge {
                        0 => [10, 20, 30, 0],
                        1..=7 => [0, 0, 0, (edge * 30) as u8],
                        _ => window(x, y),
                    }
                }),
            ),
        ];
        #[cfg(target_os = "macos")]
        cases.push(("the system's Display P3 profile", frame(ColorInfo::for_space(ColorSpace::DisplayP3), window)));

        // A flat image becomes a palette PNG, whose iCCP some tools (`sips`) do not report.
        let mut palettes = 0;
        for (name, frame) in cases {
            let balanced = frame.encode(ImageEncoding::Png(PngEffort::Balanced)).unwrap();
            let optimised = frame.encode(ImageEncoding::Png(PngEffort::Optimised)).unwrap();
            // The colour type, in the IHDR right after the signature.
            palettes += usize::from(optimised[25] == 3);
            assert!(
                optimised.len() < balanced.len(),
                "{name}: {} bytes, not fewer than {}",
                optimised.len(),
                balanced.len()
            );
            let file = decode(&optimised);
            assert!(file.rgba == frame.to_rgba(), "{name}: the pixels changed");
            assert_eq!(
                file.profile,
                frame.color.icc_profile.as_deref().map(<[u8]>::to_vec),
                "{name}: the profile changed"
            );
            let srgb = frame.color.icc_profile.is_none().then_some(png::SrgbRenderingIntent::Perceptual);
            assert_eq!(file.srgb, srgb, "{name}: the sRGB chunk changed");
            let per_metre = (144.0 / 0.0254_f64).round() as u32;
            assert_eq!(file.dims, Some((per_metre, per_metre, true)), "{name}: the resolution changed");
        }
        assert!(palettes > 0, "no image became a palette PNG, so none checked that one keeps its profile");
    }
}
