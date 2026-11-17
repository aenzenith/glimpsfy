//! Image preparation before the engine reads: light-on-dark captures are inverted, and small text
//! is read again from an upscaled copy. The image always arrives at the capture's physical
//! resolution (the selection is cut from the frozen frame in pixels), so nothing here scales a
//! capture down.
//!
//! Each step is kept only because the sample set (`tests/samples`, `examples/accuracy.rs`) showed
//! that it lowers the character error rate; `docs/ARCHITECTURE.md` lists the numbers.

use glimpsfy_capture::{ColorInfo, Frame, PhysicalSize, PixelFormat};

use crate::RawText;

/// Which preparation steps run. The defaults are the measured winners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preparation {
    /// Inverts a capture whose background is dark (a terminal, dark mode) before reading it.
    pub invert_dark: bool,
    /// Reads the image again at a larger scale when its text is small.
    pub upscale_small_text: bool,
    /// Text whose median line box is shorter than this many pixels counts as small.
    pub small_text_px: f64,
    /// The line height the upscaled copy aims for.
    pub target_text_px: f64,
    /// Lets the engine correct words with its language model.
    pub language_correction: bool,
    /// Reads the image again without language correction when most of its text turns out to be
    /// code, because the language model "corrects" identifiers and spacing in code.
    pub code_without_correction: bool,
}

impl Default for Preparation {
    fn default() -> Self {
        Preparation {
            invert_dark: true,
            upscale_small_text: true,
            small_text_px: 24.0,
            target_text_px: 48.0,
            language_correction: true,
            code_without_correction: true,
        }
    }
}

impl Preparation {
    /// No preparation at all, the baseline the measurements compare against.
    pub fn none() -> Self {
        Preparation {
            invert_dark: false,
            upscale_small_text: false,
            code_without_correction: false,
            ..Preparation::default()
        }
    }
}

/// The relative luminance (0–1) of a pixel, from sRGB values without linearisation; it only has
/// to rank pixels.
fn luma(px: [u8; 4]) -> f64 {
    (0.2126 * px[0] as f64 + 0.7152 * px[1] as f64 + 0.0722 * px[2] as f64) / 255.0
}

/// Whether the image is light text on a dark background: the most common brightness (the
/// background) is dark. Up to about 250 000 pixels are sampled on a grid.
pub fn light_on_dark(frame: &Frame) -> bool {
    let (w, h) = (frame.size.width, frame.size.height);
    if w == 0 || h == 0 {
        return false;
    }
    let step = ((w as u64 * h as u64 / 250_000) as f64).sqrt().max(1.0) as u32;
    let mut bins = [0u32; 16];
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            if let Some(px) = frame.rgba_at(x, y) {
                bins[((luma(px) * 15.999) as usize).min(15)] += 1;
            }
            x += step;
        }
        y += step;
    }
    let background = bins.iter().enumerate().max_by_key(|(_, n)| **n).map_or(15, |(i, _)| i);
    background < 6
}

/// The image with every colour inverted, as tight RGBA rows.
pub fn inverted(frame: &Frame) -> Frame {
    let mut data = frame.to_rgba();
    for px in data.chunks_exact_mut(4) {
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
        px[3] = 255;
    }
    rgba_frame(frame.size, frame.color.clone(), data, frame)
}

fn rgba_frame(size: PhysicalSize, color: ColorInfo, data: Vec<u8>, like: &Frame) -> Frame {
    Frame { size, bytes_per_row: size.width as usize * 4, format: PixelFormat::Rgba8, color, scale: like.scale, data }
}

/// How much to enlarge the image before a second reading, or `None` when the text is large
/// enough. An image in which nothing was read and that is small gets a second chance at 3×.
pub fn upscale_factor(raw: &RawText, frame: &Frame, preparation: &Preparation) -> Option<u32> {
    let pixels = frame.size.width as u64 * frame.size.height as u64;
    let factor = match median_line_height(raw) {
        Some(height) if height < preparation.small_text_px => {
            (preparation.target_text_px / height.max(1.0)).ceil().clamp(2.0, 4.0) as u32
        }
        Some(_) => return None,
        None if pixels <= 1_500_000 => 3,
        None => return None,
    };
    // An upscaled copy stays under about 24 million pixels.
    let budget = ((24_000_000.0 / pixels.max(1) as f64).sqrt().floor() as u32).min(factor);
    (budget >= 2).then_some(budget)
}

fn median_line_height(raw: &RawText) -> Option<f64> {
    let mut heights: Vec<f64> = raw.lines.iter().map(|l| l.bounds.height).filter(|h| *h > 0.0).collect();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(f64::total_cmp);
    Some(heights[heights.len() / 2])
}

/// Whether a second reading is better than the first: it read at least as much text, weighed by
/// the engine's confidence.
pub fn better(candidate: &RawText, current: &RawText) -> bool {
    let score = |raw: &RawText| -> f64 {
        raw.lines.iter().map(|l| l.text.chars().filter(|c| !c.is_whitespace()).count() as f64 * l.confidence).sum()
    };
    score(candidate) >= score(current)
}

/// Enlarges the image by a whole factor with Catmull-Rom interpolation, which keeps glyph edges
/// sharper than bilinear filtering does. The result is tight RGBA rows.
pub fn upscaled(frame: &Frame, factor: u32) -> Frame {
    let factor = factor.max(1);
    let (w, h) = (frame.size.width as usize, frame.size.height as usize);
    let (nw, nh) = (w * factor as usize, h * factor as usize);
    let src = frame.to_rgba();
    let taps = |n: usize, out: usize| -> Vec<([usize; 4], [f32; 4])> {
        (0..out)
            .map(|o| {
                let x = (o as f64 + 0.5) / factor as f64 - 0.5;
                let base = x.floor();
                let t = (x - base) as f32;
                let idx = |k: i64| (base as i64 + k).clamp(0, n as i64 - 1) as usize;
                ([idx(-1), idx(0), idx(1), idx(2)], catmull_rom(t))
            })
            .collect()
    };
    let (tx, ty) = (taps(w, nw), taps(h, nh));
    // Horizontal pass into floats, then the vertical pass into bytes.
    let mut wide = vec![0f32; nw * h * 4];
    for y in 0..h {
        let row = &src[y * w * 4..(y + 1) * w * 4];
        let out = &mut wide[y * nw * 4..(y + 1) * nw * 4];
        for (o, (idx, weights)) in tx.iter().enumerate() {
            for c in 0..4 {
                out[o * 4 + c] = (0..4).map(|k| row[idx[k] * 4 + c] as f32 * weights[k]).sum();
            }
        }
    }
    let mut data = vec![0u8; nw * nh * 4];
    for (o, (idx, weights)) in ty.iter().enumerate() {
        let out = &mut data[o * nw * 4..(o + 1) * nw * 4];
        for x in 0..nw * 4 {
            let v: f32 = (0..4).map(|k| wide[idx[k] * nw * 4 + x] * weights[k]).sum();
            out[x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    rgba_frame(PhysicalSize::new(nw as u32, nh as u32), frame.color.clone(), data, frame)
}

fn catmull_rom(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [-0.5 * t3 + t2 - 0.5 * t, 1.5 * t3 - 2.5 * t2 + 1.0, -1.5 * t3 + 2.0 * t2 + 0.5 * t, 0.5 * t3 - 0.5 * t2]
}
