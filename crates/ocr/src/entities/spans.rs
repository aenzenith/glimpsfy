//! The recognised lines as one text, and the way back from a range of that text to boxes in the
//! image: each word is found in its line's text, and a piece that covers part of a word gets the
//! matching share of the word's box, counted in characters.

use std::ops::Range;

use super::EntityBox;
use crate::{Bounds, OcrResult};

/// A word's place in the joined text and its box.
struct Word {
    range: Range<usize>,
    bounds: Bounds,
}

struct Line {
    range: Range<usize>,
    bounds: Bounds,
    words: Vec<Word>,
}

/// The lines of a result joined with `\n`, with every word's byte range in the joined text.
pub(super) struct Located {
    text: String,
    lines: Vec<Line>,
}

/// A word found this far after the one before belongs to another place of the line; indentation
/// and the tabs between table cells stay well under it.
const MAX_GAP: usize = 96;

impl Located {
    pub(super) fn new(result: &OcrResult) -> Located {
        let mut text = String::new();
        let mut lines = Vec::with_capacity(result.lines.len());
        for (i, line) in result.lines.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let start = text.len();
            // A line never holds a break of its own; one that did would shift every later line.
            text.push_str(&line.text.replace(['\n', '\r'], " "));
            let mut at = start;
            let mut words = Vec::with_capacity(line.words.len());
            for word in &line.words {
                let forms = [word.text.as_str(), word.text.trim_end_matches(['-', '\u{AD}', '\u{2010}'])];
                let found = forms.iter().filter(|form| !form.is_empty()).find_map(|form| {
                    let i = text[at..].find(form)?;
                    (i <= MAX_GAP).then(|| at + i..at + i + form.len())
                });
                if let Some(range) = found {
                    at = range.end;
                    words.push(Word { range, bounds: word.bounds });
                }
            }
            lines.push(Line { range: start..text.len(), bounds: line.bounds, words });
        }
        Located { text, lines }
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// The boxes `range` covers, one per line, in pixels of the recognised image.
    pub(super) fn boxes(&self, range: Range<usize>) -> Vec<EntityBox> {
        let mut out = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            let (from, to) = (range.start.max(line.range.start), range.end.min(line.range.end));
            if from >= to || self.text[from..to].trim().is_empty() {
                continue;
            }
            let mut union: Option<Bounds> = None;
            for word in &line.words {
                let (a, b) = (from.max(word.range.start), to.min(word.range.end));
                if a < b {
                    let part = self.share(&word.range, word.bounds, a..b);
                    union = Some(union.map_or(part, |u| u.union(&part)));
                }
            }
            // A piece in text no word was found in (or a line without words) takes its share of
            // the line's box instead.
            let bounds = union.unwrap_or_else(|| self.share(&line.range, line.bounds, from..to));
            out.push(EntityBox { line: index as u32, bounds });
        }
        out
    }

    /// The part of `bounds`, the box of the text at `whole`, that the characters of `part` take,
    /// splitting the width by the number of characters.
    fn share(&self, whole: &Range<usize>, bounds: Bounds, part: Range<usize>) -> Bounds {
        let count = |r: Range<usize>| self.text[r].chars().count() as f64;
        let total = count(whole.clone()).max(1.0);
        let before = count(whole.start..part.start);
        let inside = count(part);
        let x = bounds.x + bounds.width * before / total;
        Bounds::new(x, bounds.y, bounds.width * inside / total, bounds.height)
    }
}

/// Converts the UTF-16 ranges NSDataDetector reports into byte ranges of the same text.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) struct Utf16Map {
    /// The byte offset of each UTF-16 position, `None` inside a surrogate pair; one more entry
    /// than the text has units, for the end.
    bytes: Vec<Option<usize>>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Utf16Map {
    pub(super) fn new(text: &str) -> Utf16Map {
        let mut bytes = Vec::with_capacity(text.len() + 1);
        for (offset, c) in text.char_indices() {
            bytes.push(Some(offset));
            if c.len_utf16() == 2 {
                bytes.push(None);
            }
        }
        bytes.push(Some(text.len()));
        Utf16Map { bytes }
    }

    /// The number of UTF-16 units of the text.
    pub(super) fn len(&self) -> usize {
        self.bytes.len() - 1
    }

    /// The byte range of `length` units from `location`, or `None` when it does not start and end
    /// on a character of the text.
    pub(super) fn range(&self, location: usize, length: usize) -> Option<Range<usize>> {
        let start = (*self.bytes.get(location)?)?;
        let end = (*self.bytes.get(location.checked_add(length)?)?)?;
        (start <= end).then_some(start..end)
    }
}
