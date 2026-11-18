//! The take's audio: every source is brought to 48 kHz stereo, placed on the file's timeline
//! without clicks, and mixed into one track that plays everywhere.
//!
//! Positions are counted in frames (one sample per channel) at [`RATE`] from media time 0.
//! A source writes contiguously as long as its buffers arrive where the timeline expects them;
//! when a buffer comes more than [`SLACK_FRAMES`] late or early (a microphone that started late,
//! a dropout, a resume), the gap is filled with silence or the overlap is dropped, so the audio
//! never drifts from the video.

use std::collections::VecDeque;

/// Every audio track of a recording is 48 kHz stereo.
pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// How far a buffer may land from the end of its source's audio before it is moved: 40 ms.
pub const SLACK_FRAMES: u64 = RATE as u64 / 25;
/// How long the mix waits for a source that has nothing yet, before treating it as silence.
const MIX_HOLD_FRAMES: u64 = RATE as u64 / 4;

/// The position in frames of media time `ns`.
pub fn frames_at(ns: u64) -> u64 {
    (ns as u128 * RATE as u128 / 1_000_000_000) as u64
}

/// The media time in nanoseconds of frame position `frames`.
pub fn ns_at(frames: u64) -> u64 {
    (frames as u128 * 1_000_000_000 / RATE as u128) as u64
}

/// Converts one source's buffers to 48 kHz stereo. Rate conversion is linear and keeps its
/// phase across buffers, which is plenty for speech and system sounds.
#[derive(Debug, Default)]
pub struct Conformer {
    rate: u32,
    /// Where the next output frame falls between the last input frame and the next one.
    phase: f64,
    last: [f32; 2],
    primed: bool,
}

impl Conformer {
    pub fn new() -> Conformer {
        Conformer::default()
    }

    /// Returns `samples` (interleaved, `channels` per frame, at `rate`) as interleaved stereo at
    /// [`RATE`]. Mono is copied to both sides; channels past the second are dropped.
    pub fn conform(&mut self, samples: &[f32], channels: u16, rate: u32) -> Vec<f32> {
        let channels = channels.max(1) as usize;
        let stereo = |frame: &[f32]| -> [f32; 2] {
            match frame {
                [m] => [*m, *m],
                [l, r, ..] => [*l, *r],
                [] => [0.0, 0.0],
            }
        };
        let frames = samples.chunks_exact(channels).map(stereo);
        if rate == RATE || rate == 0 {
            return frames.flatten().collect();
        }
        if rate != self.rate {
            *self = Conformer { rate, ..Conformer::default() };
        }
        let step = rate as f64 / RATE as f64;
        let mut out = Vec::with_capacity(samples.len() / channels * 2 * RATE as usize / rate as usize + 4);
        for frame in frames {
            if !self.primed {
                self.last = frame;
                self.primed = true;
                continue;
            }
            while self.phase < 1.0 {
                let p = self.phase as f32;
                out.push(self.last[0] + (frame[0] - self.last[0]) * p);
                out.push(self.last[1] + (frame[1] - self.last[1]) * p);
                self.phase += step;
            }
            self.phase -= 1.0;
            self.last = frame;
        }
        out
    }
}

/// Keeps one source contiguous on the timeline.
#[derive(Debug, Default)]
pub struct Placer {
    next: Option<u64>,
}

impl Placer {
    pub fn new() -> Placer {
        Placer::default()
    }

    /// Where the source's audio ends so far, in frames.
    pub fn end(&self) -> u64 {
        self.next.unwrap_or(0)
    }

    /// Places a buffer whose first frame belongs at `at` frames. Returns the position to write
    /// it at, the silence to write before it, and how many frames to drop from its start.
    pub fn place(&mut self, at: u64, frames: u64) -> Placement {
        let Some(next) = self.next else {
            // A source that starts late begins its track with silence from 0, the same way a
            // gap later on is filled, so the buffer itself still lands at `at`.
            self.next = Some(at + frames);
            return Placement { position: 0, silence: at, skip: 0 };
        };
        let placement = if at > next + SLACK_FRAMES {
            Placement { position: next, silence: at - next, skip: 0 }
        } else if at + SLACK_FRAMES < next {
            let skip = (next - at).min(frames);
            Placement { position: next, silence: 0, skip }
        } else {
            Placement { position: next, silence: 0, skip: 0 }
        };
        self.next = Some(placement.position + placement.silence + frames - placement.skip);
        placement
    }
}

/// Where [`Placer::place`] puts a buffer: `silence` frames of silence go first at `position`,
/// then the buffer without its first `skip` frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub position: u64,
    pub silence: u64,
    pub skip: u64,
}

/// Mixes the sources in real time. Each source adds its frames at their position; the mix is
/// handed out once every active source has passed a position, or once the lagging one is more
/// than a quarter second behind, in which case it counts as silent there.
#[derive(Debug)]
pub struct Mixer {
    base: u64,
    sum: VecDeque<f32>,
    ends: Vec<u64>,
}

impl Mixer {
    pub fn new(sources: usize) -> Mixer {
        Mixer { base: 0, sum: VecDeque::new(), ends: vec![0; sources] }
    }

    /// Where the mix has been handed out up to, in frames.
    pub fn emitted(&self) -> u64 {
        self.base
    }

    /// Adds `samples` (stereo) of `source` at frame `at`. Frames already handed out are lost to
    /// the mix (they still reach the source's own track).
    pub fn add(&mut self, source: usize, at: u64, samples: &[f32]) {
        let frames = (samples.len() / CHANNELS) as u64;
        let end = at + frames;
        if let Some(e) = self.ends.get_mut(source) {
            *e = (*e).max(end);
        }
        if end <= self.base {
            return;
        }
        let needed = ((end - self.base) as usize) * CHANNELS;
        if self.sum.len() < needed {
            self.sum.resize(needed, 0.0);
        }
        let first = self.base.saturating_sub(at) as usize;
        for (i, frame) in samples.chunks_exact(CHANNELS).enumerate().skip(first) {
            let index = (at + i as u64 - self.base) as usize * CHANNELS;
            self.sum[index] += frame[0];
            self.sum[index + 1] += frame[1];
        }
    }

    /// Hands out what is ready, as `(position, samples)`; with `flush`, hands out everything.
    pub fn take(&mut self, flush: bool) -> Option<(u64, Vec<f32>)> {
        let latest = self.ends.iter().copied().max().unwrap_or(0);
        let earliest = self.ends.iter().copied().min().unwrap_or(0);
        let ready = if flush { latest } else { earliest.max(latest.saturating_sub(MIX_HOLD_FRAMES)) };
        if ready <= self.base {
            return None;
        }
        let count = ((ready - self.base) as usize * CHANNELS).min(self.sum.len());
        let mut out: Vec<f32> = self.sum.drain(..count).collect();
        out.resize((ready - self.base) as usize * CHANNELS, 0.0);
        for s in &mut out {
            // A soft limit: two loud sources summed would clip, and a hard clip crackles.
            *s = if s.abs() <= 0.8 { *s } else { s.signum() * (0.8 + 0.2 * ((s.abs() - 0.8) / 0.2).tanh()) };
        }
        let at = self.base;
        self.base = ready;
        Some((at, out))
    }
}

/// The peak of `samples` as a 0–1 level for a meter, on a decibel scale from −60 dB.
pub fn level(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |p, s| p.max(s.abs()));
    if peak <= 0.001 {
        return 0.0;
    }
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer's sound lands at `position + silence` on its track. A source heard first half a
    /// second into the take lands there, with silence before it from 0, and its next buffer
    /// follows it without overwriting its tail.
    #[test]
    fn a_late_source_lands_where_it_was_heard() {
        let mut placer = Placer::new();
        let late = frames_at(500_000_000);
        let first = placer.place(late, 480);
        assert_eq!(first, Placement { position: 0, silence: late, skip: 0 });
        assert_eq!(placer.place(late + 480, 480), Placement { position: late + 480, silence: 0, skip: 0 });
        assert_eq!(placer.end(), late + 960);

        let mut placer = Placer::new();
        assert_eq!(placer.place(0, 480), Placement { position: 0, silence: 0, skip: 0 });
    }
}
