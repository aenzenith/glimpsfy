//! Reads an image file into a [`Frame`], for the history and the editor, which recognise files
//! rather than a live selection. On macOS ImageIO decodes every format the system knows (PNG,
//! JPEG, HEIC, WebP, TIFF); elsewhere only PNG is read until the other platforms need more.

use std::path::Path;

use glimpsfy_capture::Frame;

use crate::{OcrError, Result};

/// Decodes the image at `path` into tight RGBA rows. A missing file fails with
/// [`OcrError::Io`]; a file that is not an image fails with [`OcrError::InvalidImage`].
pub fn load_image(path: &Path) -> Result<Frame> {
    let bytes = std::fs::read(path)?;
    decode(&bytes)
}

#[cfg(target_os = "macos")]
fn decode(bytes: &[u8]) -> Result<Frame> {
    use glimpsfy_capture::{ColorInfo, PhysicalSize, PixelFormat, ScaleFactor};
    use objc2_core_foundation::{CFData, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        kCGColorSpaceSRGB, CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
        CGImageByteOrderInfo,
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
    // SAFETY: an Apple constant, valid for the life of the process.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| OcrError::Engine("the sRGB colour space is missing".into()))?;
    let info = CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0;
    // SAFETY: `pixels` holds `height` rows of `row` bytes and outlives the context, which is
    // dropped before the buffer is used.
    let context =
        unsafe { CGBitmapContextCreate(pixels.as_mut_ptr().cast(), width, height, 8, row, Some(&space), info) }
            .ok_or_else(|| OcrError::Engine("Core Graphics could not make a bitmap".into()))?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64));
    // A transparent image is read as if it lay on white, the way a viewer shows it.
    CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
    CGContext::fill_rect(Some(&context), rect);
    CGContext::draw_image(Some(&context), rect, Some(&image));
    drop(context);
    Ok(Frame::new(
        PhysicalSize::new(width as u32, height as u32),
        row,
        PixelFormat::Rgba8,
        ColorInfo::srgb(),
        ScaleFactor::ONE,
        pixels,
    )?)
}

#[cfg(not(target_os = "macos"))]
fn decode(bytes: &[u8]) -> Result<Frame> {
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
        // Transparent pixels are read as if they lay on white.
        let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
        rgba.extend_from_slice(&[over(r), over(g), over(b), 255]);
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
