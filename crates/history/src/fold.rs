//! The folding that makes search forgiving about case, accents and the four Turkish i's.
//!
//! SQLite's `unicode61` tokenizer folds case one code point at a time with Unicode's default
//! (non-Turkish) rules, and that breaks Turkish in both directions: `I` becomes `i` while `ı`
//! stays `ı`, so "IŞIK" and "ışık" never meet, and `İ` keeps its dot as a separate mark. A Turkish
//! reader expects the opposite of a strict Turkish lower-casing too, because captures mix English
//! and Turkish ("INVOICE" lower-cased the Turkish way is "ınvoıce"), OCR engines confuse dotted and
//! dotless letters, and many people type Turkish without its letters on an English keyboard.
//!
//! So the history keeps a folded shadow of every searchable field in the FTS index, made here in
//! Rust, and folds the query the same way:
//!
//! - compatibility decomposition (NFKD), then every combining mark is dropped, so `ş` → `s`,
//!   `ğ` → `g`, `ç` → `c`, `ö` → `o`, `ü` → `u`, `â` → `a`, `ﬁ` → `fi`, and `İ` → `I`;
//! - `I`, `İ`, `ı` and `i` all become `i`, whatever the language, and every other letter is
//!   lower-cased with the default rules.
//!
//! "ışık", "IŞIK", "Işık" and "isik" therefore find each other, and so do "İstanbul",
//! "ISTANBUL" and "istanbul". The price is that "kır" also finds "kir", which a search box can
//! afford. [`FOLD_VERSION`] changes whenever this mapping changes, and the store rebuilds the index
//! when it finds an index made by another version.
//!
//! Chinese and Japanese leave no space between words, so `unicode61` would keep a whole run such
//! as "明日の会議資料" as one token, and "会議" would never be found inside it. Every Han, kana and
//! Hangul character ([`is_cjk`]) therefore stands as a token of its own in the index and in the
//! query, and a query of several such characters is a phrase of single characters, which matches
//! them only side by side, wherever the run holds them.

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Bumped whenever [`fold`] maps any character differently, so old indexes are rebuilt.
pub const FOLD_VERSION: i64 = 2;

/// The folded form of `text`, as the search index stores it and as a query is matched: folded
/// for case, accents and the Turkish i's, with a space on both sides of every CJK character.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        fold_char(c, &mut |f| {
            if is_cjk(f) {
                out.push(' ');
                out.push(f);
                out.push(' ');
            } else {
                out.push(f);
            }
        });
    }
    out
}

/// `text` folded as [`fold`] does, without the spaces around CJK characters, for sorting by name:
/// the spaces would put every name that starts with a CJK character first.
pub fn fold_for_sorting(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        fold_char(c, &mut |f| out.push(f));
    }
    out
}

/// Whether `c` belongs to a script written without spaces between words, as [`fold`] leaves it:
/// Han ideographs, hiragana, katakana and Hangul (whose syllables fold into their jamo).
pub fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{11FF}'     // Hangul jamo
        | '\u{2E80}'..='\u{2FDF}'   // CJK and Kangxi radicals
        | '\u{3005}'..='\u{3007}'   // 々 〆 〇
        | '\u{303B}'..='\u{303C}'   // 〻 〼
        | '\u{3040}'..='\u{30FF}'   // hiragana, katakana
        | '\u{3130}'..='\u{318F}'   // Hangul compatibility jamo
        | '\u{31F0}'..='\u{31FF}'   // katakana phonetic extensions
        | '\u{3400}'..='\u{4DBF}'   // CJK extension A
        | '\u{4E00}'..='\u{9FFF}'   // CJK unified ideographs
        | '\u{A960}'..='\u{A97F}'   // Hangul jamo extended A
        | '\u{AC00}'..='\u{D7FF}'   // Hangul syllables, jamo extended B
        | '\u{F900}'..='\u{FAFF}'   // CJK compatibility ideographs
        | '\u{FF65}'..='\u{FFDC}'   // half-width katakana and Hangul
        | '\u{1B000}'..='\u{1B16F}' // kana supplement and extended A
        | '\u{20000}'..='\u{3FFFF}' // CJK extensions B to H, compatibility supplement
    )
}

fn fold_char(c: char, emit: &mut impl FnMut(char)) {
    // ASCII is the bulk of most captures and folds without the decomposition tables.
    if c.is_ascii() {
        emit(c.to_ascii_lowercase());
        return;
    }
    // `ı` has no decomposition, and lower-casing leaves it alone, so it is mapped by hand.
    if c == 'ı' {
        emit('i');
        return;
    }
    for d in std::iter::once(c).nfkd() {
        if is_combining_mark(d) {
            continue;
        }
        if d == 'ı' {
            emit('i');
            continue;
        }
        for l in d.to_lowercase() {
            if !is_combining_mark(l) {
                emit(l);
            }
        }
    }
}

/// `text` folded, with the UTF-16 range of the original character each folded character came
/// from, so a match found in the folded text can be marked in the original.
pub fn fold_mapped(text: &str) -> (Vec<char>, Vec<(u32, u32)>) {
    let mut chars = Vec::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len());
    let mut at = 0u32;
    for c in text.chars() {
        let end = at + c.len_utf16() as u32;
        fold_char(c, &mut |f| {
            chars.push(f);
            origin.push((at, end));
        });
        at = end;
    }
    (chars, origin)
}

/// Whether the tokenizer (`unicode61`) keeps `c` inside a token. Everything else separates
/// tokens, which is how the query is split too.
pub fn is_token_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// A query as the user typed it, folded and split: groups are separated by white space, and the
/// tokens of one group (`sk_live`, `INV-2026-0412`) must stand next to each other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub groups: Vec<Vec<String>>,
}

impl Query {
    pub fn parse(text: &str) -> Query {
        let groups = text
            .split_whitespace()
            .map(|word| {
                fold(word).split(|c: char| !is_token_char(c)).filter(|t| !t.is_empty()).map(str::to_string).collect()
            })
            .filter(|tokens: &Vec<String>| !tokens.is_empty())
            .collect();
        Query { groups }
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// What marking looks for in the original text: every token of every group, except that the
    /// single-character tokens of a CJK run are looked for together, as the phrase that matched.
    pub fn needles(&self) -> Vec<String> {
        let mut needles: Vec<String> = Vec::new();
        for group in &self.groups {
            let mut run_open = false;
            for token in group {
                let cjk = token.chars().all(is_cjk);
                match needles.last_mut() {
                    Some(last) if cjk && run_open => last.push_str(token),
                    _ => needles.push(token.clone()),
                }
                run_open = cjk;
            }
        }
        needles
    }

    /// The FTS5 expression: each group is a phrase whose last token matches as a prefix, so the
    /// results follow every keystroke, and the groups must all match. Tokens are quoted, so no
    /// character the user types is read as FTS5 syntax.
    pub fn fts_expression(&self) -> String {
        self.groups
            .iter()
            .map(|tokens| format!("\"{}\"*", tokens.join(" ").replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

/// The UTF-16 ranges of `text` where a query token starts a word, as `(start, end)` pairs in
/// order and merged where they touch. Every CJK character starts a word, as it does in the index.
pub fn match_ranges(text: &str, query: &Query) -> Vec<(u32, u32)> {
    let (chars, origin) = fold_mapped(text);
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for token in query.needles() {
        let token: Vec<char> = token.chars().collect();
        if token.is_empty() || token.len() > chars.len() {
            continue;
        }
        for start in 0..=chars.len() - token.len() {
            let at_word_start =
                start == 0 || !is_token_char(chars[start - 1]) || is_cjk(chars[start - 1]) || is_cjk(chars[start]);
            if at_word_start && chars[start..start + token.len()] == token[..] {
                ranges.push((origin[start].0, origin[start + token.len() - 1].1));
            }
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Whether some token of the query starts a word of `text`.
pub fn matches(text: &str, query: &Query) -> bool {
    !match_ranges(text, query).is_empty()
}
