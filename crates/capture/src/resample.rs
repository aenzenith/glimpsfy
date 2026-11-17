//! Scaling a frame down to one pixel per point (Settings › Output › Retina: scale to 1×).
//!
//! The filter is an area average: every output pixel is the mean of the source area it covers,
//! each source pixel weighted by how much of it lies inside. At 2× that is the exact 2 × 2 box
//! macOS itself uses for a non-Retina screen, and at fractional scales (1.25×, 1.5×) a source
//! pixel split between two outputs counts in both by its share. An area average never rings
//! around text the way Lanczos does, and it never drops a row the way nearest-neighbour does.
//!
//! Colours are averaged with premultiplied alpha, so the transparent pixels around a window's
//! shadow never bleed their (meaningless) colour into the edge. The averaging runs on the stored
//! values, as every screenshot tool and the system do, not in linear light.

use crate::frame::Frame;
use crate::geometry::{PhysicalSize, ScaleFactor};

/// For one output index: the first source index it covers and the weight of each source index
/// from there, summing to 1.
struct Span {
    start: usize,
    weights: Vec<f32>,
}

fn spans(source: u32, target: u32) -> Vec<Span> {
    let ratio = source as f64 / target as f64;
    (0..target)
        .map(|o| {
            let from = o as f64 * ratio;
            let to = ((o + 1) as f64 * ratio).min(source as f64);
            let first = from.floor() as usize;
            let last = (to.ceil() as usize).clamp(first + 1, source as usize);
            let weights = (first..last)
                .map(|i| {
                    let overlap = (to.min((i + 1) as f64) - from.max(i as f64)).max(0.0);
                    (overlap / (to - from)) as f32
                })
                .collect();
            Span { start: first, weights }
        })
        .collect()
}

impl Frame {
    /// The frame at `size`, by area average. Meant for shrinking; a larger `size` repeats pixels.
    pub fn resized(&self, size: PhysicalSize) -> Frame {
        let size = PhysicalSize::new(size.width.max(1), size.height.max(1));
        let (sw, sh) = (self.size.width as usize, self.size.height as usize);
        let (tw, th) = (size.width as usize, size.height as usize);
        let opaque = self.is_opaque();

        // Premultiplied floats, one row of the source at a time, averaged across first.
        let columns = spans(self.size.width, size.width);
        let mut across = vec![0f32; tw * sh * 4];
        let mut row = vec![0f32; sw * 4];
        for y in 0..sh {
            let src = &self.data[y * self.bytes_per_row..y * self.bytes_per_row + sw * 4];
            for (dst, p) in row.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                let a = p[3] as f32;
                let k = if opaque { 1.0 } else { a / 255.0 };
                dst[0] = p[0] as f32 * k;
                dst[1] = p[1] as f32 * k;
                dst[2] = p[2] as f32 * k;
                dst[3] = a;
            }
            let out = &mut across[y * tw * 4..(y + 1) * tw * 4];
            for (x, span) in columns.iter().enumerate() {
                let mut sum = [0f32; 4];
                for (i, w) in span.weights.iter().enumerate() {
                    let p = &row[(span.start + i) * 4..(span.start + i) * 4 + 4];
                    for c in 0..4 {
                        sum[c] += p[c] * w;
                    }
                }
                out[x * 4..x * 4 + 4].copy_from_slice(&sum);
            }
        }

        let rows = spans(self.size.height, size.height);
        let mut data = vec![0u8; tw * th * 4];
        for (y, span) in rows.iter().enumerate() {
            let out = &mut data[y * tw * 4..(y + 1) * tw * 4];
            for x in 0..tw {
                let mut sum = [0f32; 4];
                for (i, w) in span.weights.iter().enumerate() {
                    let at = ((span.start + i) * tw + x) * 4;
                    for c in 0..4 {
                        sum[c] += across[at + c] * w;
                    }
                }
                let a = sum[3];
                let k = if opaque || a >= 255.0 {
                    1.0
                } else if a > 0.0 {
                    255.0 / a
                } else {
                    0.0
                };
                let px = &mut out[x * 4..x * 4 + 4];
                for c in 0..3 {
                    px[c] = (sum[c] * k).round().clamp(0.0, 255.0) as u8;
                }
                px[3] = a.round().clamp(0.0, 255.0) as u8;
            }
        }

        Frame {
            size,
            bytes_per_row: tw * 4,
            format: self.format,
            color: self.color.clone(),
            scale: ScaleFactor(self.scale.0 * size.width as f64 / self.size.width.max(1) as f64),
            data,
        }
    }

    /// The frame at one pixel per point: half the size of a 2× capture, two thirds of a 1.5×
    /// one. Each side becomes its point length rounded to the nearest pixel. `None` when the frame
    /// is already at 1× or below, so a 1× display's capture is never resampled.
    pub fn at_1x(&self) -> Option<Frame> {
        let scale = self.scale.0;
        if scale.is_nan() || scale <= 1.0 + 1e-6 {
            return None;
        }
        let side = |pixels: u32| ((pixels as f64 / scale).round() as u32).max(1);
        let size = PhysicalSize::new(side(self.size.width), side(self.size.height));
        let mut frame = self.resized(size);
        frame.scale = ScaleFactor::ONE;
        Some(frame)
    }
}
