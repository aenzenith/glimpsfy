//! Beautify (B11): the padding, background, rounded corner, shadow and window frame put around an
//! image as it leaves the app. `output::finish_pixels` calls [`apply`] after the scaling to 1×, so
//! a style's points become the delivered image's pixels (48 pt is 96 px at 2×).
//!
//! - **The image's pixels stay.** The capture (and a Match colour taken from it) is copied as it
//!   is; only the style's own colours are converted, from sRGB into the image's colour space, so
//!   Ocean looks the same in a Display P3 capture and nothing of the capture is clipped.
//! - **Windows keep their own look.** A window capture already has its corner and shadow (Settings
//!   › Output), so it gets only the padding and the background.
//! - **The shadow** is a rounded rectangle blurred by three box passes, which approximate a
//!   Gaussian whose deviation is half the shadow's blur, as a canvas and CSS draw it. It is worked
//!   out on a grid a few pixels wide, since a blur that soft loses nothing there, and it is drawn
//!   only outside the image, as a CSS `box-shadow` is.
//! - **The gradient** runs at 135°, CSS's way: the stops lie on the line from the top-left corner
//!   to the bottom-right one.
//! - **An image** covers the whole output and is read anew for each delivery; when it cannot be
//!   read (moved, deleted, not an image), the style's fallback gradient is used and the delivery
//!   goes on.

use std::path::Path;

use glimpsfy_annotate::beautify::FRAME_BAR;
use glimpsfy_annotate::{Background, BeautifyStyle, WindowFrame};
use glimpsfy_capture::{ColorInfo, ColorSpace, Frame, PhysicalRect, PhysicalSize, PixelFormat};

use crate::trace;

/// The longest side an image background is read at; a larger one is scaled down first.
const IMAGE_MAX_SIDE: u32 = 6144;

/// The colours of the window frame and its buttons, sRGB.
const BAR_LIGHT: [u8; 3] = [0xec, 0xec, 0xf0];
const BAR_DARK: [u8; 3] = [0x2b, 0x2c, 0x35];
const BUTTONS: [[u8; 3]; 3] = [[0xff, 0x5f, 0x57], [0xfe, 0xbc, 0x2e], [0x28, 0xc8, 0x40]];

/// Puts `style` around `frame`, a window's own image when `window` is set. The result has the
/// frame's colour space, scale and straight alpha, as RGBA rows.
pub fn apply(frame: &Frame, style: &BeautifyStyle, window: bool) -> Frame {
    let style = style.clamped();
    let s = if frame.scale.0.is_finite() && frame.scale.0 > 0.0 { frame.scale.0 } else { 1.0 };
    let space = frame.color.space;
    let (w, h) = (frame.size.width as usize, frame.size.height as usize);
    let pad = (style.padding * s).round() as usize;
    let bar = if !window && style.frame != WindowFrame::None { (FRAME_BAR * s).round() as usize } else { 0 };
    let radius = if window { 0.0 } else { (style.corner * s).min(w.min(h + bar) as f64 / 2.0) };
    let (out_w, out_h) = (w + 2 * pad, h + bar + 2 * pad);

    let layout = Layout {
        content: Rounded {
            x0: pad as f64,
            y0: pad as f64,
            x1: (pad + w) as f64,
            y1: (pad + bar + h) as f64,
            r: radius,
        },
        image_x: pad,
        image_y: pad + bar,
    };
    let background = Fill::new(&style.background, frame, space, out_w, out_h);
    let geometry = if window { None } else { style.shadow.geometry() };
    let shadow = geometry
        .map(|(blur, dy, opacity)| ShadowMask::new(&layout.content, blur / 2.0 * s, dy * s, opacity, out_w, out_h));
    let chrome = (bar > 0).then(|| Chrome::new(style.frame, space, s, &layout));

    let mut out = vec![0u8; out_w * out_h * 4];
    let opaque = frame.is_opaque();
    for y in 0..out_h {
        let row = &mut out[y * out_w * 4..(y + 1) * out_w * 4];
        // A row through the middle of an opaque image needs no compositing there: the image's
        // own pixels are copied, and only the ring around it is worked out pixel by pixel.
        let iy = y as i64 - layout.image_y as i64;
        let straight = opaque
            && iy >= 0
            && (iy as usize) < h
            && (y as f64 + 0.5) >= layout.content.y0 + radius
            && (y as f64 + 0.5) <= layout.content.y1 - radius;
        let (copy_from, copy_to) = if straight { (layout.image_x, layout.image_x + w) } else { (0, 0) };
        for x in 0..out_w {
            if x >= copy_from && x < copy_to {
                continue;
            }
            let px = pixel(x, y, frame, &layout, &background, shadow.as_ref(), chrome.as_ref());
            row[x * 4..x * 4 + 4].copy_from_slice(&px);
        }
        if straight {
            copy_row(frame, iy as usize, &mut row[copy_from * 4..copy_to * 4]);
        }
    }
    Frame {
        size: PhysicalSize::new(out_w as u32, out_h as u32),
        bytes_per_row: out_w * 4,
        format: PixelFormat::Rgba8,
        color: frame.color.clone(),
        scale: frame.scale,
        data: out,
    }
}

/// Copies row `y` of the frame as RGBA.
fn copy_row(frame: &Frame, y: usize, dst: &mut [u8]) {
    let start = y * frame.bytes_per_row;
    let src = &frame.data[start..start + frame.size.width as usize * 4];
    match frame.format {
        PixelFormat::Rgba8 => dst.copy_from_slice(src),
        PixelFormat::Bgra8 => glimpsfy_capture::frame::swizzle_bgra_rgba(src, dst),
    }
}

// ----------------------------------------------------------------------------------- geometry

/// Where everything goes in the output, in its pixels.
struct Layout {
    /// The frame and the image together, with their corner.
    content: Rounded,
    image_x: usize,
    image_y: usize,
}

/// A rectangle with rounded corners.
#[derive(Clone, Copy)]
struct Rounded {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    r: f64,
}

impl Rounded {
    /// The signed distance from the outline: negative inside.
    fn distance(&self, x: f64, y: f64) -> f64 {
        let (cx, cy) = ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0);
        let (hw, hh) = ((self.x1 - self.x0) / 2.0, (self.y1 - self.y0) / 2.0);
        let qx = (x - cx).abs() - (hw - self.r);
        let qy = (y - cy).abs() - (hh - self.r);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        outside + qx.max(qy).min(0.0) - self.r
    }

    /// How much of the pixel whose centre is (`x`, `y`) lies inside, from 0 to 1.
    fn coverage(&self, x: f64, y: f64) -> f64 {
        (0.5 - self.distance(x, y)).clamp(0.0, 1.0)
    }
}

// ------------------------------------------------------------------------------------- colour

/// An 8-bit sRGB colour in `space`, as 0–1 values.
fn convert(rgb: [u8; 3], space: ColorSpace) -> [f32; 3] {
    let encoded = rgb.map(|c| c as f64 / 255.0);
    match space {
        ColorSpace::Srgb => encoded.map(|c| c as f32),
        ColorSpace::DisplayP3 => {
            // Both spaces share the sRGB transfer curve; only the primaries differ.
            let [r, g, b] = encoded.map(linear);
            let p3 = [
                0.822_462 * r + 0.177_538 * g,
                0.033_194 * r + 0.966_806 * g,
                0.017_083 * r + 0.072_397 * g + 0.910_520 * b,
            ];
            p3.map(|c| encode(c.clamp(0.0, 1.0)) as f32)
        }
    }
}

fn linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn rgb_of(hex: &glimpsfy_annotate::HexColor) -> [u8; 3] {
    let digits = hex.as_str().trim_start_matches('#');
    let at = |i: usize| u8::from_str_radix(digits.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    [at(0), at(2), at(4)]
}

/// A colour with straight alpha, 0–1, composited over `dst`.
fn over(src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let a = src[3] + dst[3] * (1.0 - src[3]);
    if a <= 0.0 {
        return [0.0; 4];
    }
    let mix = |i: usize| (src[i] * src[3] + dst[i] * dst[3] * (1.0 - src[3])) / a;
    [mix(0), mix(1), mix(2), a]
}

// ------------------------------------------------------------------------------- background

enum Fill {
    Clear,
    Solid([f32; 3]),
    /// The stops, and the sum of the output's sides that a 135° gradient spans.
    Gradient(Vec<[f32; 3]>, f64),
    /// The image, covering the output, as RGBA rows.
    Image(Frame),
}

impl Fill {
    fn new(background: &Background, frame: &Frame, space: ColorSpace, w: usize, h: usize) -> Fill {
        let gradient = |stops: &[glimpsfy_annotate::HexColor]| match stops {
            [] => Fill::Clear,
            [one] => Fill::Solid(convert(rgb_of(one), space)),
            many => Fill::Gradient(many.iter().map(|c| convert(rgb_of(c), space)).collect(), (w + h) as f64),
        };
        match background {
            Background::Transparent => Fill::Clear,
            Background::Solid { color } => Fill::Solid(convert(rgb_of(color), space)),
            Background::Gradient { stops } => gradient(stops),
            Background::Image { path, fallback } => match load_image(Path::new(path), space) {
                Some(image) => Fill::Image(cover(&image, w, h)),
                None => {
                    trace::log(format!(
                        "[glimpsfy] beautify: the image {path} could not be read; its gradient is used"
                    ));
                    gradient(fallback)
                }
            },
            Background::Match => Fill::Solid(edge_median(frame).map(|c| c as f32 / 255.0)),
        }
    }

    fn at(&self, x: usize, y: usize) -> [f32; 4] {
        match self {
            Fill::Clear => [0.0; 4],
            Fill::Solid([r, g, b]) => [*r, *g, *b, 1.0],
            Fill::Gradient(stops, span) => {
                let t = ((x as f64 + 0.5 + y as f64 + 0.5) / span).clamp(0.0, 1.0) * (stops.len() - 1) as f64;
                let i = (t.floor() as usize).min(stops.len() - 2);
                let f = (t - i as f64) as f32;
                let (a, b) = (stops[i], stops[i + 1]);
                [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f, 1.0]
            }
            Fill::Image(image) => image.rgba_at(x as u32, y as u32).map_or([0.0; 4], |p| p.map(|c| c as f32 / 255.0)),
        }
    }
}

/// The median of each channel over the image's outermost 2-pixel band, sRGB bytes in the
/// frame's own space. A window's band is its transparent shadow, so only opaque pixels count,
/// and an image without any there takes its opaque pixels from everywhere.
pub fn edge_median(frame: &Frame) -> [u8; 3] {
    let (w, h) = (frame.size.width, frame.size.height);
    let mut tally = Tally::default();
    for y in 0..h {
        if y < 2 || y + 2 >= h {
            (0..w).for_each(|x| tally.add(frame.rgba_at(x, y)));
        } else {
            for x in [0, 1, w.saturating_sub(2), w.saturating_sub(1)] {
                tally.add(frame.rgba_at(x, y));
            }
        }
    }
    if tally.total == 0 {
        for y in (0..h).step_by(4) {
            (0..w).step_by(4).for_each(|x| tally.add(frame.rgba_at(x, y)));
        }
    }
    tally.median()
}

/// How often each value of each channel was seen among opaque pixels.
struct Tally {
    counts: [[u32; 256]; 3],
    total: u32,
}

impl Default for Tally {
    fn default() -> Tally {
        Tally { counts: [[0; 256]; 3], total: 0 }
    }
}

impl Tally {
    fn add(&mut self, pixel: Option<[u8; 4]>) {
        let Some(p) = pixel.filter(|p| p[3] == 255) else { return };
        for (channel, value) in self.counts.iter_mut().zip(p) {
            channel[value as usize] += 1;
        }
        self.total += 1;
    }

    fn median(&self) -> [u8; 3] {
        if self.total == 0 {
            return [255, 255, 255];
        }
        self.counts.map(|channel| {
            let mut seen = 0u32;
            channel
                .iter()
                .position(|n| {
                    seen += n;
                    seen * 2 >= self.total
                })
                .unwrap_or(0) as u8
        })
    }
}

/// `image` scaled to cover `w` × `h` and cut to it, centred.
fn cover(image: &Frame, w: usize, h: usize) -> Frame {
    let (iw, ih) = (image.size.width.max(1) as f64, image.size.height.max(1) as f64);
    let k = (w as f64 / iw).max(h as f64 / ih);
    let size = PhysicalSize::new(((iw * k).ceil() as u32).max(w as u32), ((ih * k).ceil() as u32).max(h as u32));
    let scaled = if size == image.size { image.clone() } else { image.resized(size) };
    let x = (size.width - w as u32) / 2;
    let y = (size.height - h as u32) / 2;
    let rect = PhysicalRect { x: x as i32, y: y as i32, width: w as u32, height: h as u32 };
    let cut = scaled.crop(rect).unwrap_or(scaled);
    let row = cut.size.width as usize * 4;
    Frame { data: cut.to_rgba(), bytes_per_row: row, format: PixelFormat::Rgba8, ..cut }
}

/// Reads an image file into `space`, at most `IMAGE_MAX_SIDE` on its longer side, or `None`
/// when the file is gone or no image.
pub fn load_image(path: &Path, space: ColorSpace) -> Option<Frame> {
    if !path.is_file() {
        return None;
    }
    platform_image(path, space).or_else(|| decoded_image(path, space))
}

/// PNG and JPEG without the system's help; their pixels are taken as sRGB.
fn decoded_image(path: &Path, space: ColorSpace) -> Option<Frame> {
    let bytes = std::fs::read(path).ok()?;
    let (w, h, rgba) = if bytes.starts_with(b"\x89PNG") {
        decode_png(&bytes)?
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        decode_jpeg(&bytes)?
    } else {
        return None;
    };
    let mut frame = Frame::new(
        PhysicalSize::new(w, h),
        w as usize * 4,
        PixelFormat::Rgba8,
        ColorInfo::for_space(space),
        glimpsfy_capture::ScaleFactor::ONE,
        rgba,
    )
    .ok()?;
    if space == ColorSpace::DisplayP3 {
        for p in frame.data.chunks_exact_mut(4) {
            let c = convert([p[0], p[1], p[2]], space);
            for (byte, value) in p.iter_mut().zip(c) {
                *byte = (value * 255.0).round() as u8;
            }
        }
    }
    Some(shrunk(frame))
}

fn shrunk(frame: Frame) -> Frame {
    let longest = frame.size.width.max(frame.size.height);
    if longest <= IMAGE_MAX_SIDE {
        return frame;
    }
    let k = IMAGE_MAX_SIDE as f64 / longest as f64;
    let side = |v: u32| ((v as f64 * k).round() as u32).max(1);
    frame.resized(PhysicalSize::new(side(frame.size.width), side(frame.size.height)))
}

fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    let (w, h) = (info.width, info.height);
    let pixels = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::GrayscaleAlpha => pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        _ => return None,
    };
    Some((w, h, rgba))
}

#[cfg(not(target_os = "macos"))]
fn decode_jpeg(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace as JpegSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    // Asked for RGB, the decoder converts every kind of JPEG (grey, YCbCr, CMYK), as the
    // clipboard of Windows and the OCR crate do.
    let options = DecoderOptions::default().jpeg_set_out_colorspace(JpegSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().ok()?;
    let info = decoder.info()?;
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    if w == 0 || h == 0 || decoder.output_buffer_size() != Some(w * h * 3) {
        return None;
    }
    let mut rgb = vec![0u8; w * h * 3];
    decoder.decode_into(&mut rgb).ok()?;
    let rgba = rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    Some((w as u32, h as u32, rgba))
}

/// macOS reads JPEG (and every other format) through the system.
#[cfg(target_os = "macos")]
fn decode_jpeg(_bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// Every format macOS reads (HEIC, JPEG, PNG, TIFF…), drawn by Core Graphics into `space`, so the
/// file's own profile is honoured.
#[cfg(target_os = "macos")]
fn platform_image(path: &Path, space: ColorSpace) -> Option<Frame> {
    use objc2::AnyThread;
    use objc2_app_kit::NSImage;
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        kCGColorSpaceDisplayP3, kCGColorSpaceSRGB, CGBitmapContextCreate, CGColorSpace, CGContext, CGImage,
        CGImageAlphaInfo,
    };
    use objc2_foundation::NSString;

    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&path.to_string_lossy()))?;
    // SAFETY: a null rectangle asks for the image's own size; no context and no hints are given.
    let cg = unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let (iw, ih) = (CGImage::width(Some(&cg)), CGImage::height(Some(&cg)));
    if iw == 0 || ih == 0 {
        return None;
    }
    let longest = iw.max(ih) as f64;
    let k = (IMAGE_MAX_SIDE as f64 / longest).min(1.0);
    let (w, h) = (((iw as f64 * k).round() as usize).max(1), ((ih as f64 * k).round() as usize).max(1));
    // SAFETY: the names are Core Graphics' own constants.
    let name = unsafe {
        match space {
            ColorSpace::DisplayP3 => kCGColorSpaceDisplayP3,
            ColorSpace::Srgb => kCGColorSpaceSRGB,
        }
    };
    let target = CGColorSpace::with_name(Some(name))?;
    let mut data = vec![0u8; w * h * 4];
    // SAFETY: the buffer holds exactly `h` rows of `w * 4` bytes and outlives the context, which
    // is dropped at the end of this block.
    let context = unsafe {
        CGBitmapContextCreate(
            data.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            Some(&target),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
    CGContext::draw_image(Some(&context), rect, Some(&cg));
    drop(context);
    // The context keeps premultiplied alpha; frames carry it straight.
    for p in data.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a != 255 && a != 0 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Frame::new(
        PhysicalSize::new(w as u32, h as u32),
        w * 4,
        PixelFormat::Rgba8,
        ColorInfo::for_space(space),
        glimpsfy_capture::ScaleFactor::ONE,
        data,
    )
    .ok()
}

#[cfg(not(target_os = "macos"))]
fn platform_image(_path: &Path, _space: ColorSpace) -> Option<Frame> {
    None
}

// ------------------------------------------------------------------------------------- shadow

/// The shadow's alpha on a coarse grid of the output, blurred.
struct ShadowMask {
    /// Output pixels per grid cell.
    cell: f64,
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl ShadowMask {
    fn new(content: &Rounded, sigma: f64, dy: f64, opacity: f64, out_w: usize, out_h: usize) -> ShadowMask {
        let cell = (sigma / 2.0).floor().clamp(1.0, 16.0);
        let (w, h) = ((out_w as f64 / cell).ceil() as usize + 1, (out_h as f64 / cell).ceil() as usize + 1);
        let shape = Rounded { y0: content.y0 + dy, y1: content.y1 + dy, ..*content };
        let mut data = vec![0f32; w * h];
        for gy in 0..h {
            for gx in 0..w {
                let (x, y) = ((gx as f64 + 0.5) * cell, (gy as f64 + 0.5) * cell);
                data[gy * w + gx] = ((0.5 - shape.distance(x, y) / cell).clamp(0.0, 1.0) * opacity) as f32;
            }
        }
        for size in boxes(sigma / cell) {
            blur_rows(&mut data, w, h, size / 2);
            transpose_blur(&mut data, w, h, size / 2);
        }
        ShadowMask { cell, w, h, data }
    }

    /// The shadow's alpha at the centre of output pixel (`x`, `y`), by bilinear interpolation.
    fn at(&self, x: usize, y: usize) -> f64 {
        let u = ((x as f64 + 0.5) / self.cell - 0.5).max(0.0);
        let v = ((y as f64 + 0.5) / self.cell - 0.5).max(0.0);
        let (x0, y0) = ((u.floor() as usize).min(self.w - 1), (v.floor() as usize).min(self.h - 1));
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = ((u - x0 as f64) as f32, (v - y0 as f64) as f32);
        let get = |x: usize, y: usize| self.data[y * self.w + x];
        let top = get(x0, y0) + (get(x1, y0) - get(x0, y0)) * fx;
        let bottom = get(x0, y1) + (get(x1, y1) - get(x0, y1)) * fx;
        (top + (bottom - top) * fy) as f64
    }
}

/// The widths of three box blurs that together approximate a Gaussian of `sigma`.
fn boxes(sigma: f64) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut low = ideal.floor() as i64;
    if low % 2 == 0 {
        low -= 1;
    }
    let low = low.max(1);
    let lf = low as f64;
    let m = ((12.0 * sigma * sigma - n * lf * lf - 4.0 * n * lf - 3.0 * n) / (-4.0 * lf - 4.0)).round() as i64;
    std::array::from_fn(|i| if (i as i64) < m { low as usize } else { low as usize + 2 })
}

/// A box blur of radius `r` along every row; outside the grid counts as no shadow.
fn blur_rows(data: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let width = (2 * r + 1) as f32;
    let mut line = vec![0f32; w];
    for y in 0..h {
        let row = &mut data[y * w..(y + 1) * w];
        let mut sum: f32 = row.iter().take(r + 1).sum();
        for (x, out) in line.iter_mut().enumerate() {
            *out = sum / width;
            if x + r + 1 < w {
                sum += row[x + r + 1];
            }
            if x >= r {
                sum -= row[x - r];
            }
        }
        row.copy_from_slice(&line);
    }
}

/// The same blur along every column.
fn transpose_blur(data: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut t = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            t[x * h + y] = data[y * w + x];
        }
    }
    blur_rows(&mut t, h, w, r);
    for y in 0..h {
        for x in 0..w {
            data[y * w + x] = t[x * h + y];
        }
    }
}

// ------------------------------------------------------------------------------- window frame

/// The title bar of the window frame.
struct Chrome {
    bar: [f32; 3],
    buttons: [([f64; 2], [f32; 3]); 3],
    button_r: f64,
}

impl Chrome {
    fn new(frame: WindowFrame, space: ColorSpace, s: f64, layout: &Layout) -> Chrome {
        let bar = convert(if frame == WindowFrame::Dark { BAR_DARK } else { BAR_LIGHT }, space);
        let (x0, y0) = (layout.content.x0, layout.content.y0);
        let buttons =
            std::array::from_fn(|i| ([x0 + (16.0 + 17.0 * i as f64) * s, y0 + 14.0 * s], convert(BUTTONS[i], space)));
        Chrome { bar, buttons, button_r: 5.5 * s }
    }

    fn at(&self, x: f64, y: f64) -> [f32; 4] {
        let mut px = [self.bar[0], self.bar[1], self.bar[2], 1.0];
        for ([cx, cy], color) in &self.buttons {
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - self.button_r;
            let a = (0.5 - d).clamp(0.0, 1.0) as f32;
            if a > 0.0 {
                px = over([color[0], color[1], color[2], a], px);
            }
        }
        px
    }
}

// ------------------------------------------------------------------------------------- pixels

/// One output pixel worked out from every layer: background, shadow, then the frame and the
/// image clipped to the rounded content.
fn pixel(
    x: usize,
    y: usize,
    frame: &Frame,
    layout: &Layout,
    background: &Fill,
    shadow: Option<&ShadowMask>,
    chrome: Option<&Chrome>,
) -> [u8; 4] {
    let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
    let inside = layout.content.coverage(fx, fy) as f32;
    let mut px = background.at(x, y);
    if let Some(shadow) = shadow {
        let a = shadow.at(x, y) as f32 * (1.0 - inside);
        if a > 0.0 {
            px = over([0.0, 0.0, 0.0, a], px);
        }
    }
    if inside > 0.0 {
        let local_y = y as i64 - layout.image_y as i64;
        let content = if local_y < 0 {
            chrome.map(|c| c.at(fx, fy)).unwrap_or([0.0; 4])
        } else {
            let ix = x as i64 - layout.image_x as i64;
            match (u32::try_from(ix), u32::try_from(local_y)) {
                (Ok(ix), Ok(iy)) => frame.rgba_at(ix, iy).map_or([0.0; 4], |p| p.map(|c| c as f32 / 255.0)),
                _ => [0.0; 4],
            }
        };
        if content[3] > 0.0 {
            px = over([content[0], content[1], content[2], content[3] * inside], px);
        }
    }
    px.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glimpsfy_annotate::{HexColor, Shadow};
    use glimpsfy_capture::ScaleFactor;

    /// A Display P3 capture keeps every one of its pixels exactly when it is beautified: the
    /// style's sRGB colours are converted into P3, the capture's own values are never converted,
    /// clipped or blended, and a 2× capture gets twice the points as pixels.
    #[test]
    fn beautifying_a_p3_capture_leaves_its_pixels_as_they_were() {
        let (w, h) = (40u32, 30u32);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                // Saturated values only P3 holds, in BGRA as the system delivers a screen.
                data.extend_from_slice(&[(x * 6) as u8, (y * 8) as u8, 255 - (x * 3) as u8, 255]);
            }
        }
        let frame = Frame::new(
            PhysicalSize::new(w, h),
            (w * 4) as usize,
            PixelFormat::Bgra8,
            ColorInfo { space: ColorSpace::DisplayP3, icc_profile: None },
            ScaleFactor(2.0),
            data,
        )
        .unwrap();
        let style = BeautifyStyle {
            preset: None,
            padding: 10.0,
            corner: 4.0,
            shadow: Shadow::Deep,
            background: Background::Solid { color: HexColor::try_from("#ff0000".to_string()).unwrap() },
            frame: WindowFrame::None,
        };

        let out = apply(&frame, &style, false);
        assert_eq!(out.color.space, ColorSpace::DisplayP3);
        assert_eq!((out.size.width, out.size.height), (w + 40, h + 40), "10 pt of padding is 20 px at 2×");

        // Away from the rounded corners, every pixel of the capture is where it was, unchanged.
        let corner = 8;
        for y in 0..h {
            for x in 0..w {
                let in_corner = (x < corner || x >= w - corner) && (y < corner || y >= h - corner);
                if in_corner {
                    continue;
                }
                assert_eq!(out.rgba_at(x + 20, y + 20), frame.rgba_at(x, y), "pixel {x},{y} changed");
            }
        }

        // The padding is sRGB red converted into P3, not the sRGB bytes themselves.
        let red = out.rgba_at(0, 0).unwrap();
        assert!(red[0] < 255 && red[1] > 0, "the red was not converted into Display P3: {red:?}");
        assert_eq!(red[3], 255);
    }
}
