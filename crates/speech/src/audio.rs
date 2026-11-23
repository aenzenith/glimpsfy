//! Bringing a recording's sound to the model's format: 16 kHz mono `f32`, converted while the
//! sound is decoded, so a long recording is never held at its own rate.
//!
//! The conversion is a polyphase resampler: the input is mixed to mono, conceptually upsampled by
//! `L`, low-passed by a Kaiser-windowed sinc and downsampled by `M` (`48 kHz → 16 kHz` is `L = 1`,
//! `M = 3`). The low-pass keeps everything below 7.2 kHz and removes what would fold back from
//! above 8 kHz, which plain decimation would alias into the speech band. The filter's delay is
//! compensated, so sample `n` of the output is the moment `n / 16 000` s of the input and word
//! times stay in the recording's milliseconds.

use crate::SAMPLE_RATE;

/// Stopband attenuation of the low-pass, in dB. Aliases 60 dB down are far below what a
/// microphone's own noise leaves in the speech band.
const ATTENUATION_DB: f64 = 60.0;
/// The share of the output's Nyquist frequency that passes untouched; the rest is the transition.
const PASSBAND: f64 = 0.9;

/// Converts interleaved `f32` sound of any rate and channel count to 16 kHz mono, buffer by
/// buffer. [`Downsampler::new`] takes the recorder's format, 48 kHz stereo.
pub struct Downsampler {
    /// The input's format, kept to start over after [`Downsampler::finish`].
    rate: u32,
    channels: usize,
    /// Upsampling factor `L` and downsampling factor `M` of the rational ratio.
    up: usize,
    down: usize,
    /// Taps per phase; the prototype filter has `up * taps` coefficients.
    taps: usize,
    /// For each phase, its coefficients in the order of the input window (oldest first), scaled
    /// by `up`. Empty when the rate already matches and the samples are copied.
    phases: Vec<f32>,
    /// The filter's delay in upsampled samples, skipped so the output lines up with the input.
    delay: u64,
    /// Mono input from absolute index `history_start` on; starts with `taps - 1` zeros of the time
    /// before the first sample.
    history: Vec<f32>,
    history_start: i64,
    /// Mono input samples received so far.
    received: u64,
    /// The index of the next output sample.
    produced: u64,
    /// The samples of a frame that a buffer cut in the middle.
    partial: Vec<f32>,
}

impl Default for Downsampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Downsampler {
    /// A converter for the recorder's sound: 48 kHz, two channels interleaved.
    pub fn new() -> Self {
        Self::with_input(48_000, 2)
    }

    /// A converter for interleaved sound of `rate` Hz with `channels` channels.
    pub fn with_input(rate: u32, channels: u16) -> Self {
        let input_rate = rate.max(1);
        let rate = input_rate as usize;
        let target = SAMPLE_RATE as usize;
        let g = gcd(rate, target);
        let (up, down) = (target / g, rate / g);
        let channels = usize::from(channels.max(1));
        if up == 1 && down == 1 {
            return Downsampler {
                rate: input_rate,
                channels,
                up,
                down,
                taps: 1,
                phases: Vec::new(),
                delay: 0,
                history: Vec::new(),
                history_start: 0,
                received: 0,
                produced: 0,
                partial: Vec::new(),
            };
        }

        // The prototype runs at the upsampled rate; its band edge is the lower Nyquist frequency.
        let upsampled = (rate * up) as f64;
        let nyquist = rate.min(target) as f64 / 2.0;
        let pass = nyquist * PASSBAND;
        let transition = (nyquist - pass) / upsampled;
        let cutoff = (pass + nyquist) / 2.0 / upsampled;
        // Kaiser's estimate of the length that reaches the attenuation over the transition.
        let length = ((ATTENUATION_DB - 7.95) / (14.36 * transition)).ceil() as usize + 1;
        let mut taps = length.div_ceil(up);
        if taps.is_multiple_of(2) {
            taps += 1;
        }
        let n = taps * up;
        let beta = 0.1102 * (ATTENUATION_DB - 8.7);
        let centre = (n - 1) as f64 / 2.0;
        let mut h: Vec<f64> = (0..n)
            .map(|k| {
                let x = k as f64 - centre;
                let sinc = if x == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let r = x / centre.max(1.0);
                sinc * bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta)
            })
            .collect();
        // Unity gain at DC for every phase once the upsampling's zeros are made up for.
        let sum: f64 = h.iter().sum();
        for c in &mut h {
            *c *= up as f64 / sum;
        }
        let mut phases = vec![0f32; up * taps];
        for p in 0..up {
            for w in 0..taps {
                // Window slot `w` holds input `j0 - (taps - 1 - w)`, whose coefficient is
                // `h[p + (taps - 1 - w) * up]`.
                phases[p * taps + w] = h[p + (taps - 1 - w) * up] as f32;
            }
        }
        Downsampler {
            rate: input_rate,
            channels,
            up,
            down,
            taps,
            phases,
            delay: ((n - 1) / 2) as u64,
            history: vec![0.0; taps - 1],
            history_start: -(taps as i64 - 1),
            received: 0,
            produced: 0,
            partial: Vec::new(),
        }
    }

    /// Converts the next buffer of interleaved samples, appending 16 kHz mono samples to `out`. A
    /// frame split between two buffers is joined.
    pub fn push(&mut self, interleaved: &[f32], out: &mut Vec<f32>) {
        let mut rest = interleaved;
        if !self.partial.is_empty() {
            let need = (self.channels - self.partial.len()).min(rest.len());
            self.partial.extend_from_slice(&rest[..need]);
            rest = &rest[need..];
            if self.partial.len() < self.channels {
                return;
            }
            let frame = std::mem::take(&mut self.partial);
            self.mono(&frame, out);
        }
        let whole = rest.len() - rest.len() % self.channels;
        self.mono(&rest[..whole], out);
        self.partial.extend_from_slice(&rest[whole..]);
    }

    /// Ends the sound: flushes what the filter still holds, so the output is as long as the input
    /// in time, and starts over for a next sound.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.phases.is_empty() {
            self.partial.clear();
            return;
        }
        let total = (self.received * self.up as u64).div_ceil(self.down as u64);
        // Zeros after the end let the last outputs see the whole filter.
        let padding = self.taps + (self.delay as usize) / self.up + 1;
        self.history.extend(std::iter::repeat_n(0.0, padding));
        self.received += padding as u64;
        self.produce(out, Some(total));
        debug_assert_eq!(self.produced, total);
        *self = Self::with_input(self.rate, self.channels as u16);
    }

    /// Mixes whole frames to mono and converts them.
    fn mono(&mut self, frames: &[f32], out: &mut Vec<f32>) {
        if frames.is_empty() {
            return;
        }
        let scale = 1.0 / self.channels as f32;
        let mixed = frames.chunks_exact(self.channels).map(|f| f.iter().sum::<f32>() * scale);
        if self.phases.is_empty() {
            out.extend(mixed);
            return;
        }
        self.history.extend(mixed);
        self.received += (frames.len() / self.channels) as u64;
        self.produce(out, None);
    }

    /// Writes every output whose input window is complete, up to `limit` outputs in all.
    fn produce(&mut self, out: &mut Vec<f32>, limit: Option<u64>) {
        let (up, down, taps) = (self.up as u64, self.down as u64, self.taps);
        loop {
            if limit.is_some_and(|l| self.produced >= l) {
                break;
            }
            let t = self.produced * down + self.delay;
            let newest = t / up;
            if newest >= self.received {
                break;
            }
            let phase = (t % up) as usize;
            let end = (newest as i64 - self.history_start) as usize + 1;
            let window = &self.history[end - taps..end];
            out.push(dot(&self.phases[phase * taps..(phase + 1) * taps], window));
            self.produced += 1;
        }
        // Drop input no later output can reach, in large steps so the copy stays rare.
        let next_oldest = ((self.produced * down + self.delay) / up) as i64 - (taps as i64 - 1);
        let droppable = (next_oldest - self.history_start).max(0) as usize;
        if droppable >= 1 << 16 {
            self.history.drain(..droppable);
            self.history_start += droppable as i64;
        }
    }
}

/// A dot product in eight lanes, which the compiler turns into vector instructions; a plain fold
/// stays scalar because float addition is not reordered.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            lanes[i] += x[i] * y[i];
        }
    }
    let mut sum: f32 = lanes.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        sum += x * y;
    }
    sum
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The modified Bessel function of the first kind and order zero, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    let half = x / 2.0;
    while term > sum * 1e-12 {
        term *= (half / k) * (half / k);
        sum += term;
        k += 1.0;
    }
    sum
}
