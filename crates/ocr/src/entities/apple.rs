//! NSDataDetector, the system's own finder of links, phone numbers and dates, on macOS. It runs
//! on the machine, reads the text it is given and nothing else, and knows more ways to write a
//! date than the rules do ("tomorrow at 3", "next Friday").

use chrono::{Duration, Local, NaiveDateTime, NaiveTime, TimeZone, Timelike};
use objc2::rc::autoreleasepool;
use objc2_foundation::{
    NSDataDetector, NSMatchingOptions, NSRange, NSString, NSTextCheckingResult, NSTextCheckingType,
};

use super::rules::{
    card_expiry, dial_value, fits_written, has_time, is_address, known_domain, labelled_id, names_day, trim_link,
};
use super::spans::Utf16Map;
use super::{Candidate, EntityDate, EntityKind};
use crate::link_for;

/// The links, email addresses, phone numbers and dates NSDataDetector finds in `text`, by byte
/// range. A detector that cannot be made finds nothing, and the rules still run.
pub(super) fn detect(text: &str) -> Vec<Candidate> {
    autoreleasepool(|_| {
        let types = (NSTextCheckingType::Link | NSTextCheckingType::PhoneNumber | NSTextCheckingType::Date).0;
        let Ok(detector) = NSDataDetector::dataDetectorWithTypes_error(types) else {
            return Vec::new();
        };
        let map = Utf16Map::new(text);
        let string = NSString::from_str(text);
        let matches =
            detector.matchesInString_options_range(&string, NSMatchingOptions::empty(), NSRange::new(0, map.len()));
        matches
            .iter()
            .filter_map(|found| {
                let range = found.range();
                let bytes = map.range(range.location, range.length)?;
                let mut bytes = bytes.start..bytes.start + text[bytes.clone()].trim_end().len();
                if found.resultType() == NSTextCheckingType::Link {
                    bytes = trim_link(text, bytes);
                }
                let (kind, value, date) = read(&found, text, bytes.clone())?;
                Some(Candidate { kind, range: bytes, value, date })
            })
            .collect()
    })
}

/// What one result of the detector is, or `None` for one the text layer offers no action for (a
/// link of another scheme, a link that `link_for` refuses, a number labelled as an ID, a date the
/// text does not write).
fn read(
    found: &NSTextCheckingResult,
    text: &str,
    range: std::ops::Range<usize>,
) -> Option<(EntityKind, String, Option<EntityDate>)> {
    let piece = &text[range.clone()];
    let kind = found.resultType();
    if kind == NSTextCheckingType::Link {
        let link = found.URL()?;
        let scheme = link.scheme()?.to_string().to_ascii_lowercase();
        return match scheme.as_str() {
            "mailto" => {
                let address = match piece.get(..7) {
                    Some(head) if head.eq_ignore_ascii_case("mailto:") => &piece[7..],
                    _ => piece,
                };
                let address = address.split('?').next().unwrap_or_default();
                is_address(address).then(|| (EntityKind::Email, address.to_string(), None))
            }
            "http" | "https" => {
                let given = ["http://", "https://"]
                    .iter()
                    .any(|s| piece.get(..s.len()).is_some_and(|head| head.eq_ignore_ascii_case(s)));
                if !given && !known_domain(piece) {
                    return None;
                }
                // A bare domain is opened over HTTPS, whatever scheme the detector guessed.
                let value = if given { piece.to_string() } else { format!("https://{piece}") };
                link_for(&value).map(|value| (EntityKind::Link, value, None))
            }
            _ => None,
        };
    }
    if kind == NSTextCheckingType::PhoneNumber {
        if labelled_id(text, range.start) {
            return None;
        }
        let number = found.phoneNumber()?.to_string();
        let value = dial_value(&number);
        // No phone number has more than 15 digits (E.164); a longer run is a card or an account.
        let digits = value.trim_start_matches('+').len();
        return (7..=15).contains(&digits).then_some((EntityKind::Phone, value, None));
    }
    if kind == NSTextCheckingType::Date {
        // No date runs over a line break, and a card's "12/27" is its expiry.
        if !names_day(piece) || piece.contains('\n') || card_expiry(text, range.clone()) {
            return None;
        }
        let seconds = found.date()?.timeIntervalSince1970();
        let start = local(seconds)?;
        // The detector rolls an impossible day over ("31.02.2026" becomes 3 March).
        if !fits_written(piece, start.date()) {
            return None;
        }
        return Some((EntityKind::Date, piece.to_string(), Some(span(start, found.duration(), has_time(piece)))));
    }
    None
}

/// The local wall-clock time of a moment, in seconds since 1970.
fn local(seconds: f64) -> Option<NaiveDateTime> {
    if !seconds.is_finite() {
        return None;
    }
    let whole = seconds.floor();
    let nanos = ((seconds - whole) * 1e9) as u32;
    Local.timestamp_opt(whole as i64, nanos.min(999_999_999)).single().map(|t| t.naive_local())
}

/// The event of a detected date: `timed` when the text names a clock time, otherwise the whole
/// day. NSDataDetector reports a duration only for a range ("14:30–15:30", "27–29 Kasım").
fn span(start: NaiveDateTime, duration: f64, timed: bool) -> EntityDate {
    let length = (duration.is_finite() && duration > 0.0).then(|| Duration::seconds(duration.round() as i64));
    if timed {
        let start =
            start.date().and_time(NaiveTime::from_hms_opt(start.hour(), start.minute(), 0).unwrap_or(start.time()));
        let end = length.map(|l| start + l);
        return EntityDate {
            start,
            end: Some(end.unwrap_or(start + Duration::hours(1))),
            all_day: false,
            end_assumed: end.is_none(),
        };
    }
    let day = start.date();
    let last = length.map(|l| (start + l).date()).filter(|last| *last > day);
    EntityDate {
        start: day.and_time(NaiveTime::MIN),
        end: last.map(|d| d.and_time(NaiveTime::MIN)),
        all_day: true,
        end_assumed: false,
    }
}
