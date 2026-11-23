//! Reads an image file into a [`Frame`], for the history and the editor, which recognise files
//! rather than a live selection. On macOS ImageIO decodes every format the system knows (PNG,
//! JPEG, HEIC, WebP, TIFF); elsewhere only PNG is read until the other platforms need more.
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
            Alpha::OnWhite => {
                // Transparent pixels are read as if they lay on white.
                let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
                [over(r), over(g), over(b), 255]
            }
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
    use glimpsfy_capture::{ColorInfo, PhysicalSize, PixelFormat, ScaleFactor};

    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| OcrError::InvalidImage(e.to_string()))?;
    let mut buffer = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buffer).map_err(|e| OcrError::InvalidImage(e.to_string()))?;
    let (width, height) = (info.width as usize, info.height as usize);
    let mut rgba = Vec::with_capacity(width * height * 4);
    let channels = info.color_type.samples();
    for px in buffer[..info.buffer_size()].chunks_exact(channels) {
        let (r, g, b, a) = match channels {
            1 => (px[0], px[0], px[0], 255),
            2 => (px[0], px[0], px[0], px[1]),
            3 => (px[0], px[1], px[2], 255),
            _ => (px[0], px[1], px[2], px[3]),
        };
        rgba.extend_from_slice(&alpha.apply(r, g, b, a));
    }
    Ok(Frame::new(
        PhysicalSize::new(width as u32, height as u32),
        width * 4,
        PixelFormat::Rgba8,
        ColorInfo::srgb(),
        ScaleFactor::ONE,
        rgba,
    )?)
}
