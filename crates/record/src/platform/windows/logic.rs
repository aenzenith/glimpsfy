//! The arithmetic of the Windows writer and reader that needs no system call: bit rates and
//! levels, Media Foundation's time and size encodings, BGRA to NV12 and float to 16-bit PCM,
//! rows behind a signed stride, and the waveform's peaks. It uses the standard library only,
//! so it can be compiled and exercised on any host.

/// Media Foundation counts time in 100 ns units.
pub const HNS_PER_SECOND: i64 = 10_000_000;

/// A key frame at least this often, so seeking in the trim window stays quick on a still screen
/// whose few frames would otherwise all hang off one key frame.
pub const KEYFRAME_EVERY_HNS: i64 = HNS_PER_SECOND;

/// `ns` nanoseconds in 100 ns units.
pub fn hns(ns: u64) -> i64 {
    (ns / 100).min(i64::MAX as u64) as i64
}

/// `hns` 100 ns units in nanoseconds; times before zero count as zero.
pub fn ns(hns: i64) -> u64 {
    hns.max(0) as u64 * 100
}

/// Two 32-bit values in one 64-bit attribute, the way `MFSetAttributeSize` and
/// `MFSetAttributeRatio` store them (the first in the high half).
pub fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

pub fn unpack(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, value as u32)
}

/// The average bit rate of a take, the same as the macOS writer's: bits per pixel per frame
/// (0.08 for H.264, 0.05 for HEVC, which needs less for the same picture), clamped to 2–60 Mbit/s.
pub fn bit_rate(width: u32, height: u32, fps: u32, hevc: bool) -> u32 {
    let bpp = if hevc { 0.05 } else { 0.08 };
    let bits = width as f64 * height as f64 * fps.max(1) as f64 * bpp;
    bits.clamp(2_000_000.0, 60_000_000.0) as u32
}

/// The AAC encoder takes four bit rates only (12000, 16000, 20000 and 24000 bytes per second):
/// the mix gets the highest, as the macOS writer gives it 192 kbit/s, and a single source
/// 160 kbit/s.
pub fn aac_bytes_per_second(mixed: bool) -> u32 {
    if mixed {
        24_000
    } else {
        20_000
    }
}

/// The lowest HEVC level (as `eAVEncH265VLevel`, thirty times the level number) whose picture
/// size, width, height and luma sample rate limits (ITU-T H.265 Annex A, table A.8) hold
/// `width`×`height` at `fps`; level 6.2 when nothing smaller does.
pub fn hevc_level(width: u32, height: u32, fps: u32) -> u32 {
    // (level, MaxLumaPs, MaxLumaSr)
    const LEVELS: [(u32, u64, u64); 8] = [
        (120, 2_228_224, 66_846_720),
        (123, 2_228_224, 133_693_440),
        (150, 8_912_896, 267_386_880),
        (153, 8_912_896, 534_773_760),
        (156, 8_912_896, 1_069_547_520),
        (180, 35_651_584, 1_069_547_520),
        (183, 35_651_584, 2_139_095_040),
        (186, 35_651_584, 4_278_190_080),
    ];
    let picture = width as u64 * height as u64;
    let side = width.max(height) as u64;
    let rate = picture * fps.max(1) as u64;
    LEVELS
        .iter()
        .find(|&&(_, max_picture, max_rate)| {
            // A side may be at most sqrt(8 × MaxLumaPs).
            picture <= max_picture && side * side <= 8 * max_picture && rate <= max_rate
        })
        .map_or(186, |&(level, _, _)| level)
}

/// How many video frames may wait for the encoder before new ones are dropped: half a second's
/// worth (between 4 and 30), and never more than about 384 MB of converted pictures, so a slow
/// software encoder on a 5K screen drops frames instead of filling the memory.
pub fn backlog_limit(fps: u32, frame_bytes: usize) -> u64 {
    let by_time = (fps / 2).clamp(4, 30) as u64;
    let by_memory = (384 * 1024 * 1024 / frame_bytes.max(1)) as u64;
    by_time.min(by_memory).max(2)
}

/// Whether the frame at `at` should be forced to be a key frame.
pub fn keyframe_due(last_key: Option<i64>, at: i64) -> bool {
    last_key.is_none_or(|key| at - key >= KEYFRAME_EVERY_HNS)
}

/// When a still screen has sent no frame for a while, the last picture is written again so the
/// file holds a video sample every second, as long as the audio has moved on; this returns the
/// time of that repeat, if one is due. The repeat stays a quarter second behind the audio, which
/// may still be on its way.
pub fn repeat_due(last_video: i64, audio_end: i64) -> Option<i64> {
    let at = last_video + HNS_PER_SECOND;
    (audio_end - HNS_PER_SECOND / 4 >= at).then_some(at)
}

/// The trim's start and end in nanoseconds, the end at least a millisecond after the start.
pub fn trim_window(start_ms: u64, end_ms: u64) -> (u64, u64) {
    let start = start_ms.saturating_mul(1_000_000);
    (start, end_ms.saturating_mul(1_000_000).max(start + 1_000_000))
}

/// The size of a picture at most `max_height` pixels high (and at least 16), keeping the aspect
/// ratio of `width`×`height`.
pub fn poster_size(width: u32, height: u32, max_height: u32) -> (u32, u32) {
    let source_height = height.max(1);
    let out_height = max_height.clamp(16, source_height.max(16));
    let out_width = (width as f64 * out_height as f64 / source_height as f64).round().max(1.0) as u32;
    (out_width, out_height)
}

/// Text for a compressed format the reader does not name: the four characters of the FOURCC
/// that the subtype GUID's first field holds, as the macOS reader reports `avc1` or `hvc1`.
pub fn fourcc(data1: u32) -> String {
    let text: String = data1
        .to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() { (b as char).to_ascii_lowercase() } else { '?' })
        .collect();
    text.trim_end_matches('?').to_string()
}

/// The part of a decoded picture to keep: `aperture` is Media Foundation's minimum display
/// aperture (x, y, width, height), clamped into the `width`×`height` buffer; without one the
/// whole buffer is kept.
pub fn visible_rect(aperture: Option<(i32, i32, i32, i32)>, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let Some((x, y, w, h)) = aperture else { return (0, 0, width, height) };
    let x = x.clamp(0, width as i32) as u32;
    let y = y.clamp(0, height as i32) as u32;
    let w = (w.max(0) as u32).min(width - x);
    let h = (h.max(0) as u32).min(height - y);
    if w == 0 || h == 0 {
        (0, 0, width, height)
    } else {
        (x, y, w, h)
    }
}

/// A picture of 32-bit pixels in memory.
pub struct Pixels<'a> {
    pub data: &'a [u8],
    /// The distance from one row to the next. A negative stride is a bottom-up picture: row 0 is
    /// the last one in `data`, which is how Media Foundation lays out a negative pitch.
    pub stride: isize,
    pub width: usize,
    pub height: usize,
    /// Red first (RGBA); otherwise blue first (BGRA, and BGRX as `MFVideoFormat_RGB32` is).
    pub red_first: bool,
}

impl Pixels<'_> {
    /// Row `y`, `width` pixels long, or `None` outside the picture or the buffer.
    pub fn row(&self, y: usize) -> Option<&[u8]> {
        if y >= self.height {
            return None;
        }
        let step = self.stride.unsigned_abs();
        let index = if self.stride >= 0 { y } else { self.height - 1 - y };
        let start = index.checked_mul(step)?;
        self.data.get(start..start.checked_add(self.width * 4)?)
    }

    /// Whether every row lies inside the buffer.
    pub fn fits(&self) -> bool {
        self.stride.unsigned_abs() >= self.width * 4
            && (self.height == 0 || (self.row(0).is_some() && self.row(self.height - 1).is_some()))
    }

    /// The part `(x, y, width, height)` as tight RGBA rows with opaque alpha, or `None` when it
    /// does not lie inside the picture.
    pub fn to_rgba(&self, (x, y, width, height): (u32, u32, u32, u32)) -> Option<Vec<u8>> {
        let (x, y, width, height) = (x as usize, y as usize, width as usize, height as usize);
        if x + width > self.width || y + height > self.height {
            return None;
        }
        let mut out = Vec::with_capacity(width * height * 4);
        for row in y..y + height {
            let pixels = self.row(row)?.get(x * 4..(x + width) * 4)?;
            for p in pixels.chunks_exact(4) {
                if self.red_first {
                    out.extend_from_slice(&[p[0], p[1], p[2], 255]);
                } else {
                    out.extend_from_slice(&[p[2], p[1], p[0], 255]);
                }
            }
        }
        Some(out)
    }

    /// The pixel at (`x`, `y`) as red, green, blue; black outside the picture, which pads a
    /// frame that is smaller than the file.
    fn rgb(&self, row: Option<&[u8]>, x: usize) -> [i32; 3] {
        match row.and_then(|r| r.get(x * 4..x * 4 + 3)) {
            Some(p) if self.red_first => [p[0] as i32, p[1] as i32, p[2] as i32],
            Some(p) => [p[2] as i32, p[1] as i32, p[0] as i32],
            None => [0, 0, 0],
        }
    }
}

/// The bytes of a `width`×`height` NV12 picture: a full-size luma plane, then a half-size plane
/// of interleaved Cb and Cr.
pub fn nv12_len(width: usize, height: usize) -> usize {
    width * height + width * height.div_ceil(2)
}

/// Converts `src` into the NV12 picture `out` of `width`×`height` (both even) with the BT.709
/// matrix in limited range (16–235), the colour the H.264 and HEVC streams are tagged with. A
/// source larger than the picture is cut at the right and bottom, a smaller one is padded with
/// black. Large pictures are split into bands converted on a few threads. Returns `false`, and
/// leaves `out` alone, when the sizes do not fit together.
pub fn bgra_to_nv12(src: &Pixels, out: &mut [u8], width: usize, height: usize) -> bool {
    if width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || out.len() != nv12_len(width, height)
    {
        return false;
    }
    if !src.fits() {
        return false;
    }
    let (luma, chroma) = out.split_at_mut(width * height);
    let pairs = height / 2;
    let threads = if width * height >= 200_000 {
        std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 4)
    } else {
        1
    };
    let band = pairs.div_ceil(threads).max(1);
    if threads == 1 {
        convert_band(src, luma, chroma, 0, width);
        return true;
    }
    std::thread::scope(|scope| {
        let bands = luma.chunks_mut(band * 2 * width).zip(chroma.chunks_mut(band * width));
        for (index, (luma, chroma)) in bands.enumerate() {
            scope.spawn(move || convert_band(src, luma, chroma, index * band, width));
        }
    });
    true
}

/// Converts the row pairs from `first_pair` on into `luma` (two rows per pair) and `chroma`
/// (one row per pair). The loop indexes plain slices rather than chaining iterators, because the
/// app's dev builds run this crate unoptimised and the recording must still keep up there.
fn convert_band(src: &Pixels, luma: &mut [u8], chroma: &mut [u8], first_pair: usize, width: usize) {
    let (red, blue) = if src.red_first { (0, 2) } else { (2, 0) };
    // Blocks of 2×2 pixels that lie wholly inside the source take the quick path.
    let inside = width.min(src.width) / 2;
    let pairs = chroma.len() / width;
    for pair in 0..pairs {
        let y = (first_pair + pair) * 2;
        let Some((luma_top, rest)) = luma.get_mut(pair * 2 * width..).map(|rows| rows.split_at_mut(width)) else {
            return;
        };
        let luma_bottom = &mut rest[..width];
        let uv = &mut chroma[pair * width..(pair + 1) * width];
        let (top, bottom) = (src.row(y), src.row(y + 1));
        let mut block = 0;
        if let (Some(t), Some(b)) = (top, bottom) {
            while block < inside {
                let (x, i) = (block * 2, block * 8);
                let (r0, g0, b0) = (t[i + red] as i32, t[i + 1] as i32, t[i + blue] as i32);
                let (r1, g1, b1) = (t[i + 4 + red] as i32, t[i + 5] as i32, t[i + 4 + blue] as i32);
                let (r2, g2, b2) = (b[i + red] as i32, b[i + 1] as i32, b[i + blue] as i32);
                let (r3, g3, b3) = (b[i + 4 + red] as i32, b[i + 5] as i32, b[i + 4 + blue] as i32);
                luma_top[x] = luma_of([r0, g0, b0]);
                luma_top[x + 1] = luma_of([r1, g1, b1]);
                luma_bottom[x] = luma_of([r2, g2, b2]);
                luma_bottom[x + 1] = luma_of([r3, g3, b3]);
                let average =
                    [(r0 + r1 + r2 + r3 + 2) >> 2, (g0 + g1 + g2 + g3 + 2) >> 2, (b0 + b1 + b2 + b3 + 2) >> 2];
                let (cb, cr) = chroma_of(average);
                uv[x] = cb;
                uv[x + 1] = cr;
                block += 1;
            }
        }
        // The rest (a picture wider or taller than the source, an odd last column) is padded
        // with black where the source has no pixel.
        while block < width / 2 {
            let x = block * 2;
            let pixels = [src.rgb(top, x), src.rgb(top, x + 1), src.rgb(bottom, x), src.rgb(bottom, x + 1)];
            luma_top[x] = luma_of(pixels[0]);
            luma_top[x + 1] = luma_of(pixels[1]);
            luma_bottom[x] = luma_of(pixels[2]);
            luma_bottom[x + 1] = luma_of(pixels[3]);
            let mut sum = [0i32; 3];
            for p in pixels {
                sum[0] += p[0];
                sum[1] += p[1];
                sum[2] += p[2];
            }
            let (cb, cr) = chroma_of([(sum[0] + 2) >> 2, (sum[1] + 2) >> 2, (sum[2] + 2) >> 2]);
            uv[x] = cb;
            uv[x + 1] = cr;
            block += 1;
        }
    }
}

/// BT.709 luma in limited range, with the coefficients scaled by 219/255 and 256.
fn luma_of([r, g, b]: [i32; 3]) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(16, 235) as u8
}

/// BT.709 Cb and Cr in limited range, with the coefficients scaled by 224/255 and 256.
fn chroma_of([r, g, b]: [i32; 3]) -> (u8, u8) {
    let cb = ((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128;
    let cr = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
    (cb.clamp(16, 240) as u8, cr.clamp(16, 240) as u8)
}

/// Appends `samples` as little-endian 16-bit PCM, the only input the AAC encoder takes; values
/// outside -1 to 1 are clipped and NaN becomes silence.
pub fn pcm16(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 2);
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
}

/// Raises the peaks of the bins that the interleaved `samples` (starting at `start` in 100 ns
/// units, `channels` per frame, `rate` frames per second) fall into, for a file `duration` long.
/// A peak is the largest magnitude of any channel, at most 1.
pub fn add_peaks(peaks: &mut [f32], duration: i64, start: i64, samples: &[f32], channels: usize, rate: u32) {
    if peaks.is_empty() || duration <= 0 || channels == 0 || rate == 0 {
        return;
    }
    let bins = peaks.len() as i128;
    let last = peaks.len() - 1;
    for (index, frame) in samples.chunks_exact(channels).enumerate() {
        let at = start as i128 + index as i128 * HNS_PER_SECOND as i128 / rate as i128;
        let bin = (at.max(0) * bins / duration as i128).min(last as i128) as usize;
        let peak = frame.iter().fold(0.0f32, |peak, s| peak.max(s.abs())).min(1.0);
        if let Some(slot) = peaks.get_mut(bin) {
            *slot = slot.max(peak);
        }
    }
}

/// What the privacy settings say about an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consent {
    Denied,
    Allowed,
    /// Nothing could be read, as on Windows versions without the switch.
    Unknown,
}

/// The consent behind the registry's `Value` strings, read from the most general switch to the
/// most specific one (the device's, the user's, then the one for desktop apps). Any "Deny"
/// wins, because each switch can block the microphone on its own.
pub fn consent<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Consent {
    let mut seen = Consent::Unknown;
    for value in values.into_iter().flatten() {
        if value.eq_ignore_ascii_case("Deny") {
            return Consent::Denied;
        }
        if value.eq_ignore_ascii_case("Allow") {
            seen = Consent::Allowed;
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    // A wrong colour conversion or a garbled sample format is written into every take, and the
    // take is the only copy of what was on screen: nothing notices until someone watches it.

    #[test]
    fn nv12_keeps_the_colours_of_the_screen() {
        // Four 2×2 blocks: white, black, pure red and a mid grey, in BGRA, one row of padding.
        let (width, height, stride) = (4usize, 4usize, 20usize);
        let colour = |x: usize, y: usize| -> [u8; 3] {
            match (x / 2, y / 2) {
                (0, 0) => [255, 255, 255],
                (1, 0) => [0, 0, 0],
                (0, 1) => [255, 0, 0],
                _ => [128, 128, 128],
            }
        };
        let mut data = vec![0u8; stride * height];
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = colour(x, y);
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        let src = Pixels { data: &data, stride: stride as isize, width, height, red_first: false };
        let mut out = vec![0u8; nv12_len(width, height)];
        assert!(bgra_to_nv12(&src, &mut out, width, height));
        let y_at = |x: usize, y: usize| out[y * width + x];
        let uv_at = |bx: usize, by: usize| {
            (out[width * height + by * width + bx * 2], out[width * height + by * width + bx * 2 + 1])
        };
        assert_eq!((y_at(0, 0), uv_at(0, 0)), (235, (128, 128)));
        assert_eq!((y_at(2, 0), uv_at(1, 0)), (16, (128, 128)));
        assert_eq!((y_at(0, 2), uv_at(0, 1)), (63, (102, 240)));
        assert_eq!((y_at(3, 3), uv_at(1, 1)), (126, (128, 128)));

        // The same picture bottom-up, and a bigger target padded with black.
        let mut flipped = vec![0u8; stride * height];
        for y in 0..height {
            flipped[(height - 1 - y) * stride..(height - y) * stride]
                .copy_from_slice(&data[y * stride..(y + 1) * stride]);
        }
        let src = Pixels { data: &flipped, stride: -(stride as isize), width, height, red_first: false };
        let mut again = vec![0u8; nv12_len(width, height)];
        assert!(bgra_to_nv12(&src, &mut again, width, height));
        assert_eq!(again, out);
        let mut padded = vec![0u8; nv12_len(6, 4)];
        assert!(bgra_to_nv12(&src, &mut padded, 6, 4));
        assert_eq!((padded[4], padded[6 * 4 + 4], padded[6 * 4 + 5]), (16, 128, 128));
        assert!(!bgra_to_nv12(&src, &mut padded, 5, 4));
    }

    #[test]
    fn pcm16_is_little_endian_and_clipped() {
        let mut out = Vec::new();
        pcm16(&[0.0, 1.0, -1.0, 2.0, f32::NAN, 0.5], &mut out);
        let values: Vec<i16> = out.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(values, [0, 32767, -32767, 32767, 0, 16384]);
    }
}
