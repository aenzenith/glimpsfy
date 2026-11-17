//! What a search result shows: the quoted line with its marks, and the boxes of every match in
//! the recognised text, so a card can zoom onto one and the inspector can outline them all.

use crate::fold::{match_ranges, Query};
use crate::{RecognizedText, TextBox, TextMatch, TextRange};

/// How many UTF-16 units of context the snippet keeps before and after the first match.
const BEFORE: u32 = 34;
const AFTER: u32 = 80;

/// The line that gives the snippet: its index, its match ranges and the index of its first box.
type Best = (usize, Vec<(u32, u32)>, usize);

/// The match of one entry. `name` is the entry's name, `text` its recognised or copied text, and
/// `lines` the recognised lines with boxes, when there are any.
pub(crate) fn text_match(
    query: &Query,
    name: &str,
    other_fields: &[&str],
    text: Option<&str>,
    lines: Option<&RecognizedText>,
) -> TextMatch {
    let in_name =
        std::iter::once(name).chain(other_fields.iter().copied()).any(|field| !match_ranges(field, query).is_empty());

    // The lines to look in: the recognised lines when the engine gave them, the text's own lines
    // otherwise (a text copy).
    let plain: Vec<&str> = match (lines, text) {
        (Some(lines), _) => lines.lines.iter().map(|l| l.text.as_str()).collect(),
        (None, Some(text)) => text.lines().collect(),
        (None, None) => Vec::new(),
    };

    let mut boxes = Vec::new();
    let mut count = 0u32;
    let mut best: Option<Best> = None;
    for (index, line) in plain.iter().enumerate() {
        let ranges = match_ranges(line, query);
        if ranges.is_empty() {
            continue;
        }
        count += ranges.len() as u32;
        let first_box = boxes.len();
        if let Some(recognized) = lines {
            let line = &recognized.lines[index];
            boxes.extend(ranges.iter().map(|&range| box_of(line, range)));
        }
        let longer = best.as_ref().is_none_or(|(b, _, _)| line.chars().count() > plain[*b].chars().count());
        if longer {
            best = Some((index, ranges, first_box));
        }
    }

    match best {
        Some((index, ranges, first_box)) => {
            let (snippet, highlights) = cut(plain[index], &ranges);
            TextMatch {
                snippet,
                highlights,
                in_name,
                count,
                best: lines.map(|_| first_box as u32),
                boxes,
                image_size: lines.map(|l| l.image_size),
            }
        }
        None => TextMatch {
            snippet: name.to_string(),
            highlights: match_ranges(name, query).into_iter().map(|(start, end)| TextRange { start, end }).collect(),
            in_name,
            count: 0,
            boxes: Vec::new(),
            best: None,
            image_size: lines.map(|l| l.image_size),
        },
    }
}

/// The box of the match at `range` (UTF-16 units of the line's text): the union of the words it
/// touches when the engine gave words, otherwise a share of the line's box in proportion to the
/// characters before and inside the match.
fn box_of(line: &crate::TextLine, (start, end): (u32, u32)) -> TextBox {
    let units: Vec<u16> = line.text.encode_utf16().collect();
    let length = units.len().max(1) as f64;
    if !line.words.is_empty() {
        let mut cursor = 0usize;
        let mut found: Option<TextBox> = None;
        for word in &line.words {
            let word_units: Vec<u16> = word.text.encode_utf16().collect();
            if word_units.is_empty() {
                continue;
            }
            let Some(at) = find(&units[cursor..], &word_units).map(|i| i + cursor) else {
                continue;
            };
            let (word_start, word_end) = (at as u32, (at + word_units.len()) as u32);
            cursor = at + word_units.len();
            if word_start < end && word_end > start {
                found = Some(match found {
                    Some(b) => b.union(word.bounds),
                    None => word.bounds,
                });
            }
        }
        if let Some(found) = found {
            return found;
        }
    }
    let b = line.bounds;
    TextBox {
        x: b.x + b.width * start as f64 / length,
        y: b.y,
        width: (b.width * (end - start) as f64 / length).max(1.0),
        height: b.height,
    }
}

fn find(haystack: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

/// The part of `line` around its first match, with "…" where it was cut and the marks moved
/// into the cut's coordinates. A cut at the start moves forward to the next word.
fn cut(line: &str, ranges: &[(u32, u32)]) -> (String, Vec<TextRange>) {
    let units: Vec<u16> = line.encode_utf16().collect();
    let total = units.len() as u32;
    let (first_start, first_end) = ranges[0];
    let mut start = first_start.saturating_sub(BEFORE);
    let end = (first_end + AFTER).min(total);
    if start > 0 {
        if let Some(space) = (start..first_start).find(|&i| units[i as usize] == b' ' as u16) {
            start = space + 1;
        }
        // Never start inside a surrogate pair.
        while start < first_start && (0xDC00..0xE000).contains(&units[start as usize]) {
            start += 1;
        }
    }
    let mut end = end;
    while end < total && end > 0 && (0xDC00..0xE000).contains(&units[end as usize]) {
        end += 1;
    }
    let lead = if start > 0 { "…" } else { "" };
    let tail = if end < total { "…" } else { "" };
    let body = String::from_utf16_lossy(&units[start as usize..end as usize]);
    let shift = lead.encode_utf16().count() as u32;
    let highlights = ranges
        .iter()
        .filter(|(s, e)| *s >= start && *e <= end)
        .map(|(s, e)| TextRange { start: s - start + shift, end: e - start + shift })
        .collect();
    (format!("{lead}{body}{tail}"), highlights)
}
