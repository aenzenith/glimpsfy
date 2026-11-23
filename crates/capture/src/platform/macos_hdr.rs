//! HDR stills on macOS: which displays show content brighter than SDR white, the capture request
//! for them, and the tone mapping that brings their half-float image down to an 8-bit frame.
//!
//! - A display takes this path while `NSScreen.maximumExtendedDynamicRangeColorComponentValue`
//!   (its EDR headroom) is above 1.0, which macOS reports only while some content on that screen
//!   draws brighter than SDR white. AppKit may only be read on the main thread, so `prepare`
//!   reads the value once and listens for `NSApplicationDidChangeScreenParametersNotification`,
//!   which AppKit posts when it changes; a capture reads the cache and never waits for the main
//!   thread.
//! - The request asks for `HDRLocalDisplay` in `RGhA` (64-bit half float) without a colour space,
//!   because a request with one gets its values clipped to 1.0 (Apple developer forums, thread
//!   815971). LocalDisplay is chosen because the target is what this screen shows.
//! - The answer comes in the display's own profile. An image with no value above 1.0 is converted
//!   into the frame's space as the 8-bit path's would be, so its SDR pixels keep their values.
//!   Any other image is read in the same profile with extended range and tone mapped by Core
//!   Graphics into an 8-bit BGRA bitmap of the frame's space (Display P3 or sRGB), so the frame
//!   keeps the layout and ICC profile of every other capture.
//! - Macs where this cannot work keep the SDR path: macOS 14 (the tone mapping functions are
//!   looked up with `dlsym` once, so the app still starts there) and Intel Macs, where
//!   ScreenCaptureKit ignores the HDR request.

use std::ffi::{c_void, CStr};
use std::ptr::NonNull;
use std::sync::{Mutex, Once, OnceLock};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{sel, ClassType, MainThreadMarker};
use objc2_app_kit::{NSApplicationDidChangeScreenParametersNotification, NSScreen};
use objc2_core_foundation::{CFDictionary, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceDisplayP3, kCGColorSpaceSRGB, CGBitmapContextCreate, CGBitmapInfo, CGBlendMode, CGColorSpace,
    CGContext, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo, CGToneMapping,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNumber, NSOperationQueue, NSString};
use objc2_screen_capture_kit::{SCCaptureDynamicRange, SCCaptureResolutionType, SCStreamConfiguration};

use crate::error::{CaptureError, Result};
use crate::frame::{ColorInfo, ColorSpace, Frame, PixelFormat};
use crate::geometry::{PhysicalSize, ScaleFactor};

/// `kCVPixelFormatType_64RGBAHalf`, spelled out to keep Core Video out of the dependencies.
const PIXEL_FORMAT_RGHA: u32 = u32::from_be_bytes(*b"RGhA");

/// SDR white, 1.0, as the bits of a half float.
const HALF_ONE: u16 = 0x3C00;
/// Infinity as the bits of a half float; anything above it is NaN.
const HALF_INFINITY: u16 = 0x7C00;

/// The EDR headroom of each display by id, as AppKit last reported it.
static HEADROOM: Mutex<Vec<(u32, f32)>> = Mutex::new(Vec::new());

/// The headroom to capture `display` with when it takes the HDR path: its EDR headroom while that
/// is above 1.0, or 1.0 when `GLIMPSFY_FORCE_HDR` forces the path in a dev build. `None` keeps the
/// SDR path.
pub(super) fn headroom(display: u32) -> Option<f32> {
    if !available() {
        return None;
    }
    let current = HEADROOM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|(id, _)| *id == display)
        .map_or(1.0, |(_, headroom)| *headroom);
    if current > 1.0 {
        Some(current)
    } else {
        forced().then_some(1.0)
    }
}

/// Reads every screen's headroom on the main thread and, the first time, starts listening for its
/// changes. Called from a worker thread, which does not wait for the main thread's answer.
pub(super) fn prepare() {
    if !available() {
        return;
    }
    fn on_main(mtm: MainThreadMarker) {
        observe();
        refresh(mtm);
    }
    match MainThreadMarker::new() {
        Some(mtm) => on_main(mtm),
        None => DispatchQueue::main().exec_async(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                on_main(mtm);
            }
        }),
    }
}

/// Listens for AppKit's screen parameter changes, which it posts when a headroom changes.
fn observe() {
    static OBSERVING: Once = Once::new();
    OBSERVING.call_once(|| {
        let block = RcBlock::new(|_: NonNull<NSNotification>| {
            if let Some(mtm) = MainThreadMarker::new() {
                refresh(mtm);
            }
        });
        // SAFETY: the name is AppKit's constant, the block reads AppKit only on the main queue it
        // is given, and the returned observer is kept for the life of the process.
        let observer = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidChangeScreenParametersNotification),
                None,
                Some(&NSOperationQueue::mainQueue()),
                &block,
            )
        };
        std::mem::forget(observer);
    });
}

fn refresh(mtm: MainThreadMarker) {
    let key = NSString::from_str("NSScreenNumber");
    let fresh: Vec<(u32, f32)> = NSScreen::screens(mtm)
        .iter()
        .filter_map(|screen| {
            let number = screen.deviceDescription().objectForKey(&key)?.downcast::<NSNumber>().ok()?;
            Some((number.unsignedIntValue(), screen.maximumExtendedDynamicRangeColorComponentValue() as f32))
        })
        .collect();
    *HEADROOM.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = fresh;
}

/// `GLIMPSFY_FORCE_HDR=1` in a dev build, which sends every display down the HDR path, so the path
/// can be timed and its SDR output compared on a Mac without HDR content on screen.
fn forced() -> bool {
    static FORCED: OnceLock<bool> = OnceLock::new();
    *FORCED.get_or_init(|| {
        cfg!(debug_assertions)
            && std::env::var("GLIMPSFY_FORCE_HDR").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    })
}

/// Whether this Mac can take the HDR path: Apple silicon, `setCaptureDynamicRange:` (macOS 15)
/// and Core Graphics' tone mapping (macOS 15).
fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        cfg!(target_arch = "aarch64")
            && SCStreamConfiguration::class().responds_to(sel!(setCaptureDynamicRange:))
            && api().is_some()
    })
}

/// The request for a display or window on a display that shows HDR content.
pub(super) fn hdr_configuration(size: PhysicalSize, shows_cursor: bool) -> Retained<SCStreamConfiguration> {
    // SAFETY: plain property setters on a configuration this function owns; the one added after
    // macOS 14.0 is only reached when `available` found it.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(size.width as usize);
        config.setHeight(size.height as usize);
        config.setCaptureDynamicRange(SCCaptureDynamicRange::HDRLocalDisplay);
        config.setPixelFormat(PIXEL_FORMAT_RGHA);
        config.setShowsCursor(shows_cursor);
        config.setCaptureResolution(SCCaptureResolutionType::Best);
        config.setScalesToFit(false);
        config
    }
}

/// Whether ScreenCaptureKit answered in half floats, which only an HDR request gets; the image
/// decides rather than the request, because a Mac that ignores the request answers in 8 bits.
pub(super) fn is_half_float(image: &CGImage) -> bool {
    CGImage::bits_per_component(Some(image)) == 16
        && CGImage::bitmap_info(Some(image)).0 & CGImageComponentInfo::Float.0 != 0
}

/// `CGContextDrawImageApplyingToneMapping`.
type DrawApplyingToneMapping =
    unsafe extern "C-unwind" fn(*mut CGContext, CGRect, *const CGImage, CGToneMapping, *const CFDictionary) -> bool;
/// `CGImageGetContentHeadroom`.
type ContentHeadroom = unsafe extern "C-unwind" fn(*const CGImage) -> f32;
/// `CGImageCreateCopyWithContentHeadroom`.
type CopyWithHeadroom = unsafe extern "C-unwind" fn(f32, *const CGImage) -> *mut CGImage;

/// The tone mapping functions of macOS 15, looked up at run time so the app starts on macOS 14.
struct Api {
    draw: DrawApplyingToneMapping,
    content_headroom: ContentHeadroom,
    with_headroom: CopyWithHeadroom,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let find = |name: &CStr| {
            // SAFETY: a lookup by name in the images already loaded; Core Graphics is linked.
            let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            (!found.is_null()).then_some(found)
        };
        let draw = find(c"CGContextDrawImageApplyingToneMapping")?;
        let content_headroom = find(c"CGImageGetContentHeadroom")?;
        let with_headroom = find(c"CGImageCreateCopyWithContentHeadroom")?;
        // SAFETY: the signatures are the ones Core Graphics declares for these names.
        unsafe {
            Some(Api {
                draw: std::mem::transmute::<*mut c_void, DrawApplyingToneMapping>(draw),
                content_headroom: std::mem::transmute::<*mut c_void, ContentHeadroom>(content_headroom),
                with_headroom: std::mem::transmute::<*mut c_void, CopyWithHeadroom>(with_headroom),
            })
        }
    })
    .as_ref()
}

/// Where a half-float image keeps its channels.
#[derive(Clone, Copy, Debug)]
struct Layout {
    /// Halves per pixel.
    channels: usize,
    /// The channel that holds alpha or padding, which is not a colour.
    skip: Option<usize>,
    /// Whether `skip` is alpha, which never passes 1.0, rather than padding, which holds anything.
    skip_is_alpha: bool,
    little_endian: bool,
}

impl Layout {
    fn of(image: &CGImage) -> Layout {
        let channels = (CGImage::bits_per_pixel(Some(image)) / 16).max(1);
        let info = CGImage::alpha_info(Some(image));
        let skip = match info {
            CGImageAlphaInfo::First | CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::NoneSkipFirst => Some(0),
            CGImageAlphaInfo::None => None,
            _ => Some(channels - 1),
        };
        let skip_is_alpha = !matches!(info, CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast);
        let little_endian = CGImage::byte_order_info(Some(image)) == CGImageByteOrderInfo::Order16Little;
        Layout { channels, skip, skip_is_alpha, little_endian }
    }
}

/// The brightest colour value of a half-float image, as the bits of a non-negative half float.
/// Negative values (colours outside the space's gamut) and NaN are passed over. Bits are compared
/// instead of values, because `CGImageCalculateContentHeadroom` needs macOS 26, and a
/// non-negative half float orders like its bits.
fn brightest(bytes: &[u8], width: usize, height: usize, bytes_per_row: usize, layout: Layout) -> u16 {
    let below_nan = |bits: u16| if bits <= HALF_INFINITY { bits } else { 0 };
    let pixel = layout.channels * 2;
    let mut colours = (0..layout.channels).filter(|c| Some(*c) != layout.skip);
    let (Some(r), Some(g), Some(b)) = (colours.next(), colours.next(), colours.next()) else { return 0 };
    let read = |p: &[u8], channel: usize| {
        let half = [p[channel * 2], p[channel * 2 + 1]];
        below_nan(if layout.little_endian { u16::from_le_bytes(half) } else { u16::from_be_bytes(half) })
    };
    // ScreenCaptureKit's layout (RGBA, little endian, alpha last) is read as one run of halves
    // into 32 lanes, which the compiler vectorises: about ten times faster than a pixel at a time
    // (2 ms for a 3600 × 2338 frame). Alpha joins the run, which is harmless because it never
    // passes 1.0.
    let run = cfg!(target_endian = "little")
        && layout.little_endian
        && layout.skip_is_alpha
        && layout.channels == 4
        && (r, g, b, layout.skip) == (0, 1, 2, Some(3));
    let mut lanes = [0u16; 32];
    let mut brightest = 0u16;
    for y in 0..height {
        let Some(line) = bytes.get(y * bytes_per_row..y * bytes_per_row + width * pixel) else { break };
        // SAFETY: every bit pattern is a valid u16; a row that does not start on two bytes comes
        // back partly in `unaligned` and takes the pixel loop.
        let (unaligned, halves, rest) = unsafe { line.align_to::<u16>() };
        if run && unaligned.is_empty() && rest.is_empty() {
            let mut chunks = halves.chunks_exact(lanes.len());
            for chunk in &mut chunks {
                for (lane, bits) in lanes.iter_mut().zip(chunk) {
                    *lane = (*lane).max(below_nan(*bits));
                }
            }
            brightest = chunks.remainder().iter().map(|bits| below_nan(*bits)).fold(brightest, u16::max);
            continue;
        }
        for p in line.chunks_exact(pixel) {
            brightest = brightest.max(read(p, r)).max(read(p, g)).max(read(p, b));
        }
    }
    lanes.into_iter().fold(brightest, u16::max)
}

/// A non-negative half float's value.
fn half_value(bits: u16) -> f32 {
    let exponent = ((bits >> 10) & 0x1F) as i32;
    let mantissa = (bits & 0x3FF) as f32;
    match exponent {
        0 => mantissa * 2f32.powi(-24),
        0x1F => 65504.0,
        _ => (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}

/// Turns a half-float HDR image into an 8-bit BGRA frame in `space`, with straight alpha when
/// `alpha` keeps it (a window) and opaque otherwise. `headroom` is the display's, used when the
/// image does not carry its own. Returns the frame and the headroom it was tone mapped from,
/// which is `None` when no value passed SDR white and the image was drawn as it is.
pub(super) fn tone_mapped_frame(
    image: &CGImage,
    space: ColorSpace,
    scale: ScaleFactor,
    headroom: f32,
    alpha: bool,
) -> Result<(Frame, Option<f32>)> {
    let api = api().ok_or_else(|| CaptureError::Backend("tone mapping needs macOS 15".into()))?;
    if !is_half_float(image) {
        return Err(CaptureError::Backend("the HDR capture is not in half floats".into()));
    }
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    let provider = CGImage::data_provider(Some(image)).ok_or_else(|| CaptureError::Backend("no pixel data".into()))?;
    let data = CGDataProvider::data(Some(&provider)).ok_or_else(|| CaptureError::Backend("no pixel data".into()))?;
    // SAFETY: the bytes are read while `data` is alive and nothing mutates it.
    let source = unsafe { data.as_bytes_unchecked() };
    let brightest = brightest(source, width, height, CGImage::bytes_per_row(Some(image)), Layout::of(image));
    drop(data);
    if brightest <= HALF_ONE {
        // Nothing passes SDR white: vImage converts the image as the 8-bit path's frame would
        // be, in 6 ms for a 3600 × 2338 frame where Core Graphics' first draw takes 50.
        let mut frame = super::macos::converted_frame(image, space, scale)?;
        if alpha {
            let (width, height) = (frame.size.width as usize, frame.size.height as usize);
            straighten(&mut frame.data, width, height, frame.bytes_per_row);
        }
        return Ok((frame, None));
    }

    // ScreenCaptureKit tags the image with the display's own profile, whose range Core Graphics
    // ends at 1.0 (it refuses to tone map it); the same profile with extended range keeps the
    // values above SDR white.
    let source_space =
        CGImage::color_space(Some(image)).ok_or_else(|| CaptureError::Backend("no colour space".into()))?;
    let extended = if source_space.uses_extended_range() {
        // SAFETY: a valid image, retained for as long as `extended` lives.
        unsafe { CFRetained::retain(NonNull::from(image)) }
    } else {
        let space = source_space.extended().ok_or_else(|| CaptureError::Backend("no extended space".into()))?;
        CGImage::new_copy_with_color_space(Some(image), Some(&space))
            .ok_or_else(|| CaptureError::Backend("the HDR capture could not be read as extended".into()))?
    };
    // SAFETY: a plain getter on a valid image.
    let own = unsafe { (api.content_headroom)(&*extended) };
    let (mapped, peak) = if own > 1.0 {
        (extended, own)
    } else {
        // A cache that has not heard of a change yet says 1.0; the content's own peak is the
        // least it reaches then.
        let peak = if headroom > 1.0 { headroom } else { linear(half_value(brightest)) };
        // SAFETY: the copy comes back retained, and `from_raw` takes that reference over.
        let copy = NonNull::new(unsafe { (api.with_headroom)(peak, &*extended) })
            .map(|copy| unsafe { CFRetained::from_raw(copy) })
            .ok_or_else(|| CaptureError::Backend("the HDR capture took no headroom".into()))?;
        (copy, peak)
    };

    let target = CGColorSpace::with_name(Some(match space {
        // SAFETY: Core Graphics' constant strings live for the whole process.
        ColorSpace::Srgb => unsafe { kCGColorSpaceSRGB },
        ColorSpace::DisplayP3 => unsafe { kCGColorSpaceDisplayP3 },
    }))
    .ok_or_else(|| CaptureError::Backend("no target colour space".into()))?;
    let bytes_per_row = width * 4;
    let mut bytes = vec![0u8; bytes_per_row * height];
    let bgra = CGBitmapInfo(CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0);
    // SAFETY: `bytes` holds `height` rows of `bytes_per_row` and outlives the context, which is
    // dropped before the bytes are read.
    let context = unsafe {
        CGBitmapContextCreate(bytes.as_mut_ptr().cast(), width, height, 8, bytes_per_row, Some(&target), bgra.0)
    }
    .ok_or_else(|| CaptureError::Backend("Core Graphics could not make a bitmap".into()))?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64));
    // Copy writes each pixel as it comes, alpha included, so a screen stays opaque and a window
    // keeps its own transparency.
    CGContext::set_blend_mode(Some(&context), CGBlendMode::Copy);
    // SAFETY: context and image are valid for the call, and no options are passed.
    let drawn = unsafe {
        (api.draw)(CFRetained::as_ptr(&context).as_ptr(), rect, &*mapped, CGToneMapping::Default, std::ptr::null())
    };
    drop(context);
    if !drawn {
        return Err(CaptureError::Backend("Core Graphics could not tone map the HDR capture".into()));
    }
    if alpha {
        straighten(&mut bytes, width, height, bytes_per_row);
    }
    let frame = Frame::new(
        PhysicalSize::new(width as u32, height as u32),
        bytes_per_row,
        PixelFormat::Bgra8,
        ColorInfo::for_space(space),
        scale,
        bytes,
    )?;
    Ok((frame, Some(peak)))
}

/// An encoded value of the sRGB curve (which Display P3 and the Mac's panel profiles share) as
/// linear light, extended above 1.0.
fn linear(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Divides premultiplied alpha out of 8-bit BGRA rows, the way `frame_from_image` does for a
/// window from the SDR path.
fn straighten(bytes: &mut [u8], width: usize, height: usize, bytes_per_row: usize) {
    for y in 0..height {
        let Some(line) = bytes.get_mut(y * bytes_per_row..y * bytes_per_row + width * 4) else { break };
        for p in line.chunks_exact_mut(4) {
            if p[3] != 255 {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = (*c as u32 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CFData, CFString};
    use objc2_core_graphics::{kCGColorSpaceExtendedLinearDisplayP3, CGColorRenderingIntent};

    use super::*;

    /// A value in [0, 1] as the bits of a half float, rounded to the nearest.
    fn to_half(value: f32) -> u16 {
        if value < 2f32.powi(-14) {
            return (value / 2f32.powi(-24)).round() as u16;
        }
        let bits = value.to_bits();
        let exponent = ((bits >> 23) & 0xFF) + 15 - 127;
        ((exponent << 10) + (((bits & 0x7F_FFFF) + 0x1000) >> 13)) as u16
    }

    /// A one-row image in ScreenCaptureKit's HDR layout (RGBA half floats, little endian,
    /// premultiplied alpha last) of `pixels`, given as straight RGBA.
    fn half_image(pixels: &[[f32; 4]], space: &CFString) -> CFRetained<CGImage> {
        let mut bytes = Vec::with_capacity(pixels.len() * 8);
        for [r, g, b, a] in pixels {
            for value in [r * a, g * a, b * a, *a] {
                bytes.extend_from_slice(&to_half(value).to_le_bytes());
            }
        }
        let provider = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&bytes))).unwrap();
        let space = CGColorSpace::with_name(Some(space)).unwrap();
        let info = CGBitmapInfo(
            CGImageAlphaInfo::PremultipliedLast.0
                | CGImageComponentInfo::Float.0
                | CGImageByteOrderInfo::Order16Little.0,
        );
        // SAFETY: the provider holds one row of `pixels.len()` pixels of 8 bytes.
        unsafe {
            CGImage::new(
                pixels.len(),
                1,
                16,
                64,
                pixels.len() * 8,
                Some(&space),
                info,
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
        .unwrap()
    }

    /// While a display shows HDR content, every capture of it takes this path, also when the
    /// captured part holds no HDR value. A wrong byte order, alpha or colour space here would
    /// shift the colours of every such capture without a word, and the shift would stay in the
    /// files and on the clipboard, so SDR values must come out as the 8-bit path gives them.
    #[test]
    fn sdr_values_keep_their_bytes() {
        if api().is_none() {
            return;
        }
        // 8-bit values and alpha: white, black, greys, near white, the primaries, a mix, and a
        // window's soft edge.
        let colours: [[u8; 4]; 11] = [
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [128, 128, 128, 255],
            [254, 253, 252, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [12, 200, 77, 255],
            [3, 1, 2, 255],
            [250, 128, 10, 255],
            [40, 160, 220, 191],
        ];
        let decode = |code: u8| {
            let v = code as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let cases: [(&CFString, ColorSpace, bool); 3] = [
            // As ScreenCaptureKit answers: encoded values in a profile without extended range.
            (unsafe { kCGColorSpaceDisplayP3 }, ColorSpace::DisplayP3, false),
            (unsafe { kCGColorSpaceSRGB }, ColorSpace::Srgb, false),
            (unsafe { kCGColorSpaceExtendedLinearDisplayP3 }, ColorSpace::DisplayP3, true),
        ];
        for (source, space, linear_values) in cases {
            let pixels: Vec<[f32; 4]> = colours
                .iter()
                .map(|[r, g, b, a]| {
                    let value = |code: u8| if linear_values { decode(code) } else { code as f32 / 255.0 };
                    [value(*r), value(*g), value(*b), *a as f32 / 255.0]
                })
                .collect();
            let image = half_image(&pixels, source);
            for alpha in [true, false] {
                let (frame, mapped) = tone_mapped_frame(&image, space, ScaleFactor(2.0), 3.2, alpha).unwrap();
                assert_eq!(mapped, None);
                assert_eq!((frame.format, frame.color.space), (PixelFormat::Bgra8, space));
                for (index, [r, g, b, a]) in colours.iter().enumerate() {
                    if !alpha && *a != 255 {
                        continue;
                    }
                    let got = &frame.data[index * 4..index * 4 + 4];
                    let want = [*b, *g, *r, *a];
                    let close = got.iter().zip(want).all(|(got, want)| got.abs_diff(want) <= 1);
                    assert!(close, "{space:?} from {source:?}, alpha {alpha}: {want:?} came out as {got:?}");
                }
            }
        }
    }
}
