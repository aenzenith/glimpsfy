//! Reads an image file into a [`Frame`], for the history and the editor, which recognise files
//! rather than a live selection. On macOS ImageIO decodes every format the system knows (PNG,
//! JPEG, HEIC, WebP, TIFF); elsewhere pure-Rust decoders read the formats the app itself writes:
//! PNG, JPEG and WebP, told apart by their first bytes and never by the file's name.
//!
//! Text recognition reads a transparent image as if it lay on white ([`load_image`]). The
//! history's Combine lays pieces on a background of its own, so [`load_image_as`] keeps the alpha
//! and draws the image in the colour space of the combined image.

use std::path::Path;

use glimpsfy_capture::{ColorSpace, Frame};

use crate::{OcrError, Result};

/// Decodes the image at `path` into tight sRGB RGBA rows, laid on white. A missing file fails
/// with [`OcrError::Io`]; a file that is not an image fails with [`OcrError::InvalidImage`].
pub fn load_image(path: &Path) -> Result<Frame> {
    let bytes = std::fs::read(path)?;
    decode(&bytes)
}

/// Decodes the image at `path` into tight RGBA rows in `space`, with straight alpha. On macOS
/// ImageIO converts the file's own colours into `space`; elsewhere every capture is sRGB, so the
/// file is read as it is and only `ColorSpace::Srgb` may be asked for.
pub fn load_image_as(path: &Path, space: ColorSpace) -> Result<Frame> {
    let bytes = std::fs::read(path)?;
    decode_image_as(&bytes, space)
}

/// [`load_image_as`] for bytes already in memory.
pub fn decode_image_as(bytes: &[u8], space: ColorSpace) -> Result<Frame> {
    #[cfg(target_os = "macos")]
    {
        let mut frame = decode_in(bytes, space, false)?;
        unpremultiply(&mut frame.data);
        Ok(frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if space != ColorSpace::Srgb {
            return Err(OcrError::Engine(format!("images are read in {} on macOS only", space.css_name())));
        }
        decode_with(bytes, Alpha::Keep)
    }
}

#[cfg(target_os = "macos")]
fn decode(bytes: &[u8]) -> Result<Frame> {
    decode_in(bytes, ColorSpace::Srgb, true)
}

/// Draws the file's first image into a bitmap in `space`; `on_white` lays it on white first, else
/// the pixels come back premultiplied, as Core Graphics draws them.
#[cfg(target_os = "macos")]
fn decode_in(bytes: &[u8], target: ColorSpace, on_white: bool) -> Result<Frame> {
    use glimpsfy_capture::{ColorInfo, PhysicalSize, PixelFormat, ScaleFactor};
    use objc2_core_foundation::{CFData, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        kCGColorSpaceDisplayP3, kCGColorSpaceSRGB, CGBitmapContextCreate, CGColorSpace, CGContext, CGImage,
        CGImageAlphaInfo, CGImageByteOrderInfo,
    };
    use objc2_image_io::CGImageSource;

    let data = CFData::from_bytes(bytes);
    // SAFETY: the options dictionary may be null.
    let source = unsafe { CGImageSource::with_data(&data, None) }
        .ok_or_else(|| OcrError::InvalidImage("the file is not an image".into()))?;
    // SAFETY: index 0 always exists for a source that was created; null options are allowed.
    let image = unsafe { source.image_at_index(0, None) }
        .ok_or_else(|| OcrError::InvalidImage("the image could not be decoded".into()))?;
    let (width, height) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    if width == 0 || height == 0 || width > 32_768 || height > 32_768 {
        return Err(OcrError::InvalidImage(format!("an image of {width}×{height} pixels cannot be read")));
    }
    let row = width * 4;
    let mut pixels = vec![0u8; row * height];
    // SAFETY: Apple constants, valid for the life of the process.
    let name = unsafe {
        match target {
            ColorSpace::Srgb => kCGColorSpaceSRGB,
            ColorSpace::DisplayP3 => kCGColorSpaceDisplayP3,
        }
    };
    let space = CGColorSpace::with_name(Some(name))
        .ok_or_else(|| OcrError::Engine(format!("the {} colour space is missing", target.css_name())))?;
    let info = CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0;
    // SAFETY: `pixels` holds `height` rows of `row` bytes and outlives the context, which is
    // dropped before the buffer is used.
    let context =
        unsafe { CGBitmapContextCreate(pixels.as_mut_ptr().cast(), width, height, 8, row, Some(&space), info) }
            .ok_or_else(|| OcrError::Engine("Core Graphics could not make a bitmap".into()))?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64));
    if on_white {
        // A transparent image is read as if it lay on white, the way a viewer shows it.
        CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(Some(&context), rect);
    }
    CGContext::draw_image(Some(&context), rect, Some(&image));
    drop(context);
    let color = match target {
        ColorSpace::Srgb => ColorInfo::srgb(),
        ColorSpace::DisplayP3 => ColorInfo::for_space(ColorSpace::DisplayP3),
    };
    Ok(Frame::new(
        PhysicalSize::new(width as u32, height as u32),
        row,
        PixelFormat::Rgba8,
        color,
        ScaleFactor::ONE,
        pixels,
    )?)
}

/// Divides premultiplied RGBA back to straight alpha, which every `Frame` holds.
#[cfg(target_os = "macos")]
fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = (*c as u32 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
        }
    }
}

/// The longest side an image may have, the same limit as the macOS path.
#[cfg(not(target_os = "macos"))]
const MAX_SIDE: usize = 32_768;

/// The most pixels an image may have. A header is a few bytes and can claim any size, so the
/// size is checked before a buffer is made for it; this many pixels are one gibibyte of RGBA.
#[cfg(not(target_os = "macos"))]
const MAX_PIXELS: usize = 1 << 28;

/// What a decoder does with a pixel's alpha.
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy)]
enum Alpha {
    /// Lays the pixel on white, for text recognition.
    OnWhite,
    /// Keeps it straight, for the combine.
    Keep,
}

#[cfg(not(target_os = "macos"))]
impl Alpha {
    fn apply(self, r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
        match self {
            Alpha::OnWhite => on_white(r, g, b, a),
            Alpha::Keep => [r, g, b, a],
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn decode(bytes: &[u8]) -> Result<Frame> {
    decode_with(bytes, Alpha::OnWhite)
}

#[cfg(not(target_os = "macos"))]
fn decode_with(bytes: &[u8], alpha: Alpha) -> Result<Frame> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
    if bytes.starts_with(PNG) {
        decode_png(bytes, alpha)
    } else if bytes.starts_with(JPEG) {
        decode_jpeg(bytes)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        decode_webp(bytes, alpha)
    } else {
        Err(OcrError::InvalidImage("the file is not an image".into()))
    }
}

/// The size as `usize`s when an image of that size may be read.
#[cfg(not(target_os = "macos"))]
fn checked_size(width: u64, height: u64) -> Result<(usize, usize)> {
    let fits = |side: u64| side >= 1 && side <= MAX_SIDE as u64;
    if !fits(width) || !fits(height) || width * height > MAX_PIXELS as u64 {
        return Err(OcrError::InvalidImage(format!("an image of {width}×{height} pixels cannot be read")));
    }
    Ok((width as usize, height as usize))
}

/// Wraps tight RGBA rows of a decoded file into a frame.
#[cfg(not(target_os = "macos"))]
fn rgba_frame(width: usize, height: usize, rgba: Vec<u8>) -> Result<Frame> {
    use glimpsfy_capture::{ColorInfo, PhysicalSize, PixelFormat, ScaleFactor};

    Ok(Frame::new(
        PhysicalSize::new(width as u32, height as u32),
        width * 4,
        PixelFormat::Rgba8,
        ColorInfo::srgb(),
        ScaleFactor::ONE,
        rgba,
    )?)
}

/// Lays a pixel with straight alpha on white, the way a viewer shows a transparent image.
#[cfg(not(target_os = "macos"))]
fn on_white(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    if a == 255 {
        return [r, g, b, 255];
    }
    let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32) + 127) / 255) as u8;
    [over(r), over(g), over(b), 255]
}

#[cfg(not(target_os = "macos"))]
fn decode_png(bytes: &[u8], alpha: Alpha) -> Result<Frame> {
    let invalid = |e: png::DecodingError| OcrError::InvalidImage(e.to_string());
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(invalid)?;
    let (width, height) = checked_size(reader.info().width.into(), reader.info().height.into())?;
    let size =
        reader.output_buffer_size().ok_or_else(|| OcrError::InvalidImage("the image is too large to decode".into()))?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(invalid)?;
    let channels = info.color_type.samples();
    let decoded = buffer.get(..info.buffer_size()).unwrap_or(&buffer);
    if info.width as usize != width || info.height as usize != height || decoded.len() != width * height * channels {
        return Err(OcrError::InvalidImage("the decoded image does not have the size its header names".into()));
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for px in decoded.chunks_exact(channels) {
        let (r, g, b, a) = match channels {
            1 => (px[0], px[0], px[0], 255),
            2 => (px[0], px[0], px[0], px[1]),
            3 => (px[0], px[1], px[2], 255),
            _ => (px[0], px[1], px[2], px[3]),
        };
        rgba.extend_from_slice(&alpha.apply(r, g, b, a));
    }
    rgba_frame(width, height, rgba)
}

/// JPEG has no alpha. The decoder is asked for RGB and not RGBA, because RGB is the output it
/// converts every kind of JPEG to (grey, YCbCr, CMYK and YCCK alike).
#[cfg(not(target_os = "macos"))]
fn decode_jpeg(bytes: &[u8]) -> Result<Frame> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let invalid = |e: zune_jpeg::errors::DecodeErrors| OcrError::InvalidImage(e.to_string());
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(invalid)?;
    let info = decoder.info().ok_or_else(|| OcrError::InvalidImage("the JPEG header could not be read".into()))?;
    let (width, height) = checked_size(info.width.into(), info.height.into())?;
    // The rows a damaged file does not reach stay white, as an image viewer leaves them blank.
    let mut rgb = vec![255; width * height * 3];
    if decoder.output_buffer_size() != Some(rgb.len()) {
        return Err(OcrError::InvalidImage("the JPEG cannot be decoded to RGB".into()));
    }
    decoder.decode_into(&mut rgb).map_err(invalid)?;
    rgba_frame(width, height, opaque(&rgb))
}

/// Tight RGB rows as opaque RGBA.
#[cfg(not(target_os = "macos"))]
fn opaque(rgb: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    rgba
}

/// Lossy and lossless WebP; an animated file gives its first frame, as ImageIO does on macOS.
#[cfg(not(target_os = "macos"))]
fn decode_webp(bytes: &[u8], alpha: Alpha) -> Result<Frame> {
    let invalid = |e: image_webp::DecodingError| OcrError::InvalidImage(e.to_string());
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).map_err(invalid)?;
    let (width, height) = decoder.dimensions();
    let (width, height) = checked_size(width.into(), height.into())?;
    if decoder.has_alpha() {
        let mut rgba = vec![0; width * height * 4];
        decoder.read_image(&mut rgba).map_err(invalid)?;
        for px in rgba.chunks_exact_mut(4) {
            let out = alpha.apply(px[0], px[1], px[2], px[3]);
            px.copy_from_slice(&out);
        }
        rgba_frame(width, height, rgba)
    } else {
        let mut rgb = vec![0; width * height * 3];
        decoder.read_image(&mut rgb).map_err(invalid)?;
        rgba_frame(width, height, opaque(&rgb))
    }
}
