//! The take's timeline: maps a buffer's host time to its time in the file, leaving out paused
//! time, so a pause leaves neither a gap nor a jump backwards.
//!
//! Media time 0 is the first video frame. Every pause is kept as a host-time interval; a buffer
//! inside one is dropped (it arrived late, after the user paused), and a buffer after it moves
//! back by the length of every pause before it. Video frames are also kept strictly increasing,
//! which a writer needs.

use glimpsfy_capture::HostTime;

#[derive(Clone, Debug, Default)]
pub struct Timeline {
    origin: Option<u64>,
    /// Finished pauses as `[start, end)` in host nanoseconds, in order.
    pauses: Vec<(u64, u64)>,
    paused_since: Option<u64>,
    last_video: Option<u64>,
}

impl Timeline {
    pub fn new() -> Timeline {
        Timeline::default()
    }

    pub fn origin(&self) -> Option<HostTime> {
        self.origin.map(HostTime)
    }

    pub fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    pub fn pause(&mut self, at: HostTime) {
        if self.paused_since.is_none() {
            self.paused_since = Some(at.0);
        }
    }

    pub fn resume(&mut self, at: HostTime) {
        if let Some(start) = self.paused_since.take() {
            // A pause before the first frame moves nothing: the origin is set by the first
            // frame after it.
            if self.origin.is_some() && at.0 > start {
                self.pauses.push((start, at.0));
            }
        }
    }

    /// The file time, in nanoseconds, of a buffer stamped `at`; `None` while paused, inside a
    /// pause, or before the first frame.
    pub fn map(&self, at: HostTime) -> Option<u64> {
        let origin = self.origin?;
        let t = at.0;
        if t < origin {
            return None;
        }
        if let Some(start) = self.paused_since {
            if t >= start {
                return None;
            }
        }
        let mut skipped = 0;
        for &(start, end) in &self.pauses {
            if t >= end {
                skipped += end - start;
            } else if t >= start {
                return None;
            } else {
                break;
            }
        }
        Some(t - origin - skipped)
    }

    /// The file time of a video frame: the first frame sets time 0, and a frame that would not
    /// come strictly after the previous one is dropped.
    pub fn map_video(&mut self, at: HostTime) -> Option<u64> {
        if self.origin.is_none() {
            if self.paused_since.is_some() {
                return None;
            }
            self.origin = Some(at.0);
        }
        let media = self.map(at)?;
        if self.last_video.is_some_and(|last| media <= last) {
            return None;
        }
        self.last_video = Some(media);
        Some(media)
    }

    /// The length of the take so far at host time `now`, without paused time.
    pub fn elapsed(&self, now: HostTime) -> u64 {
        let Some(origin) = self.origin else { return 0 };
        let until = self.paused_since.unwrap_or(now.0).max(origin);
        let paused: u64 = self.pauses.iter().map(|(s, e)| e - s).sum();
        until.saturating_sub(origin).saturating_sub(paused)
    }

    pub fn last_video(&self) -> Option<u64> {
        self.last_video
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// Frames every 33 ms with a 5 s pause in the middle land back to back in the file.
    #[test]
    fn a_pause_leaves_no_gap_and_no_step_back() {
        let mut t = Timeline::new();
        let mut host = 1_000 * MS;
        let mut out = Vec::new();
        for i in 0..90 {
            if i == 30 {
                t.pause(HostTime(host));
                // Frames that arrive during the pause are dropped.
                for _ in 0..150 {
                    host += 33 * MS;
                    assert_eq!(t.map_video(HostTime(host)), None);
                }
                t.resume(HostTime(host));
            }
            host += 33 * MS;
            if let Some(m) = t.map_video(HostTime(host)) {
                out.push(m);
            }
        }
        assert_eq!(out.len(), 90);
        for pair in out.windows(2) {
            assert!(pair[1] > pair[0], "time went backwards");
            assert!(pair[1] - pair[0] <= 66 * MS, "a gap of {} ms", (pair[1] - pair[0]) / MS);
        }
        assert!(t.elapsed(HostTime(host)) < 90 * 34 * MS);
    }
}
