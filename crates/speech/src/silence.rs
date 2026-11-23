//! The sound's level over time and what is read from it: which 10 ms frames are quiet, the
//! silences a cut can shorten, and the edges of words, which the model's times only place to
//! within a few tens of milliseconds.
//!
//! # Quiet
//!
//! A frame is quiet when its level is below a threshold set from the recording itself: a little
//! above the noise floor (the level of the quietest tenth of the frames), more so when the
//! speech stands far above that floor, and never above -40 dBFS, so steady music or a loud room
//! is not taken for silence. Sounds shorter than 50 ms in a quiet stretch (a click, a key) do
//! not break it. The level alone decides, so silences are found in a recording without speech
//! as well.

use crate::{Silence, Word, SAMPLE_RATE};

/// The length of one frame of the envelope.
pub const FRAME_MS: u64 = 10;
/// The shortest silence reported; the trim window offers 0.5, 1 and 2 s as thresholds.
pub const MIN_SILENCE_MS: u64 = 500;

const FRAME_SAMPLES: usize = (SAMPLE_RATE as u64 * FRAME_MS / 1000) as usize;
/// Sounds shorter than this inside a quiet stretch count as quiet.
const BLIP_FRAMES: usize = 5;
/// A quiet stretch this long ends a word.
const WORD_GAP_FRAMES: usize = 20;
/// How far back a word's start may move to the onset of its sound. The model's start of the first
/// word after a pause is often 0.3 s late.
const ONSET_BACK_FRAMES: usize = 60;
/// A quiet stretch this long before a sound is a pause, not the closure of a stop consonant
/// inside a word.
const PAUSE_FRAMES: usize = 8;
/// The highest threshold, in dBFS.
const CEILING_DB: f32 = -40.0;
/// The lowest noise floor, in dBFS; digital silence is treated as this level.
const FLOOR_DB: f32 = -90.0;

/// Which frames of a 16 kHz mono sound are voiced.
pub struct Envelope {
    voiced: Vec<bool>,
    /// `prefix[i]` is the number of voiced frames before frame `i`.
    prefix: Vec<u32>,
    duration_ms: u64,
}

impl Envelope {
    pub fn new(audio: &[f32]) -> Envelope {
        let levels: Vec<f32> = audio
            .chunks(FRAME_SAMPLES)
            .map(|frame| {
                let power = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
                10.0 * (power + 1e-12).log10()
            })
            .collect();
        let threshold = threshold(&levels);
        let mut voiced: Vec<bool> = levels.iter().map(|l| *l >= threshold).collect();
        drop_blips(&mut voiced);
        let mut prefix = Vec::with_capacity(voiced.len() + 1);
        let mut count = 0u32;
        prefix.push(0);
        for v in &voiced {
            count += u32::from(*v);
            prefix.push(count);
        }
        Envelope { voiced, prefix, duration_ms: audio.len() as u64 * 1000 / u64::from(SAMPLE_RATE) }
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// Whether any frame is voiced: false for a silent recording.
    pub fn any_voiced(&self) -> bool {
        self.prefix.last().is_some_and(|n| *n > 0)
    }

    fn frames(&self) -> usize {
        self.voiced.len()
    }

    fn frame(&self, ms: u64) -> usize {
        ((ms / FRAME_MS) as usize).min(self.frames())
    }

    fn voiced_at(&self, frame: usize) -> bool {
        self.voiced.get(frame).copied().unwrap_or(false)
    }

    /// The number of voiced frames between two times.
    pub fn voiced_between(&self, start_ms: u64, end_ms: u64) -> u32 {
        let (a, b) = (self.frame(start_ms), self.frame(end_ms));
        if b <= a {
            0
        } else {
            self.prefix[b] - self.prefix[a]
        }
    }

    /// The window of `length_ms` with the most voiced frames, for reading the language from the
    /// part of the recording where most is said.
    pub fn busiest(&self, length_ms: u64) -> (u64, u64) {
        let length = (length_ms / FRAME_MS) as usize;
        if self.frames() <= length {
            return (0, self.duration_ms);
        }
        let step = (1000 / FRAME_MS) as usize;
        let mut best = (0, 0u32);
        let mut start = 0;
        while start + length <= self.frames() {
            let n = self.prefix[start + length] - self.prefix[start];
            if n > best.1 {
                best = (start, n);
            }
            start += step;
        }
        let from = best.0 as u64 * FRAME_MS;
        (from, (from + length_ms).min(self.duration_ms))
    }

    /// A word's start moved onto its sound. The model places a word's start early (in the pause
    /// before it) or late (inside it): an early start moves forward to the first voiced frame
    /// before `limit_ms`; a late one moves back to the onset of the sound it falls in, when that
    /// onset follows a pause, lies at most 600 ms back and not before `floor_ms`. A cut that
    /// starts at the word then takes all of it.
    pub fn snap_start(&self, start_ms: u64, limit_ms: u64, floor_ms: u64) -> u64 {
        let f = self.frame(start_ms);
        let limit = self.frame(limit_ms).max(f + 1).min(self.frames());
        if !self.voiced_at(f) {
            return (f..limit).find(|i| self.voiced_at(*i)).map_or(start_ms, |i| i as u64 * FRAME_MS);
        }
        let back = f.saturating_sub(ONSET_BACK_FRAMES).max(self.frame(floor_ms));
        let mut onset = f;
        while onset > back && self.voiced_at(onset - 1) {
            onset -= 1;
        }
        if onset == 0 {
            // The recording starts with this word.
            return 0;
        }
        if onset == back {
            return start_ms;
        }
        let pause = (0..onset).rev().take(PAUSE_FRAMES).take_while(|i| !self.voiced_at(*i)).count();
        if pause >= PAUSE_FRAMES || pause == onset {
            onset as u64 * FRAME_MS
        } else {
            start_ms
        }
    }

    /// Where a word that starts at `start_ms` ends: at the first quiet stretch of 200 ms after its
    /// sound, and never after `limit_ms` (the next word's start). A word without any sound before
    /// the limit keeps a quarter of a second.
    pub fn word_end(&self, start_ms: u64, limit_ms: u64) -> u64 {
        let limit_ms = limit_ms.max(start_ms + FRAME_MS);
        let s = self.frame(start_ms);
        let limit = self.frame(limit_ms);
        let (mut last_voiced, mut quiet) = (None, 0);
        for i in s..limit {
            if self.voiced_at(i) {
                last_voiced = Some(i);
                quiet = 0;
            } else {
                quiet += 1;
                if quiet >= WORD_GAP_FRAMES && last_voiced.is_some() {
                    break;
                }
            }
        }
        match last_voiced {
            Some(v) => ((v as u64 + 1) * FRAME_MS).clamp(start_ms + FRAME_MS, limit_ms),
            None => limit_ms.min(start_ms + 250),
        }
    }

    /// The voiced stretches between two times, as `(start_ms, end_ms)`.
    pub fn voiced_runs(&self, start_ms: u64, end_ms: u64) -> Vec<(u64, u64)> {
        let (a, b) = (self.frame(start_ms), self.frame(end_ms));
        let mut runs = Vec::new();
        let mut open = None;
        for i in a..b {
            match (self.voiced_at(i), open) {
                (true, None) => open = Some(i),
                (false, Some(o)) => {
                    runs.push((o as u64 * FRAME_MS, i as u64 * FRAME_MS));
                    open = None;
                }
                _ => {}
            }
        }
        if let Some(o) = open {
            runs.push((o as u64 * FRAME_MS, b as u64 * FRAME_MS));
        }
        runs
    }

    /// The quiet stretches of at least `min_ms` that no word overlaps. `words` are in order.
    pub fn silences(&self, words: &[Word], min_ms: u64) -> Vec<Silence> {
        let mut out = Vec::new();
        let mut words = words.iter().peekable();
        let mut i = 0;
        while i < self.frames() {
            if self.voiced[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < self.frames() && !self.voiced[i] {
                i += 1;
            }
            let (run_start, run_end) = (start as u64 * FRAME_MS, (i as u64 * FRAME_MS).min(self.duration_ms));
            // A word inside a quiet stretch (a soft one, or one placed a little off) splits it.
            let mut from = run_start;
            while let Some(w) = words.peek() {
                if w.end_ms <= from {
                    words.next();
                    continue;
                }
                if w.start_ms >= run_end {
                    break;
                }
                if w.start_ms > from && w.start_ms - from >= min_ms {
                    out.push(Silence { start_ms: from, end_ms: w.start_ms });
                }
                from = from.max(w.end_ms);
                if w.end_ms >= run_end {
                    // It may reach into the next quiet stretch as well.
                    break;
                }
                words.next();
            }
            if run_end > from && run_end - from >= min_ms {
                out.push(Silence { start_ms: from, end_ms: run_end });
            }
        }
        out
    }
}

/// The silences of 16 kHz mono `audio` of at least half a second that none of `words` overlaps,
/// from the sound's level alone; `words` may be empty.
pub fn find_silences(audio: &[f32], words: &[Word]) -> Vec<Silence> {
    Envelope::new(audio).silences(words, MIN_SILENCE_MS)
}

/// The level below which a frame is quiet; see the module's notes.
fn threshold(levels: &[f32]) -> f32 {
    if levels.is_empty() {
        return 0.0;
    }
    let mut sorted = levels.to_vec();
    sorted.sort_unstable_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10].max(FLOOR_DB);
    // The speech's level is read from the frames that stand out of the floor, so a recording that
    // is mostly pauses still has its speech measured.
    let above = sorted.partition_point(|l| *l < floor + 6.0);
    let margin = if above < sorted.len() {
        let loud = &sorted[above..];
        let level = loud[(loud.len() * 95 / 100).min(loud.len() - 1)];
        ((level - floor) / 3.0).clamp(6.0, 18.0)
    } else {
        6.0
    };
    (floor + margin).min(CEILING_DB)
}

/// Clears voiced stretches shorter than [`BLIP_FRAMES`].
fn drop_blips(voiced: &mut [bool]) {
    let mut i = 0;
    while i < voiced.len() {
        if !voiced[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < voiced.len() && voiced[i] {
            i += 1;
        }
        if i - start < BLIP_FRAMES {
            voiced[start..i].fill(false);
        }
    }
}
