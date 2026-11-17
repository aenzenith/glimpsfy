//! Latency marks of one capture, from the shortcut to the first painted frame.
//!
//! Every capture carries a `Trace`. In dev builds the overlay's paint report prints one line per
//! capture (`summary`), which is how milestone 0 measures the 150 ms budget:
//!
//! ```text
//! [glimpsfy] capture #4 · captured 38.1 · served 52.4 · visible 61.0 · painted 77.9 ms · rgba · 1 display · screencapturekit
//! ```
//!
//! Each number is milliseconds since the shortcut reached the app. "served" is the last frame
//! the scheme handler answered, "visible" is when the last overlay window was ordered in at full
//! opacity, and "painted" is when the overlay's page reported its first frame drawn after that
//! (two `requestAnimationFrame` callbacks, so the frame holding the image was produced).

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    Captured,
    Served,
    Visible,
    Painted,
}

impl Mark {
    fn name(self) -> &'static str {
        match self {
            Mark::Captured => "captured",
            Mark::Served => "served",
            Mark::Visible => "visible",
            Mark::Painted => "painted",
        }
    }
}

#[derive(Debug)]
pub struct Trace {
    pub start: Instant,
    marks: Mutex<Vec<(Mark, Duration)>>,
    notes: Mutex<Vec<String>>,
}

impl Trace {
    /// Starts a trace at `start`, the moment the shortcut (or the menu item) reached the app.
    pub fn new(start: Instant) -> Trace {
        Trace { start, marks: Mutex::new(Vec::new()), notes: Mutex::new(Vec::new()) }
    }

    /// Records `mark` now. A mark recorded twice keeps the later time, so "served" and "visible"
    /// end up at the last display's.
    pub fn mark(&self, mark: Mark) -> Duration {
        let elapsed = self.start.elapsed();
        let mut marks = self.marks.lock().unwrap_or_else(|p| p.into_inner());
        marks.retain(|(m, _)| *m != mark);
        marks.push((mark, elapsed));
        elapsed
    }

    pub fn get(&self, mark: Mark) -> Option<Duration> {
        self.marks.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(m, _)| *m == mark).map(|(_, d)| *d)
    }

    /// Adds a short note to the summary line, such as the page's own fetch and draw times.
    pub fn note(&self, note: impl Into<String>) {
        self.notes.lock().unwrap_or_else(|p| p.into_inner()).push(note.into());
    }

    /// The one-line summary, marks in pipeline order.
    pub fn summary(&self, label: &str) -> String {
        let mut marks = self.marks.lock().unwrap_or_else(|p| p.into_inner()).clone();
        marks.sort();
        let mut line = format!("[glimpsfy] {label}");
        for (mark, at) in marks {
            line.push_str(&format!(" · {} {:.1}", mark.name(), at.as_secs_f64() * 1000.0));
        }
        line.push_str(" ms");
        for note in self.notes.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            line.push_str(" · ");
            line.push_str(note);
        }
        line
    }
}

/// Prints a line in dev builds only.
pub fn log(line: impl AsRef<str>) {
    if cfg!(debug_assertions) {
        eprintln!("{}", line.as_ref());
    }
}

/// Milliseconds of a duration, for summaries.
pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
