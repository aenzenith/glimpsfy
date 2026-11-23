//! The parts of a recording an export with cuts keeps: the cuts sorted, merged and clamped to the
//! trim, each kept part with its place in the exported file, the cut edges moved to the quietest
//! moment nearby so a join falls between sounds, and the fade that keeps a join from clicking.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::{Result, TrimRange};

/// A kept part shorter than this, about one frame, goes into the cut around it: it would only
/// flash one picture and click.
pub const MIN_PART_MS: u64 = 40;
/// How far a cut edge may move to find a quiet moment.
pub const SNAP_MS: u64 = 120;
/// The window whose loudness is compared while an edge looks for a quiet moment.
pub const QUIET_WINDOW_MS: u64 = 10;
/// The fade on each side of a join: the part before it fades out over its last 40 ms and the
/// part after it fades in over its first.
pub const RAMP_MS: u64 = 40;

/// One kept part: where it lies in the recording and where it starts in the exported file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub source: TrimRange,
    pub output_start_ms: u64,
}

impl Span {
    pub fn len_ms(&self) -> u64 {
        self.source.end_ms.saturating_sub(self.source.start_ms)
    }
}

/// Lays `parts` (start and end in the recording, in order) end to end in the exported file.
fn end_to_end(parts: impl IntoIterator<Item = (u64, u64)>) -> Vec<Span> {
    let mut output_start_ms = 0;
    parts
        .into_iter()
        .map(|(start_ms, end_ms)| {
            let span = Span { source: TrimRange { start_ms, end_ms }, output_start_ms };
            output_start_ms += end_ms - start_ms;
            span
        })
        .collect()
}

/// The parts of `trim` that `cuts` leave, in order, each with its start in the exported file.
/// The cuts may be unsorted, overlap, touch or reach outside the trim. Without a cut inside the
/// trim the trim itself is the one part; with one, a part shorter than [`MIN_PART_MS`] goes into
/// the cut, and when everything is cut no part is left.
pub fn kept_spans(trim: TrimRange, cuts: &[TrimRange]) -> Vec<Span> {
    let mut clamped: Vec<(u64, u64)> = cuts
        .iter()
        .map(|c| (c.start_ms.max(trim.start_ms), c.end_ms.min(trim.end_ms)))
        .filter(|(start, end)| start < end)
        .collect();
    if clamped.is_empty() {
        return end_to_end([(trim.start_ms, trim.end_ms)]);
    }
    clamped.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(clamped.len());
    for (start, end) in clamped {
        match merged.last_mut() {
            // Touching cuts merge too, so no empty part is left between them.
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let mut parts = Vec::with_capacity(merged.len() + 1);
    let mut from = trim.start_ms;
    for (start, end) in merged.into_iter().chain(std::iter::once((trim.end_ms, trim.end_ms))) {
        if start >= from + MIN_PART_MS {
            parts.push((from, start));
        }
        from = from.max(end);
    }
    end_to_end(parts)
}

/// Moves every cut edge inside the trim to the point `quietest(edge, lo, hi)` picks in
/// `lo..=hi`. An edge moves at most [`SNAP_MS`], at most half into the cut it borders (so a cut
/// never turns inside out) and at most half of what its part has beyond [`MIN_PART_MS`] (so a part
/// never gets shorter than that); the trim's own edges stay. The parts stay sorted, disjoint and
/// inside the trim whatever `quietest` answers.
pub fn snapped(
    trim: TrimRange,
    spans: &[Span],
    quietest: &mut dyn FnMut(u64, u64, u64) -> Result<u64>,
) -> Result<Vec<Span>> {
    let mut parts = Vec::with_capacity(spans.len());
    for (i, span) in spans.iter().enumerate() {
        let TrimRange { start_ms: a, end_ms: b } = span.source;
        let give = span.len_ms().saturating_sub(MIN_PART_MS) / 2;
        let before = if i == 0 { trim.start_ms } else { spans[i - 1].source.end_ms };
        let after = spans.get(i + 1).map_or(trim.end_ms, |next| next.source.start_ms);
        let start = if a > trim.start_ms {
            let (lo, hi) = (a - SNAP_MS.min((a - before) / 2), a + SNAP_MS.min(give));
            quietest(a, lo, hi)?.clamp(lo, hi)
        } else {
            a
        };
        let end = if b < trim.end_ms {
            let (lo, hi) = (b - SNAP_MS.min(give), b + SNAP_MS.min((after - b) / 2));
            quietest(b, lo, hi)?.clamp(lo, hi)
        } else {
            b
        };
        parts.push((start, end));
    }
    Ok(end_to_end(parts))
}

/// The quietest point in `lo..=hi` (ms) of audio track `track` of `source`: the middle of the
/// [`QUIET_WINDOW_MS`] window with the least energy, the one nearest `edge` among equals, or
/// `edge` itself where the track has no sound to compare.
pub fn quietest(source: &Path, track: usize, edge: u64, lo: u64, hi: u64, cancel: &AtomicBool) -> Result<u64> {
    let half = QUIET_WINDOW_MS / 2;
    let from = lo.saturating_sub(half);
    let to = hi + half;
    let per_ms = (crate::audio::RATE / 1000) as u64;
    let bins = (to - from) as usize;
    let mut energy = vec![0f64; bins];
    let mut heard = vec![false; bins];
    let span = TrimRange { start_ms: from, end_ms: to };
    crate::platform::decode_audio(source, track, span, cancel, &mut |samples, position| {
        for (i, frame) in samples.chunks_exact(crate::audio::CHANNELS).enumerate() {
            let bin = ((position + i as u64) / per_ms) as usize;
            if bin < bins {
                energy[bin] += frame.iter().map(|&s| s as f64 * s as f64).sum::<f64>();
                heard[bin] = true;
            }
        }
        Ok(())
    })?;
    let mut best: Option<(f64, u64, u64)> = None;
    for point in lo..=hi {
        let first = point.saturating_sub(half).max(from) - from;
        let window = first as usize..((point + half - from) as usize).min(bins);
        if window.is_empty() || !heard[window.clone()].iter().all(|&h| h) {
            continue;
        }
        let loudness: f64 = energy[window].iter().sum();
        let distance = point.abs_diff(edge);
        let better = match best {
            None => true,
            Some((quietest, nearest, _)) => loudness < quietest || (loudness == quietest && distance < nearest),
        };
        if better {
            best = Some((loudness, distance, point));
        }
    }
    Ok(best.map_or(edge, |(_, _, point)| point))
}

/// The gain at frame `frame` of a part `length` frames long: a raised-cosine fade over the first
/// `ramp` frames after a join (`fade_in`) and over the last `ramp` frames before one (`fade_out`).
pub fn ramp_gain(frame: u64, length: u64, ramp: u64, fade_in: bool, fade_out: bool) -> f32 {
    let curve = |x: f64| (0.5 - 0.5 * (std::f64::consts::PI * x.clamp(0.0, 1.0)).cos()) as f32;
    let ramp = ramp.max(1);
    let mut gain = 1.0;
    if fade_in && frame < ramp {
        gain *= curve(frame as f64 / ramp as f64);
    }
    let left = length.saturating_sub(frame);
    if fade_out && left < ramp {
        gain *= curve(left as f64 / ramp as f64);
    }
    gain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(start_ms: u64, end_ms: u64) -> TrimRange {
        TrimRange { start_ms, end_ms }
    }

    /// Sorted, disjoint, inside the trim, at least a frame long, and laid end to end.
    fn check_layout(case: &str, trim: TrimRange, spans: &[Span]) {
        let mut output = 0;
        let mut previous_end = trim.start_ms;
        for span in spans {
            let TrimRange { start_ms, end_ms } = span.source;
            assert!(start_ms >= previous_end, "{case}: {spans:?} overlap or are out of order");
            assert!(end_ms <= trim.end_ms, "{case}: {spans:?} leave the trim");
            assert!(end_ms > start_ms, "{case}: {spans:?} hold an empty part");
            assert_eq!(span.output_start_ms, output, "{case}: {spans:?} do not follow each other in the file");
            output += span.len_ms();
            previous_end = end_ms;
        }
    }

    /// The kept parts against a millisecond-by-millisecond reckoning of the cuts: every
    /// millisecond of the trim is either kept or cut, the kept ones are exactly the uncut runs
    /// (less the runs too short to keep), and kept plus cut is the trim.
    #[test]
    fn kept_parts_and_cuts_tile_the_trim() {
        let trim = r(1_000, 9_000);
        let cases: Vec<(&str, Vec<TrimRange>)> = vec![
            ("no cut", vec![]),
            ("overlapping", vec![r(2_000, 3_000), r(2_500, 3_500), r(2_600, 2_700)]),
            ("adjacent", vec![r(2_000, 3_000), r(3_000, 4_000), r(4_000, 4_200)]),
            ("outside and overflowing", vec![r(0, 500), r(500, 1_200), r(8_800, 12_000), r(20_000, 21_000)]),
            ("unsorted", vec![r(7_000, 7_500), r(2_000, 2_100), r(4_000, 5_000), r(1_500, 1_600)]),
            ("a sliver between two cuts", vec![r(2_000, 3_000), r(3_020, 4_000), r(8_985, 9_000)]),
            ("empty and inverted", vec![r(3_000, 3_000), r(5_000, 4_000)]),
            ("everything", vec![r(4_000, 9_500), r(0, 4_000)]),
        ];
        for (case, cuts) in &cases {
            let spans = kept_spans(trim, cuts);
            check_layout(case, trim, &spans);

            let length = (trim.end_ms - trim.start_ms) as usize;
            let cut: Vec<bool> =
                (trim.start_ms..trim.end_ms).map(|ms| cuts.iter().any(|c| c.start_ms <= ms && ms < c.end_ms)).collect();
            let mut uncut_runs = Vec::new();
            let mut run_start = None;
            for (i, &is_cut) in cut.iter().chain(std::iter::once(&true)).enumerate() {
                match (is_cut, run_start) {
                    (false, None) => run_start = Some(i),
                    (true, Some(s)) => {
                        uncut_runs.push((s, i));
                        run_start = None;
                    }
                    _ => {}
                }
            }
            let any_cut = cut.iter().any(|&c| c);
            let expected: Vec<TrimRange> = uncut_runs
                .iter()
                .filter(|(s, e)| !any_cut || (e - s) as u64 >= MIN_PART_MS)
                .map(|&(s, e)| r(trim.start_ms + s as u64, trim.start_ms + e as u64))
                .collect();
            let kept: Vec<TrimRange> = spans.iter().map(|s| s.source).collect();
            assert_eq!(kept, expected, "{case}");
            let kept_ms: u64 = spans.iter().map(Span::len_ms).sum();
            let cut_ms = cut.iter().filter(|&&c| c).count() as u64
                + uncut_runs
                    .iter()
                    .filter(|(s, e)| any_cut && ((e - s) as u64) < MIN_PART_MS)
                    .map(|(s, e)| (e - s) as u64)
                    .sum::<u64>();
            assert_eq!(kept_ms + cut_ms, length as u64, "{case}: kept and cut do not add up to the trim");

            // Snapping keeps the same layout even when every edge goes as far as it may, and it
            // never moves the trim's own edges.
            for far in [false, true] {
                let moved = snapped(trim, &spans, &mut |_, lo, hi| Ok(if far { hi } else { lo })).unwrap();
                check_layout(case, trim, &moved);
                assert_eq!(moved.len(), spans.len(), "{case}");
                for (m, s) in moved.iter().zip(&spans) {
                    assert!(m.len_ms() >= MIN_PART_MS.min(s.len_ms()), "{case}: {moved:?}");
                    assert!(s.source.start_ms != trim.start_ms || m.source.start_ms == trim.start_ms, "{case}");
                    assert!(s.source.end_ms != trim.end_ms || m.source.end_ms == trim.end_ms, "{case}");
                    assert!(m.source.start_ms.abs_diff(s.source.start_ms) <= SNAP_MS, "{case}: {moved:?}");
                    assert!(m.source.end_ms.abs_diff(s.source.end_ms) <= SNAP_MS, "{case}: {moved:?}");
                }
            }
        }
    }
}
