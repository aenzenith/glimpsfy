//! The pieces of a recognised text that a user can act on: phone numbers, email addresses, links
//! and dates, each with the boxes it covers in the image.
//!
//! The text layer underlines what [`find_entities`] returns and offers a menu for each piece: a
//! number is called, an address gets an email, a link opens, a date goes to the calendar. Smart
//! redaction (D28) reads the same module for the kinds it offers to hide: addresses, numbers,
//! cards, IBANs, keys, tokens and the passwords inside links ([`checks`]).
//!
//! # How a piece is found
//!
//! The lines of an [`OcrResult`] are joined with line breaks into one text ([`spans`]), so a
//! piece can run over two lines and still get one box per line. The plain Rust rules ([`rules`])
//! run on every platform; on macOS the system's NSDataDetector reads the text too ([`apple`]).
//! Where both found something at the same place, NSDataDetector's phone number, address or link
//! is kept, but the rules' date: `examples/entities.rs` measured NSDataDetector reading 16 of 35
//! sample dates in the app's seven languages (none in Russian, Japanese or Chinese), splitting
//! "30 Kasım, 09:00–12:00" in two and reading "bis zum 3. Dezember" as a range from today, while
//! the rules read 34. NSDataDetector's dates fill the places the rules leave ("tomorrow at 3",
//! "15/01/2027"), and a bare clock time it reads as today ("14:02" in a mail's header) is left
//! out. Detection runs on the recognised text only: nothing is looked up and no request leaves
//! the machine, and an area that was blurred before reading was never recognised at all.
//!
//! # Dates
//!
//! A date is read in the system's local time and carries no zone; the calendar file is written
//! the same way, so the event keeps the clock time the text shows. A date without a time is an
//! all-day event, and a time without an end lasts one hour ([`EntityDate::end_assumed`]). A
//! date without a year takes the year that puts it closest to `now`.

use std::ops::Range;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::{Bounds, OcrResult};

#[cfg(target_os = "macos")]
mod apple;
mod checks;
mod rules;
mod spans;

/// What kind of piece was found. The camelCase names are the TypeScript side's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntityKind {
    Phone,
    Email,
    /// An `http` or `https` link; a bare domain such as `lokantadeniz.com/menu` gets `https://`.
    Link,
    Date,
    /// A payment card number (D28).
    Card,
    /// An IBAN (D28).
    Iban,
    /// An API key (D28).
    ApiKey,
    /// An access token (D28).
    Token,
    /// The password inside a link such as `https://user:secret@host` (D28).
    UrlPassword,
}

/// The part of one line a piece covers, in pixels of the recognised image.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityBox {
    /// The index of the line in [`OcrResult::lines`].
    pub line: u32,
    pub bounds: Bounds,
}

/// When a found date takes place, in the system's local time without a zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDate {
    /// The start; midnight of the day for an all-day date.
    pub start: NaiveDateTime,
    /// The end of a timed date (the hour after `start` when the text names none), or the last day
    /// of an all-day date that spans several days. `None` for an all-day date of one day.
    pub end: Option<NaiveDateTime>,
    /// The text names a day without a time.
    pub all_day: bool,
    /// The text names no end, so the event was given one hour; the menu says so.
    pub end_assumed: bool,
}

/// One piece of the text, as the text layer underlines it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub kind: EntityKind,
    /// The piece as the text spells it.
    pub text: String,
    /// What an action uses: a number as `+` and digits only, an address as it is, a link with its
    /// scheme, a date as the text spells it (the parsed date is in `date`).
    pub value: String,
    /// One box per line the piece covers, in reading order.
    pub boxes: Vec<EntityBox>,
    pub date: Option<EntityDate>,
}

/// A piece one detector reported, by its byte range in the joined text.
#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    kind: EntityKind,
    range: Range<usize>,
    value: String,
    date: Option<EntityDate>,
}

impl Candidate {
    fn overlaps(&self, other: &Candidate) -> bool {
        self.range.start < other.range.end && other.range.start < self.range.end
    }
}

/// Finds the pieces of `kinds` in a recognised text, in reading order. `now` is the local time
/// a date without a year is read against.
pub fn find_entities(result: &OcrResult, kinds: &[EntityKind], now: NaiveDateTime) -> Vec<Entity> {
    if kinds.is_empty() || result.lines.is_empty() {
        return Vec::new();
    }
    let located = spans::Located::new(result);
    let text = located.text();
    #[cfg(target_os = "macos")]
    let first = apple::detect(text);
    #[cfg(not(target_os = "macos"))]
    let first = Vec::new();
    let mut chosen = merge(first, rules::find(text, now));
    // A card's or an IBAN's digits are no phone number and the `secret@host` of a link is no
    // address, whatever NSDataDetector or the rules made of them. A key may sit inside a link,
    // so the sensitive pieces are added beside the others rather than merged with them.
    let sensitive = checks::find(text);
    chosen.retain(|c| !checks::hides(text, &sensitive, c));
    chosen.extend(sensitive);
    chosen.sort_by_key(|c| c.range.start);
    chosen
        .into_iter()
        .filter(|c| kinds.contains(&c.kind))
        .filter_map(|c| {
            let boxes = located.boxes(c.range.clone());
            if boxes.is_empty() {
                return None;
            }
            Some(Entity { kind: c.kind, text: text[c.range].to_string(), value: c.value, boxes, date: c.date })
        })
        .collect()
}

/// The dates of a plain text as NSDataDetector alone (empty outside macOS), the rules alone and
/// both together read them, for `examples/entities.rs`, which measures what each reads in every
/// language of the app.
#[doc(hidden)]
pub fn measure_dates(text: &str, now: NaiveDateTime) -> [Vec<(String, EntityDate)>; 3] {
    #[cfg(target_os = "macos")]
    let first = apple::detect(text);
    #[cfg(not(target_os = "macos"))]
    let first = Vec::new();
    let rules = rules::find(text, now);
    let merged = merge(first.clone(), rules.clone());
    [first, rules, merged]
        .map(|found| found.into_iter().filter_map(|c| Some((text[c.range.clone()].to_string(), c.date?))).collect())
}

/// The pieces of both detectors without overlaps, sorted by position: the rules' dates first,
/// then NSDataDetector's other pieces, its dates, and the rules' other pieces, each kept where
/// nothing before it lies.
fn merge(first: Vec<Candidate>, rules: Vec<Candidate>) -> Vec<Candidate> {
    let is_date = |c: &Candidate| c.kind == EntityKind::Date;
    let (rule_dates, rule_rest): (Vec<_>, Vec<_>) = rules.into_iter().partition(is_date);
    let (first_dates, first_rest): (Vec<_>, Vec<_>) = first.into_iter().partition(is_date);
    let mut chosen: Vec<Candidate> = Vec::new();
    for candidate in rule_dates.into_iter().chain(first_rest).chain(first_dates).chain(rule_rest) {
        if !chosen.iter().any(|c| c.overlaps(&candidate)) {
            chosen.push(candidate);
        }
    }
    chosen.sort_by_key(|c| c.range.start);
    chosen
}
