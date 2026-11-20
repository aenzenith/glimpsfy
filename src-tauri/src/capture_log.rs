//! The log of slow and failed still captures: one line each in `capture.log` in the app's log
//! folder, in release builds too.
//!
//! A release build prints nothing (`trace::log` is for dev runs), so without this file a capture
//! that took seconds leaves no trace but ScreenCaptureKit's own lines in the unified log. Each
//! line says which path the backend took and when each display's answer came, which tells a
//! congested ScreenCaptureKit apart from a slow overlay:
//!
//! ```text
//! 2026-11-20 09:07:43.770 · Area · 5815 ms since the trigger · rect · display 1 1840 ms, display 3 1860 ms, display 2 no answer · failed: the capture did not finish within 10000 ms
//! ```

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

use glimpsfy_capture::{CaptureError, CaptureReport};
use tauri::AppHandle;

use crate::{paths, trace};

/// A capture that took at least this long after its trigger is written down.
const SLOW: Duration = Duration::from_secs(1);
/// The file is moved to `capture.log.1` (replacing the one before) once it grows past this.
const MAX_BYTES: u64 = 256 * 1024;
const FILE: &str = "capture.log";

/// Writes one line about a capture when it took `SLOW` or longer or failed. `what` names the
/// capture (its mode), `waited` runs from the trigger to the backend's answer, and `report` is
/// what the backend kept about it. Writing is best effort: a log that cannot be written is
/// skipped, never reported.
pub fn record(
    app: &AppHandle,
    what: &str,
    waited: Duration,
    report: Option<CaptureReport>,
    error: Option<&CaptureError>,
) {
    if waited < SLOW && error.is_none() {
        return;
    }
    let line = line(what, waited, report.as_ref(), error);
    trace::log(format!("[glimpsfy] {line}"));
    let _ = append(app, &line);
}

fn line(what: &str, waited: Duration, report: Option<&CaptureReport>, error: Option<&CaptureError>) -> String {
    let mut parts = vec![
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        what.to_string(),
        format!("{:.0} ms since the trigger", trace::ms(waited)),
    ];
    if let Some(report) = report {
        parts.push(report.path.as_str().to_string());
        if let Some(rect) = report.rect {
            parts.push(format!("rect refused after {:.0} ms", trace::ms(rect)));
        }
        if let Some(content) = report.content {
            parts.push(format!("content fetched in {:.0} ms", trace::ms(content)));
        }
        if !report.frames.is_empty() {
            let frames: Vec<String> = report
                .frames
                .iter()
                .map(|(id, arrived)| match arrived {
                    Some(at) => format!("display {} {:.0} ms", id.0, trace::ms(*at)),
                    None => format!("display {} no answer", id.0),
                })
                .collect();
            parts.push(frames.join(", "));
        }
    }
    parts.push(match error {
        Some(error) => format!("failed: {error}"),
        None => "ok".to_string(),
    });
    parts.join(" · ")
}

fn append(app: &AppHandle, line: &str) -> std::io::Result<()> {
    static WRITING: Mutex<()> = Mutex::new(());
    let _guard = WRITING.lock().unwrap_or_else(|p| p.into_inner());
    let dir = paths::log_dir(app).map_err(std::io::Error::other)?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(FILE);
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        fs::rename(&path, dir.join(format!("{FILE}.1")))?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")
}
