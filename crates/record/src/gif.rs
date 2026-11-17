//! The GIF encoder of the export: NeuQuant palettes with Floyd–Steinberg dithering, and frames
//! that carry only the part of the picture that changed.
//!
//! A screen recording is mostly still between frames, so each frame is cropped to the box of
//! pixels that changed, and inside the box an unchanged pixel is transparent, which lets the
//! previous frame show through (disposal "keep"). Each frame gets its own palette of 255
//! colours, trained by NeuQuant (`color_quant`) on the changed pixels only; index 255 is the
//! transparent one. `gifski` and `imagequant` are not used: their licences do not fit an MIT app.

use std::io::Write;

use color_quant::NeuQuant;

use crate::{RecordError, Result};

const TRANSPARENT: u8 = 255;
/// Two pixels closer than this (sum of channel differences) count as the same, so the noise a
/// video decoder adds to still areas does not make every pixel "changed".
const SAME: u32 = 6;

/// An RGBA picture of the GIF's size, rows packed.
pub struct Picture<'a> {
    pub rgba: &'a [u8],
}

pub struct GifEncoder<W: Write> {
    encoder: gif::Encoder<W>,
    width: u16,
    height: u16,
    /// The picture the viewer shows after the last written frame, as the source pixels it
    /// came from.
    shown: Option<Vec<u8>>,
    /// The frame waiting for its delay, which is known once the next frame's time is.
    pending: Option<(Vec<u8>, u64)>,
    /// Centiseconds already handed out, so rounding never drifts from the real time.
    written_cs: u64,
}

impl<W: Write> GifEncoder<W> {
    pub fn new(out: W, width: u32, height: u32, loop_forever: bool) -> Result<GifEncoder<W>> {
        let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
            return Err(RecordError::InvalidConfig(format!("a GIF cannot be {width}×{height} pixels")));
        };
        if w == 0 || h == 0 {
            return Err(RecordError::InvalidConfig("the GIF has no pixels".into()));
        }
        let mut encoder = gif::Encoder::new(out, w, h, &[]).map_err(gif_error)?;
        if loop_forever {
            encoder.set_repeat(gif::Repeat::Infinite).map_err(gif_error)?;
        }
        Ok(GifEncoder { encoder, width: w, height: h, shown: None, pending: None, written_cs: 0 })
    }

    /// Adds a picture shown from `at_ns` (from the start of the GIF) until the next one.
    pub fn push(&mut self, picture: Picture<'_>, at_ns: u64) -> Result<()> {
        let expected = self.width as usize * self.height as usize * 4;
        if picture.rgba.len() != expected {
            return Err(RecordError::Encoder(format!(
                "a GIF frame has {} bytes instead of {expected}",
                picture.rgba.len()
            )));
        }
        if let Some((rgba, _)) = self.pending.take() {
            self.write(&rgba, at_ns)?;
        }
        self.pending = Some((picture.rgba.to_vec(), at_ns));
        Ok(())
    }

    /// Writes the last frame, shown until `end_ns`, and the GIF's trailer.
    pub fn finish(mut self, end_ns: u64) -> Result<W> {
        if let Some((rgba, start)) = self.pending.take() {
            self.write(&rgba, end_ns.max(start + 10_000_000))?;
        }
        self.encoder.into_inner().map_err(gif_error)
    }

    /// Writes `rgba`, shown until `end_ns`.
    fn write(&mut self, rgba: &[u8], end_ns: u64) -> Result<()> {
        let end_cs = end_ns / 10_000_000;
        // A GIF delay is whole centiseconds, and most viewers slow frames under 2 cs down.
        let delay = end_cs.saturating_sub(self.written_cs).max(2);
        let (w, h) = (self.width as usize, self.height as usize);
        let changed = |i: usize| match &self.shown {
            None => true,
            Some(prev) => {
                let d = (prev[i] as i32 - rgba[i] as i32).unsigned_abs()
                    + (prev[i + 1] as i32 - rgba[i + 1] as i32).unsigned_abs()
                    + (prev[i + 2] as i32 - rgba[i + 2] as i32).unsigned_abs();
                d > SAME
            }
        };
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if changed((y * w + x) * 4) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x1 <= x0 || y1 <= y0 {
            // Nothing changed: one transparent pixel keeps the timing.
            (x0, y0, x1, y1) = (0, 0, 1, 1);
        }
        let (bw, bh) = (x1 - x0, y1 - y0);

        let mut mask = vec![false; bw * bh];
        let mut sample = Vec::with_capacity(bw * bh * 4);
        for y in 0..bh {
            for x in 0..bw {
                let i = ((y0 + y) * w + x0 + x) * 4;
                if changed(i) {
                    mask[y * bw + x] = true;
                    sample.extend_from_slice(&[rgba[i], rgba[i + 1], rgba[i + 2], 255]);
                }
            }
        }
        if sample.is_empty() {
            sample.extend_from_slice(&[0, 0, 0, 255]);
        }
        let samplefac = if sample.len() / 4 < 4096 { 1 } else { 10 };
        let quant = NeuQuant::new(samplefac, 255, &sample);
        let mut palette = quant.color_map_rgb();
        palette.resize(256 * 3, 0);

        // Floyd–Steinberg over the changed pixels; errors spread only to changed neighbours.
        let mut errors = vec![[0f32; 3]; (bw + 2) * 2];
        let mut indices = vec![TRANSPARENT; bw * bh];
        for y in 0..bh {
            let (cur, next) = errors.split_at_mut(bw + 2);
            for e in next.iter_mut() {
                *e = [0.0; 3];
            }
            for x in 0..bw {
                if !mask[y * bw + x] {
                    continue;
                }
                let i = ((y0 + y) * w + x0 + x) * 4;
                let err = cur[x + 1];
                let want = [
                    (rgba[i] as f32 + err[0]).clamp(0.0, 255.0),
                    (rgba[i + 1] as f32 + err[1]).clamp(0.0, 255.0),
                    (rgba[i + 2] as f32 + err[2]).clamp(0.0, 255.0),
                ];
                let pixel = [want[0].round() as u8, want[1].round() as u8, want[2].round() as u8, 255];
                let index = quant.index_of(&pixel).min(254);
                indices[y * bw + x] = index as u8;
                let got = &palette[index * 3..index * 3 + 3];
                let diff = [want[0] - got[0] as f32, want[1] - got[1] as f32, want[2] - got[2] as f32];
                let right = x + 1 < bw && mask[y * bw + x + 1];
                for c in 0..3 {
                    if right {
                        cur[x + 2][c] += diff[c] * 7.0 / 16.0;
                    }
                    next[x][c] += diff[c] * 3.0 / 16.0;
                    next[x + 1][c] += diff[c] * 5.0 / 16.0;
                    next[x + 2][c] += diff[c] / 16.0;
                }
            }
            let (cur, next) = errors.split_at_mut(bw + 2);
            cur.copy_from_slice(next);
        }

        let frame = gif::Frame {
            delay: delay.min(u16::MAX as u64) as u16,
            dispose: gif::DisposalMethod::Keep,
            transparent: Some(TRANSPARENT),
            left: x0 as u16,
            top: y0 as u16,
            width: bw as u16,
            height: bh as u16,
            palette: Some(palette),
            buffer: std::borrow::Cow::Owned(indices),
            ..gif::Frame::default()
        };
        self.encoder.write_frame(&frame).map_err(gif_error)?;
        self.written_cs += delay;

        let shown = self.shown.get_or_insert_with(|| rgba.to_vec());
        for y in 0..bh {
            for x in 0..bw {
                if mask[y * bw + x] {
                    let i = ((y0 + y) * w + x0 + x) * 4;
                    shown[i..i + 4].copy_from_slice(&rgba[i..i + 4]);
                }
            }
        }
        Ok(())
    }
}

fn gif_error(e: gif::EncodingError) -> RecordError {
    match e {
        gif::EncodingError::Io(e) => RecordError::Io(e),
        other => RecordError::Encoder(other.to_string()),
    }
}
