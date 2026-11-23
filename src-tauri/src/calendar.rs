//! Adds an event to the user's calendar without asking for a permission: the event becomes a
//! small iCalendar file (RFC 5545) in `<cache>/events/`, and the system opens it with the
//! calendar app, which shows the event and lets the user add it. Calendar access (EventKit,
//! the Windows and Linux calendar stores) is never requested.
//!
//! The times carry no zone ("floating" times in RFC 5545), so the calendar reads them in the
//! system's own zone, as the text the date was read from meant them. A file is never replaced,
//! and the files older than a day go when the next event is added.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use tauri::AppHandle;

use crate::commands::error::{CommandError, CommandResult};

/// An event as the text layer sends it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    /// The line the date was read from.
    pub title: String,
    /// Local time without a zone; midnight of the day for an all-day event.
    pub start: NaiveDateTime,
    /// The end of a timed event, or the last day of an all-day event over several days. Without
    /// it a timed event lasts one hour and an all-day event one day.
    #[serde(default)]
    pub end: Option<NaiveDateTime>,
    pub all_day: bool,
}

/// How long an event file stays in the cache; the calendar has read it long before.
const KEEP: Duration = Duration::from_secs(24 * 60 * 60);

/// Writes the event as a new file in `<cache>/events/` and opens it with the system's calendar
/// app. Returns the file.
pub fn add(app: &AppHandle, event: &CalendarEvent) -> CommandResult<PathBuf> {
    use tauri_plugin_opener::OpenerExt;
    let folder = crate::paths::cache_dir(app)?.join("events");
    crate::commands::render::prune(&folder, KEEP);
    let text = ics(event, Utc::now().naive_utc(), &uid());
    let stem: String = event.title.trim().chars().take(60).collect();
    let path =
        crate::output::save_named(&folder, if stem.is_empty() { "Event" } else { &stem }, "ics", text.as_bytes())?;
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| CommandError::internal(e.to_string()))?;
    Ok(path)
}

/// An identifier no other event of this app shares: the time and a counter, under the app's
/// name. Nothing is looked up for it.
fn uid() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    format!("{nanos}-{}@glimpsfy", NEXT.fetch_add(1, Ordering::SeqCst))
}

/// The calendar file of one event, with CRLF line ends and lines folded at 75 bytes. `stamp` is
/// the time the file is made, in UTC.
pub fn ics(event: &CalendarEvent, stamp: NaiveDateTime, uid: &str) -> String {
    const FLOATING: &str = "%Y%m%dT%H%M%S";
    let (start, end) = if event.all_day {
        let first = event.start.date();
        let last = event.end.map(|e| e.date()).filter(|last| *last >= first).unwrap_or(first);
        let after = last.succ_opt().unwrap_or(NaiveDate::MAX);
        (
            format!("DTSTART;VALUE=DATE:{}", first.format("%Y%m%d")),
            format!("DTEND;VALUE=DATE:{}", after.format("%Y%m%d")),
        )
    } else {
        let end = event.end.filter(|end| *end > event.start).unwrap_or(event.start + chrono::Duration::hours(1));
        (format!("DTSTART:{}", event.start.format(FLOATING)), format!("DTEND:{}", end.format(FLOATING)))
    };
    let title = event.title.trim();
    let lines = [
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Glimpsfy//Text actions//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}", escape(uid)),
        format!("DTSTAMP:{}Z", stamp.format(FLOATING)),
        start,
        end,
        format!("SUMMARY:{}", escape(if title.is_empty() { "Event" } else { title })),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
    ];
    lines.iter().map(|line| fold(line)).collect::<Vec<_>>().join("")
}

/// A TEXT value as RFC 5545 writes it: `\`, `;` and `,` escaped, every line break as `\n`, and
/// no other control character, so a title can never end its line and start a property of its
/// own.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' | '\u{2028}' | '\u{2029}' | '\u{85}' => out.push_str("\\n"),
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// One content line with its CRLF, folded so that no line is longer than 75 bytes: the rest goes
/// on lines that start with a space, and a character is never split between two lines.
fn fold(line: &str) -> String {
    const LIMIT: usize = 75;
    let mut out = String::with_capacity(line.len() + line.len() / LIMIT * 3 + 2);
    let mut used = 0;
    for c in line.chars() {
        if used + c.len_utf8() > LIMIT {
            out.push_str("\r\n ");
            used = 1;
        }
        out.push(c);
        used += c.len_utf8();
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(title: &str) -> CalendarEvent {
        let start = NaiveDate::from_ymd_opt(2026, 11, 27).and_then(|d| d.and_hms_opt(14, 30, 0)).expect("a valid time");
        CalendarEvent { title: title.into(), start, end: None, all_day: false }
    }

    fn stamp() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 23).and_then(|d| d.and_hms_opt(12, 0, 0)).expect("a valid time")
    }

    /// The content lines with the folding undone.
    fn unfolded(text: &str) -> Vec<String> {
        text.replace("\r\n ", "").split("\r\n").filter(|l| !l.is_empty()).map(str::to_string).collect()
    }

    #[test]
    fn a_title_cannot_add_a_property_of_its_own() {
        let title = "Sunum\r\nATTENDEE:mailto:x@evil.example\nEND:VEVENT\r; a, b \\ c\u{0}\u{1b}";
        let text = ics(&event(title), stamp(), "1@glimpsfy");
        let lines = unfolded(&text);
        let names: Vec<&str> = lines.iter().map(|l| l.split([':', ';']).next().unwrap_or_default()).collect();
        assert_eq!(
            names,
            [
                "BEGIN", "VERSION", "PRODID", "CALSCALE", "BEGIN", "UID", "DTSTAMP", "DTSTART", "DTEND", "SUMMARY",
                "END", "END"
            ]
        );
        let summary = lines.iter().find(|l| l.starts_with("SUMMARY:")).expect("a summary");
        assert_eq!(summary, "SUMMARY:Sunum\\nATTENDEE:mailto:x@evil.example\\nEND:VEVENT\\n\\; a\\, b \\\\ c");
        assert!(!text.contains('\u{0}') && !text.contains('\u{1b}'));
        // Only the calendar's own line ends are in the file.
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        assert!(!text.replace("\r\n", "").contains(['\r', '\n']));
    }

    #[test]
    fn folding_keeps_lines_short_and_characters_whole() {
        // Two- and four-byte characters at every offset around the 75-byte limit.
        for lead in 0..4 {
            let title = format!("{}{}", "a".repeat(lead), "ş😀ç".repeat(40));
            let text = ics(&event(&title), stamp(), "1@glimpsfy");
            for line in text.split("\r\n") {
                assert!(line.len() <= 75, "a line of {} bytes: {line:?}", line.len());
            }
            let summary = unfolded(&text).into_iter().find(|l| l.starts_with("SUMMARY:")).expect("a summary");
            assert_eq!(summary, format!("SUMMARY:{title}"));
        }
    }
}
