//! Subtitles made from a transcript: the words that stay in an exported file, grouped into short
//! captions and timed in that file.
//!
//! A caption holds one sentence, or a piece of a long one, on at most two lines. A long sentence
//! is parted after a comma when it can be, a pause in the speech starts a new caption, and every
//! caption stays on screen a little after its last word. Hesitation sounds (ııı, eee) never show;
//! the filler words the user kept show as they were said, and a word the user corrected shows as
//! corrected.

use crate::{engine, FillerKind, Span, Word, GAP_FILLER_TEXT};

/// One subtitle, in milliseconds of the exported file; `text` holds '\n' between its lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// The characters a line holds at most, the common limit for subtitles in Latin scripts.
pub const LINE_CHARS: usize = 42;
/// The line limit of the languages written without spaces, whose characters are wider.
const UNSPACED_LINE_CHARS: usize = 16;
/// A caption shows at least this long, unless the next one starts sooner.
pub const MIN_MS: u64 = 1_000;
/// A caption shows at most this long; a longer stretch of speech is parted.
pub const MAX_MS: u64 = 7_000;
/// How long a caption stays after its last word ends.
pub const LINGER_MS: u64 = 500;
/// A pause this long between two words starts a new caption, even inside a sentence.
const PAUSE_MS: u64 = 1_200;

/// A word that stays in the file, timed in the file.
struct Placed {
    start: u64,
    end: u64,
    /// The word with a space before it, when the language parts its words with spaces.
    piece: String,
}

/// The captions of `words` (a transcript of `language`) in a file made of the `kept` parts of
/// the recording, in order. A word belongs to the part that holds its middle; the words of the
/// parts that were cut out are left out.
pub fn cues(words: &[Word], language: &str, kept: &[Span]) -> Vec<Cue> {
    let unspaced = engine::unspaced(language);
    let line = if unspaced { UNSPACED_LINE_CHARS } else { LINE_CHARS };
    let placed = place(words, kept, unspaced);
    let total: u64 = kept.iter().map(|s| s.end_ms.saturating_sub(s.start_ms)).sum();

    // A sentence ends at its full stop or at a pause; each one is then parted into captions.
    let mut groups: Vec<Vec<Placed>> = Vec::new();
    let mut sentence: Vec<Placed> = Vec::new();
    for word in placed {
        if let Some(last) = sentence.last() {
            if ends_sentence(&last.piece) || word.start.saturating_sub(last.end) >= PAUSE_MS {
                groups.extend(part(std::mem::take(&mut sentence), line));
            }
        }
        sentence.push(word);
    }
    if !sentence.is_empty() {
        groups.extend(part(sentence, line));
    }

    let mut out: Vec<Cue> = Vec::with_capacity(groups.len());
    for (i, group) in groups.iter().enumerate() {
        let start = group[0].start;
        let last_end = group[group.len() - 1].end;
        let next = groups.get(i + 1).map(|g| g[0].start).unwrap_or(total);
        let end = (last_end + LINGER_MS)
            .max(start + MIN_MS)
            .min((start + MAX_MS).max(last_end))
            .min(next)
            .min(total)
            .max(start + 1);
        let text = layout(group, line).unwrap_or_else(|| joined(group));
        out.push(Cue { start_ms: start, end_ms: end, text });
    }
    out
}

/// The words of `words` that stay in the file, timed in it.
fn place(words: &[Word], kept: &[Span], unspaced: bool) -> Vec<Placed> {
    let mut bases = Vec::with_capacity(kept.len());
    let mut base = 0u64;
    for span in kept {
        bases.push(base);
        base += span.end_ms.saturating_sub(span.start_ms);
    }
    let mut out = Vec::with_capacity(words.len());
    for word in words {
        if word.filler == Some(FillerKind::Sound) {
            continue;
        }
        let text = word.shown().trim();
        if text.is_empty() || text == GAP_FILLER_TEXT {
            continue;
        }
        let middle = word.start_ms + word.end_ms.saturating_sub(word.start_ms) / 2;
        let at = kept.partition_point(|s| s.end_ms <= middle);
        let Some(span) = kept.get(at).filter(|s| s.start_ms <= middle) else { continue };
        let from = word.start_ms.clamp(span.start_ms, span.end_ms) - span.start_ms;
        let to = word.end_ms.clamp(span.start_ms, span.end_ms) - span.start_ms;
        // A transcript's words are whole words with their punctuation, so spaces part them
        // wherever the language writes spaces.
        let piece = if unspaced { text.to_string() } else { format!(" {text}") };
        out.push(Placed { start: bases[at] + from, end: bases[at] + to.max(from), piece });
    }
    out
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn joined(words: &[Placed]) -> String {
    words.iter().map(|w| w.piece.as_str()).collect::<String>().trim().to_string()
}

/// The text of `words` on one line, or on two parted between words with the lines as even as
/// possible and the upper one no longer, or `None` when they do not fit.
fn layout(words: &[Placed], line: usize) -> Option<String> {
    let all: Vec<&Placed> = words.iter().collect();
    lines(&all, line)
}

fn lines(words: &[&Placed], line: usize) -> Option<String> {
    let text = |part: &[&Placed]| part.iter().map(|w| w.piece.as_str()).collect::<String>().trim().to_string();
    let whole = text(words);
    if chars(&whole) <= line {
        return Some(whole);
    }
    (1..words.len())
        .filter_map(|k| {
            let (upper, lower) = (text(&words[..k]), text(&words[k..]));
            let (a, b) = (chars(&upper), chars(&lower));
            // A line that ends at a comma may run a few characters longer than an even one.
            let comma = if ends_clause(&upper) { 6 } else { 0 };
            (a <= line && b <= line).then(|| ((a.max(b) as i64 - comma, a > b), format!("{upper}\n{lower}")))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, text)| text)
}

/// A sentence parted into as few captions as fit (two lines, at most [`MAX_MS`]), as even in
/// length as they can be, and parted after a comma where one is near. A sentence that fits
/// stays whole; a single word too long for a line is a caption of its own.
fn part(words: Vec<Placed>, line: usize) -> Vec<Vec<Placed>> {
    let n = words.len();
    let fits = |from: usize, to: usize| {
        to - from == 1
            || (words[to - 1].end.saturating_sub(words[from].start) <= MAX_MS
                && lines(&words[from..to].iter().collect::<Vec<_>>(), line).is_some())
    };
    if n <= 1 || fits(0, n) {
        return vec![words];
    }
    // A caption holds at most two lines, so a caption ending at a word starts only a few words
    // before it.
    let reach = |to: usize| {
        let mut from = to - 1;
        let mut length = chars(&words[from].piece);
        while from > 0 && length + chars(&words[from - 1].piece) <= 2 * line + 1 {
            from -= 1;
            length += chars(&words[from].piece);
        }
        from
    };
    // `fewest[to]`: the fewest captions the words before `to` part into.
    let mut fewest = vec![usize::MAX; n + 1];
    fewest[0] = 0;
    for to in 1..=n {
        for from in reach(to)..to {
            if fewest[from] != usize::MAX && fits(from, to) {
                fewest[to] = fewest[to].min(fewest[from] + 1);
            }
        }
    }
    let count = fewest[n];
    if count == usize::MAX {
        return vec![words];
    }
    let target = chars(&joined(&words)) as i64 / count as i64;
    let comma = (line as i64).pow(2) / 4;
    // `best[k][to]`: the evenest parting of the words before `to` into `k` captions, as its cost
    // and where its last caption starts. A caption costs the square of how far its length is from
    // the even length, less a bonus when it ends at a comma.
    let mut best: Vec<Vec<Option<(i64, usize)>>> = vec![vec![None; n + 1]; count + 1];
    best[0][0] = Some((0, 0));
    for k in 1..=count {
        for to in 1..=n {
            for from in reach(to)..to {
                let Some((cost, _)) = best[k - 1][from] else { continue };
                if !fits(from, to) {
                    continue;
                }
                let length = chars(&joined(&words[from..to])) as i64;
                let bonus = if to < n && ends_clause(&words[to - 1].piece) { comma } else { 0 };
                let here = cost + (length - target).pow(2) - bonus;
                if best[k][to].is_none_or(|(c, _)| here < c) {
                    best[k][to] = Some((here, from));
                }
            }
        }
    }
    let mut starts = Vec::with_capacity(count);
    let mut at = n;
    for k in (1..=count).rev() {
        let Some((_, from)) = best[k][at] else { return vec![words] };
        starts.push(from);
        at = from;
    }
    let mut rest = words;
    let mut out = Vec::with_capacity(count);
    for &from in &starts {
        out.push(rest.split_off(from));
    }
    out.reverse();
    out
}

fn last_mark(text: &str) -> Option<char> {
    text.trim_end().trim_end_matches(['"', '\'', '”', '’', '»', ')', '」', '』']).chars().last()
}

fn ends_sentence(text: &str) -> bool {
    matches!(last_mark(text), Some('.' | '!' | '?' | '…' | '。' | '！' | '？'))
}

fn ends_clause(text: &str) -> bool {
    matches!(last_mark(text), Some(',' | ';' | ':' | '，' | '、' | '；' | '：'))
}
