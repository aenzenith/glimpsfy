//! The conversions of the Windows backend that touch no system API, so they compile and are
//! tested on every host: GDI and Windows.Graphics.Capture pixels to straight BGRA, the size and
//! scaling of a stream's frames, the pacing of a stream, the silence of an idle loopback, and
//! WASAPI's PCM to interleaved float.

use crate::frame::{ColorInfo, Frame, PixelFormat};
use crate::geometry::{PhysicalSize, ScaleFactor};

/// Sets every BGRA pixel opaque. GDI writes colour only: a 32-bit DIB comes back from `BitBlt`
/// and `PrintWindow` with its alpha bytes at 0 (or at whatever a layered window left there), and
/// a frame with that alpha would be saved as a transparent image.
pub fn make_opaque(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
}

/// Whether every pixel is black, whatever its alpha: what a capture of a desktop the session
/// may not read looks like, and what `PrintWindow` gives for a window it cannot draw.
pub fn is_black(pixels: &[u8]) -> bool {
    pixels.chunks_exact(4).all(|pixel| pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0)
}

/// The `width` × `height` pixels at (`x`, `y`) of rows that are `stride` bytes apart, as tight
/// rows; `None` when the region does not lie inside `source`.
pub fn copy_region(source: &[u8], stride: usize, x: u32, y: u32, width: u32, height: u32) -> Option<Vec<u8>> {
    let (x, y, width, height) = (x as usize, y as usize, width as usize, height as usize);
    let row = width.checked_mul(4)?;
    if width == 0 || height == 0 || stride < x.checked_add(width)?.checked_mul(4)? {
        return None;
    }
    let last = (y + height - 1).checked_mul(stride)?.checked_add(x * 4 + row)?;
    if source.len() < last {
        return None;
    }
    let mut out = Vec::with_capacity(row * height);
    for line in 0..height {
        let start = (y + line) * stride + x * 4;
        out.extend_from_slice(&source[start..start + row]);
    }
    Some(out)
}

/// Makes the premultiplied image of a window from Windows.Graphics.Capture a straight one. Only
/// the four corner squares of `corner` pixels keep their alpha, where Windows 11 rounds the
/// window; everywhere else the pixel becomes opaque, because many windows leave arbitrary alpha
/// in their client area (it means nothing on screen) and would be saved half transparent.
pub fn finish_window_pixels(pixels: &mut [u8], width: u32, height: u32, corner: u32) {
    let (width, height) = (width as usize, height as usize);
    if width == 0 || height == 0 {
        return;
    }
    let corner = (corner as usize).min(width / 2).min(height / 2);
    for (y, row) in pixels.chunks_exact_mut(width * 4).take(height).enumerate() {
        let corner_row = y < corner || y >= height - corner;
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            if !(corner_row && (x < corner || x >= width - corner)) {
                pixel[3] = 255;
                continue;
            }
            let alpha = pixel[3] as u32;
            if alpha == 0 {
                pixel[..3].fill(0);
            } else if alpha < 255 {
                for channel in &mut pixel[..3] {
                    *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
    }
}

/// Cuts rounded corners of `radius` pixels into an opaque image, with the edge of the arc
/// blended over one pixel. The fallbacks of a window capture (`PrintWindow`, a crop of the
/// screen) hold whatever lies behind the window's corners; this gives them the alpha
/// Windows.Graphics.Capture delivers on its own.
pub fn round_corners(pixels: &mut [u8], width: u32, height: u32, radius: f64) {
    let (width, height) = (width as usize, height as usize);
    let radius = radius.min(width as f64 / 2.0).min(height as f64 / 2.0);
    if radius < 0.5 || pixels.len() < width * height * 4 {
        return;
    }
    let side = radius.ceil() as usize;
    for j in 0..side {
        for i in 0..side {
            let distance = (radius - (i as f64 + 0.5)).hypot(radius - (j as f64 + 0.5));
            let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
            if coverage >= 1.0 {
                continue;
            }
            for (x, y) in [(i, j), (width - 1 - i, j), (i, height - 1 - j), (width - 1 - i, height - 1 - j)] {
                let pixel = &mut pixels[(y * width + x) * 4..(y * width + x) * 4 + 4];
                if coverage <= 0.0 {
                    pixel.fill(0);
                } else {
                    pixel[3] = (pixel[3] as f64 * coverage).round() as u8;
                }
            }
        }
    }
}

/// An even pixel count of at least 2, which video encoders need.
pub fn even(value: u32) -> u32 {
    (value & !1).max(2)
}

/// The size of a stream's frames for a source of `native` pixels: `output` when it is set, the
/// native size divided by `divisor` otherwise, with even sides either way.
pub fn stream_size(native: PhysicalSize, output: Option<PhysicalSize>, divisor: u32) -> PhysicalSize {
    match output {
        Some(size) => PhysicalSize::new(even(size.width), even(size.height)),
        None => {
            let divisor = divisor.max(1);
            PhysicalSize::new(even(native.width / divisor), even(native.height / divisor))
        }
    }
}

/// One frame of a stream as tight BGRA rows of `output` pixels.
///
/// `source` holds `available` pixels in rows `stride` bytes apart: what the system delivered
/// this time, which is the `native` size the stream was opened with unless a recorded window
/// was resized since. The picture keeps its place and its scale: a smaller one is padded with
/// black on the right and below, a larger one is cut there, because an encoder cannot change its
/// frame size during a take. An `output` one pixel short of `native` (an odd side made even)
/// cuts that pixel off instead of resampling the whole frame, and an `output` that is a whole
/// fraction of `native` averages whole blocks of pixels.
///
/// With `opaque`, every pixel is made opaque (GDI's pixels, and a window's premultiplied ones,
/// which then stand over black).
pub fn stream_pixels(
    source: &[u8],
    stride: usize,
    available: PhysicalSize,
    native: PhysicalSize,
    output: PhysicalSize,
    opaque: bool,
) -> Vec<u8> {
    let cut = |native: u32, output: u32| output <= native && native - output <= 1;
    let mut out = if cut(native.width, output.width) && cut(native.height, output.height) {
        fitted(source, stride, available, output)
    } else {
        match block_size(native, output) {
            Some(block) => averaged(source, stride, available, output, block),
            None => {
                // Any other size goes through the area average of `Frame::resized`, which is
                // exact and slow; the recorder only asks for the native size or half of it.
                let canvas = fitted(source, stride, available, native);
                let frame = Frame {
                    size: native,
                    bytes_per_row: native.width as usize * 4,
                    format: PixelFormat::Bgra8,
                    color: ColorInfo::srgb(),
                    scale: ScaleFactor::ONE,
                    data: canvas,
                };
                frame.resized(output).data
            }
        }
    };
    if opaque {
        make_opaque(&mut out);
    }
    out
}

/// The whole number of source pixels per output pixel on both axes, when `output` is `native`
/// divided by one (and made even).
fn block_size(native: PhysicalSize, output: PhysicalSize) -> Option<usize> {
    if output.width == 0 || output.height == 0 {
        return None;
    }
    let block = native.width / output.width;
    let same = block >= 2 && even(native.width / block) == output.width && even(native.height / block) == output.height;
    same.then_some(block as usize)
}

/// The top-left `output` pixels of the source, black where the source ends.
fn fitted(source: &[u8], stride: usize, available: PhysicalSize, output: PhysicalSize) -> Vec<u8> {
    let (out_w, out_h) = (output.width as usize, output.height as usize);
    let width = (available.width as usize).min(out_w).min(stride / 4);
    let rows = (available.height as usize).min(out_h);
    let mut out = Vec::with_capacity(out_w * out_h * 4);
    for y in 0..rows {
        let start = y * stride;
        let Some(row) = source.get(start..start + width * 4) else { break };
        out.extend_from_slice(row);
        out.extend(black(out_w - width));
    }
    let filled = out.len();
    out.extend(black((out_w * out_h * 4 - filled) / 4));
    out
}

/// `count` opaque black pixels.
fn black(count: usize) -> impl Iterator<Item = u8> {
    std::iter::repeat_n([0u8, 0, 0, 255], count).flatten()
}

/// `output` pixels that are each the mean of a `block` × `block` square of the source, black
/// where the source ends.
fn averaged(source: &[u8], stride: usize, available: PhysicalSize, output: PhysicalSize, block: usize) -> Vec<u8> {
    let (out_w, out_h) = (output.width as usize, output.height as usize);
    let width = (available.width as usize).min(stride / 4);
    let height = (available.height as usize).min(if stride == 0 { 0 } else { source.len().div_ceil(stride) });
    let area = (block * block) as u32;
    let mut out = vec![0u8; out_w * out_h * 4];
    for (y, row) in out.chunks_exact_mut(out_w * 4).enumerate() {
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            pixel[3] = 255;
            if (x + 1) * block > width || (y + 1) * block > height {
                continue;
            }
            let mut sum = [0u32; 4];
            for j in 0..block {
                let start = (y * block + j) * stride + x * block * 4;
                let Some(line) = source.get(start..start + block * 4) else { continue };
                for p in line.chunks_exact(4) {
                    sum[0] += p[0] as u32;
                    sum[1] += p[1] as u32;
                    sum[2] += p[2] as u32;
                    sum[3] += p[3] as u32;
                }
            }
            for (channel, total) in pixel.iter_mut().zip(sum) {
                *channel = ((total + area / 2) / area) as u8;
            }
        }
    }
    out
}

/// Keeps a stream at its frame rate. Windows.Graphics.Capture delivers a frame for every change
/// on screen, at the monitor's refresh rate; the pacer lets one through per slot of the
/// stream's own rate. The slots follow one another on the frames' own clock, so a 60 Hz monitor
/// recorded at 60 fps loses no frame to jitter, and a 144 Hz one gives 60 evenly.
#[derive(Clone, Copy, Debug)]
pub struct Pacer {
    interval: u64,
    due: Option<u64>,
}

impl Pacer {
    pub fn new(fps: u32) -> Pacer {
        Pacer { interval: 1_000_000_000 / fps.clamp(1, 1_000) as u64, due: None }
    }

    /// When the next frame is due, in host nanoseconds; `None` before the first frame.
    pub fn due(&self) -> Option<u64> {
        self.due
    }

    /// Whether the frame stamped `timestamp` goes out, asked at host time `now`. A frame that
    /// came too early for its slot is asked about again once the slot is due and then goes out,
    /// so the last change before the screen falls still is never lost.
    pub fn accept(&mut self, timestamp: u64, now: u64) -> bool {
        let Some(due) = self.due else {
            self.due = Some(timestamp + self.interval);
            return true;
        };
        // An eighth of a slot of tolerance: frames of a monitor at the stream's own rate arrive
        // a little early as often as a little late.
        if timestamp + self.interval / 8 < due && now < due {
            return false;
        }
        let next = due + self.interval;
        // After a still screen the slots start again from the frame that ended it.
        self.due = Some(if next <= timestamp { timestamp + self.interval } else { next });
        true
    }
}

/// Fills the time a loopback stream delivers nothing with silence. WASAPI's loopback sends no
/// packets while nothing plays, but a recording's system track and its level meter need a
/// source that goes on: without buffers the track ends where the sound did and the meter keeps
/// its last level.
///
/// The silence trails the clock by [`SilenceFiller::LAG`], so sound that starts again lands
/// after it with a gap the recorder's placer measures and fills exactly, instead of overlapping
/// silence already sent.
#[derive(Clone, Copy, Debug)]
pub struct SilenceFiller {
    covered_until: u64,
}

impl SilenceFiller {
    /// How long nothing may arrive before silence is sent: 400 ms.
    pub const AFTER: u64 = 400_000_000;
    /// How far the end of the silence stays behind the clock: 200 ms.
    pub const LAG: u64 = 200_000_000;

    pub fn new(now: u64) -> SilenceFiller {
        SilenceFiller { covered_until: now }
    }

    /// Real audio from `start` lasting `duration` nanoseconds went out.
    pub fn heard(&mut self, start: u64, duration: u64) {
        self.covered_until = self.covered_until.max(start + duration);
    }

    /// The silence to send at host time `now`, as its start and its length in nanoseconds.
    pub fn gap(&mut self, now: u64) -> Option<(u64, u64)> {
        if now.saturating_sub(self.covered_until) < Self::AFTER {
            return None;
        }
        let start = self.covered_until;
        self.covered_until = now - Self::LAG;
        Some((start, self.covered_until - start))
    }
}

/// How one PCM sample is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    UInt8,
    Int16,
    /// Three bytes per sample.
    Int24,
    /// Four bytes per sample, which also holds 24 valid bits in the high bytes.
    Int32,
    Float32,
    Float64,
}

impl SampleKind {
    /// The kind of a `WAVEFORMATEX` whose samples are IEEE float (`float`) or integer PCM, by
    /// the bits each sample occupies; `None` for a layout WASAPI's mixer does not use.
    pub fn of(float: bool, bits: u16) -> Option<SampleKind> {
        match (float, bits) {
            (true, 32) => Some(SampleKind::Float32),
            (true, 64) => Some(SampleKind::Float64),
            (false, 8) => Some(SampleKind::UInt8),
            (false, 16) => Some(SampleKind::Int16),
            (false, 24) => Some(SampleKind::Int24),
            (false, 32) => Some(SampleKind::Int32),
            _ => None,
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            SampleKind::UInt8 => 1,
            SampleKind::Int16 => 2,
            SampleKind::Int24 => 3,
            SampleKind::Int32 | SampleKind::Float32 => 4,
            SampleKind::Float64 => 8,
        }
    }
}

/// Little-endian PCM as float samples between −1 and 1, in the order given.
pub fn samples_to_f32(bytes: &[u8], kind: SampleKind) -> Vec<f32> {
    let samples = bytes.chunks_exact(kind.bytes());
    match kind {
        SampleKind::UInt8 => samples.map(|b| (b[0] as f32 - 128.0) / 128.0).collect(),
        SampleKind::Int16 => samples.map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0).collect(),
        // The three bytes go into the high end of an i32, which keeps the sign.
        SampleKind::Int24 => {
            samples.map(|b| i32::from_le_bytes([0, b[0], b[1], b[2]]) as f32 / 2_147_483_648.0).collect()
        }
        SampleKind::Int32 => {
            samples.map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0).collect()
        }
        SampleKind::Float32 => samples.map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
        SampleKind::Float64 => {
            samples.map(|b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32).collect()
        }
    }
}

/// The speaker positions of `dwChannelMask` this module knows, by their bit.
const FRONT_LEFT: u32 = 0x1;
const FRONT_RIGHT: u32 = 0x2;
const FRONT_CENTER: u32 = 0x4;
const BACK_LEFT: u32 = 0x10;
const BACK_RIGHT: u32 = 0x20;
const FRONT_LEFT_OF_CENTER: u32 = 0x40;
const FRONT_RIGHT_OF_CENTER: u32 = 0x80;
const BACK_CENTER: u32 = 0x100;
const SIDE_LEFT: u32 = 0x200;
const SIDE_RIGHT: u32 = 0x400;

/// The mask Windows assumes for a channel count when the format carries none.
fn default_mask(channels: usize) -> u32 {
    match channels {
        3 => 0x7,   // left, right, centre
        4 => 0x33,  // quadraphonic
        5 => 0x37,  // 5.0
        6 => 0x3F,  // 5.1
        7 => 0x13F, // 6.1
        8 => 0x63F, // 7.1 surround
        _ => 0x3,
    }
}

/// Interleaved samples of more than two channels as stereo, by the ITU downmix: the centre goes
/// to both sides at −3 dB, each surround channel to its own side at −3 dB, and the subwoofer is
/// left out. The recorder keeps the first two channels of whatever it is given, so the front
/// pair of a 5.1 device alone would lose every film's dialogue, which sits in the centre.
/// Mono and stereo pass through untouched.
pub fn downmix_to_stereo(samples: Vec<f32>, channels: usize, mask: u32) -> (Vec<f32>, u16) {
    if channels <= 2 {
        return (samples, channels as u16);
    }
    const HALF_POWER: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let mask = if mask.count_ones() as usize == channels { mask } else { default_mask(channels) };
    // The channels are stored in the order of their bits in the mask.
    let mut gains = Vec::with_capacity(channels);
    for bit in (0..32).map(|shift| 1u32 << shift).filter(|bit| mask & bit != 0).take(channels) {
        gains.push(match bit {
            FRONT_LEFT => (1.0, 0.0),
            FRONT_RIGHT => (0.0, 1.0),
            FRONT_CENTER => (HALF_POWER, HALF_POWER),
            BACK_LEFT | SIDE_LEFT | FRONT_LEFT_OF_CENTER => (HALF_POWER, 0.0),
            BACK_RIGHT | SIDE_RIGHT | FRONT_RIGHT_OF_CENTER => (0.0, HALF_POWER),
            BACK_CENTER => (0.5, 0.5),
            _ => (0.0, 0.0),
        });
    }
    // A mask that names fewer speakers than there are channels leaves the rest silent, apart
    // from the first two, which are then taken as the front pair.
    if gains.len() < channels {
        gains = vec![(0.0, 0.0); channels];
        gains[0] = (1.0, 0.0);
        gains[1] = (0.0, 1.0);
    }
    let mut out = Vec::with_capacity(samples.len() / channels * 2);
    for frame in samples.chunks_exact(channels) {
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for (sample, (to_left, to_right)) in frame.iter().zip(&gains) {
            left += sample * to_left;
            right += sample * to_right;
        }
        out.push(left.clamp(-1.0, 1.0));
        out.push(right.clamp(-1.0, 1.0));
    }
    (out, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows `stride` bytes apart whose pixels name their own place: blue is x, green is y, red
    /// is 9, and the alpha is 0 as GDI leaves it. The padding holds 0xEE, which must never show.
    fn source(width: usize, height: usize, stride: usize) -> Vec<u8> {
        let mut data = vec![0xEE; stride * height];
        for y in 0..height {
            for x in 0..width {
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[x as u8, y as u8, 9, 0]);
            }
        }
        data
    }

    fn pixel(data: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * width + x) * 4;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    /// What reaches a saved image or a recording must be the pixels the system drew, each in its
    /// place and opaque: rows are read by their pitch and never by their width, GDI's empty
    /// alpha does not make the image transparent, a window's corners keep their true colour, and
    /// a stream's frame keeps its size when the window behind it changes. A slip in any of them
    /// shears, blanks or tints the file, and the moment it showed is gone.
    #[test]
    fn captured_pixels_keep_their_place_their_colour_and_their_alpha() {
        // A padded source: the region comes out tight, from the right rows and columns.
        let padded = source(7, 5, 40);
        let region = copy_region(&padded, 40, 2, 1, 4, 3).unwrap();
        assert_eq!(region.len(), 4 * 3 * 4);
        assert_eq!(pixel(&region, 4, 0, 0), [2, 1, 9, 0]);
        assert_eq!(pixel(&region, 4, 3, 2), [5, 3, 9, 0]);
        assert_eq!(copy_region(&padded, 40, 8, 0, 4, 1), None, "a region past the end of a row");
        assert_eq!(copy_region(&padded, 40, 0, 3, 7, 3), None, "a region past the last row");

        let mut opaque = region.clone();
        assert!(!is_black(&opaque));
        make_opaque(&mut opaque);
        assert_eq!(pixel(&opaque, 4, 3, 2), [5, 3, 9, 255]);
        assert!(is_black(&[0, 0, 0, 77, 0, 0, 0, 0]));

        // A stream frame of the native size: an odd side loses its last pixel, nothing moves.
        let native = PhysicalSize::new(7, 5);
        assert_eq!(stream_size(native, None, 1), PhysicalSize::new(6, 4));
        let frame = stream_pixels(&padded, 40, native, native, PhysicalSize::new(6, 4), true);
        assert_eq!(frame.len(), 6 * 4 * 4);
        assert_eq!(pixel(&frame, 6, 0, 0), [0, 0, 9, 255]);
        assert_eq!(pixel(&frame, 6, 5, 3), [5, 3, 9, 255]);

        // The window shrank to 3 × 2: the picture stays at the top left over black.
        let shrunk = stream_pixels(&padded, 40, PhysicalSize::new(3, 2), native, PhysicalSize::new(6, 4), true);
        assert_eq!(pixel(&shrunk, 6, 2, 1), [2, 1, 9, 255]);
        assert_eq!(pixel(&shrunk, 6, 3, 1), [0, 0, 0, 255]);
        assert_eq!(pixel(&shrunk, 6, 0, 2), [0, 0, 0, 255]);

        // Half size: every pixel is the mean of its own 2 × 2 block.
        let wide = source(8, 4, 36);
        let native = PhysicalSize::new(8, 4);
        assert_eq!(stream_size(native, None, 2), PhysicalSize::new(4, 2));
        let half = stream_pixels(&wide, 36, native, native, PhysicalSize::new(4, 2), true);
        assert_eq!(half.len(), 4 * 2 * 4);
        // Blue of columns 6 and 7 is 6.5, rounded up; green of rows 2 and 3 likewise.
        assert_eq!(pixel(&half, 4, 3, 1), [7, 3, 9, 255]);
        assert_eq!(pixel(&half, 4, 0, 0), [1, 1, 9, 255]);

        // A premultiplied window image: the corner pixel on the arc gets its colour back, the
        // pixel outside the arc stays clear, and stray alpha in the body is made opaque.
        let mut window = vec![0u8; 8 * 8 * 4];
        for p in window.chunks_exact_mut(4) {
            p.copy_from_slice(&[40, 80, 120, 255]);
        }
        window[..4].copy_from_slice(&[0, 0, 0, 0]);
        window[4..8].copy_from_slice(&[20, 40, 60, 128]);
        let body = (4 * 8 + 4) * 4;
        window[body..body + 4].copy_from_slice(&[40, 80, 120, 3]);
        finish_window_pixels(&mut window, 8, 8, 2);
        assert_eq!(pixel(&window, 8, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&window, 8, 1, 0), [40, 80, 120, 128]);
        assert_eq!(pixel(&window, 8, 4, 4), [40, 80, 120, 255]);

        // Corners cut into an opaque image: clear at the very corner on all four, whole inside.
        let mut square = vec![255u8; 16 * 12 * 4];
        round_corners(&mut square, 16, 12, 4.0);
        for (x, y) in [(0, 0), (15, 0), (0, 11), (15, 11)] {
            assert_eq!(pixel(&square, 16, x, y)[3], 0, "corner {x},{y}");
        }
        for (x, y) in [(4, 4), (11, 7), (8, 0), (0, 6)] {
            assert_eq!(pixel(&square, 16, x, y), [255; 4], "inside at {x},{y}");
        }
        let edge = pixel(&square, 16, 1, 1)[3];
        assert!(edge > 0 && edge < 255, "the arc is blended, got {edge}");
    }

    /// A take's sound cannot be recorded again, and a wrong sample layout plays as noise or as
    /// silence without any error: integers keep their sign and scale, and a surround device's
    /// centre channel reaches both sides of the stereo the recorder keeps.
    #[test]
    fn pcm_keeps_its_sign_its_scale_and_the_centre_channel() {
        let int16: Vec<u8> = [0i16, 16_384, -16_384, i16::MIN].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(samples_to_f32(&int16, SampleKind::Int16), [0.0, 0.5, -0.5, -1.0]);
        // 0x400000 is half of full scale in 24 bits, 0xC00000 is minus half.
        let int24 = [0x00, 0x00, 0x40, 0x00, 0x00, 0xC0];
        assert_eq!(samples_to_f32(&int24, SampleKind::Int24), [0.5, -0.5]);
        let int32: Vec<u8> = [1i32 << 30, i32::MIN].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(samples_to_f32(&int32, SampleKind::Int32), [0.5, -1.0]);
        let float: Vec<u8> = [0.25f32, -0.75].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(samples_to_f32(&float, SampleKind::Float32), [0.25, -0.75]);
        assert_eq!(samples_to_f32(&[0, 128, 255], SampleKind::UInt8), [-1.0, 0.0, 127.0 / 128.0]);
        assert_eq!(SampleKind::of(true, 32), Some(SampleKind::Float32));
        assert_eq!(SampleKind::of(false, 24), Some(SampleKind::Int24));
        assert_eq!(SampleKind::of(true, 16), None);

        // 5.1 in WASAPI's order: left, right, centre, subwoofer, back left, back right.
        let surround = vec![0.25, 0.0, 0.5, 1.0, 0.0, 0.125];
        let (stereo, channels) = downmix_to_stereo(surround.clone(), 6, 0x3F);
        assert_eq!(channels, 2);
        let centre = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((stereo[0] - (0.25 + centre)).abs() < 1e-6, "left is {}", stereo[0]);
        assert!((stereo[1] - (centre + 0.125 * std::f32::consts::FRAC_1_SQRT_2)).abs() < 1e-6);
        // A format without a mask is read by the layout Windows assumes for its channel count.
        assert_eq!(downmix_to_stereo(surround, 6, 0).0, stereo);
        assert_eq!(downmix_to_stereo(vec![0.1, 0.2], 2, 0x3), (vec![0.1, 0.2], 2));
        assert_eq!(downmix_to_stereo(vec![0.3], 1, 0x4), (vec![0.3], 1));
    }
}
