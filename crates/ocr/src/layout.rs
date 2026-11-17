//! Reading order: turns the pieces of text an engine reports, in whatever order it reports them,
//! into lines, paragraphs, columns, tables and code in the order a person reads them.
//!
//! This step is plain Rust over boxes and knows nothing about the engine, so Vision, PP-OCR and
//! Windows.Media.Ocr get the same reading order. Every threshold below is a multiple of the text
//! height, so the rules hold at every scale; the sample set in `tests/samples` measures them.
//!
//! # The steps
//!
//! 1. **Words.** Each engine line keeps its own word boxes when they are plausible; otherwise its
//!    box is shared out over its words by their length.
//! 2. **Segments.** A line is split wherever two words stand further apart than one text height,
//!    because engines sometimes read across a column gutter or a table as one line. A segment is
//!    a run of words that certainly belong together.
//! 3. **Channels.** Two segments on one row with a wide gap between them seed a channel: a
//!    vertical strip of white space. Seeds above each other join into one channel while nothing
//!    crosses the strip, and the channel then grows up and down through rows whose text lines up
//!    with its edges. A channel is a **column** gutter when the text on both sides reads as prose
//!    (four or more words per segment) or when the rows on both sides do not share baselines (a
//!    sidebar beside content); it is a **table** gutter when the rows on both sides share their
//!    baselines and are short (cells, a form's labels and values, buttons in a row).
//! 4. **Cuts.** The page is cut recursively, XY-cut style. A node is cut vertically at every
//!    full-height gap that is a column gutter, and the parts are read left to right; otherwise it
//!    is cut horizontally at every full-width gap that does not lie inside a column channel, and
//!    the parts are read top to bottom. Forbidding horizontal cuts inside a column keeps the
//!    columns whole even when their paragraph gaps line up, and a heading that spans the columns
//!    still comes first, because it crosses the gutter and ends the channel. What cannot be cut
//!    is a row; its segments are joined left to right, with a tab between table cells.
//! 5. **Paragraphs.** Within one column, a gap clearly wider than the usual line gap, a change of
//!    text height or a row that does not overlap the one above starts a new paragraph.
//! 6. **Kinds.** A paragraph whose words have widths proportional to their length (monospaced
//!    text) is code: it keeps its line breaks, its indentation is rebuilt as spaces from the
//!    measured glyph advance, the spaces inside a line are rebuilt the same way, and blank lines
//!    come back from the gaps. Rows with cells are a table. Everything else is prose.
//! 7. **Text.** Lines are joined with line breaks, or, when the settings drop line breaks, the
//!    lines of a prose paragraph are joined with spaces. A line that ends in a hyphen is joined
//!    to the next without the hyphen only when that is safe (see [`join_hyphenated`]); otherwise
//!    the hyphen stays and only the line break goes. Paragraphs are separated by a blank line.

use std::cmp::Ordering;

use crate::{BlockKind, Bounds, OcrLine, OcrWord, RawLine, RawText};

/// What the text should look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutOptions {
    /// Keeps every line break of a prose paragraph; when off, its lines are joined with spaces.
    pub keep_line_breaks: bool,
}

/// The lines in reading order and the text they make.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub lines: Vec<OcrLine>,
    pub text: String,
}

/// Puts the engine's lines into reading order and writes the text.
pub fn arrange(raw: &RawText, options: &LayoutOptions) -> Layout {
    let segments = segments_of(&raw.lines);
    if segments.is_empty() {
        return Layout::default();
    }
    let channels = channels_of(&segments);
    let all: Vec<usize> = (0..segments.len()).collect();
    let tree = cut(&segments, &channels, &all, 0);
    let mut runs: Vec<Vec<Row>> = Vec::new();
    let mut current = Vec::new();
    collect(&segments, &tree, &mut runs, &mut current);
    if !current.is_empty() {
        runs.push(current);
    }
    write(&segments, &runs, options)
}

// ------------------------------------------------------------------------------ words, segments

#[derive(Clone, Debug)]
struct Segment {
    words: Vec<OcrWord>,
    bounds: Bounds,
    confidence: f64,
}

impl Segment {
    fn new(words: Vec<OcrWord>, confidence: f64) -> Segment {
        let bounds = words.iter().skip(1).fold(words[0].bounds, |b, w| b.union(&w.bounds));
        Segment { words, bounds, confidence }
    }

    fn height(&self) -> f64 {
        self.bounds.height.max(1.0)
    }

    fn word_count(&self) -> usize {
        self.words.len()
    }
}

/// Words a word box can be trusted for: every word has a box of its own, in order, and not every
/// box is the whole line's.
fn plausible(line: &RawLine) -> bool {
    let count = line.text.split_whitespace().count();
    if line.words.len() != count || count == 0 {
        return false;
    }
    if line.words.iter().any(|w| w.bounds.width <= 0.0 || w.bounds.height <= 0.0) {
        return false;
    }
    let tolerance = line.bounds.height * 0.5;
    let ordered = line.words.windows(2).all(|p| p[1].bounds.x >= p[0].bounds.x - tolerance);
    let all_whole = count > 1 && line.words.iter().all(|w| w.bounds.width >= line.bounds.width * 0.9);
    ordered && !all_whole
}

/// Shares the line's box out over its words by their length in characters, spaces included.
fn spread(line: &RawLine) -> Vec<OcrWord> {
    let total = line.text.chars().count().max(1) as f64;
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    let chars: Vec<char> = line.text.chars().collect();
    for i in 0..=chars.len() {
        let space = i == chars.len() || chars[i].is_whitespace();
        match (space, start) {
            (true, Some(from)) => {
                let text: String = chars[from..i].iter().collect();
                let x = line.bounds.x + line.bounds.width * from as f64 / total;
                let width = line.bounds.width * (i - from) as f64 / total;
                let bounds = Bounds::new(x, line.bounds.y, width, line.bounds.height);
                words.push(OcrWord { text, bounds, confidence: line.confidence });
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    words
}

/// Splits every engine line into segments at gaps wider than one text height.
fn segments_of(lines: &[RawLine]) -> Vec<Segment> {
    let mut segments = Vec::new();
    for line in lines {
        let words = if plausible(line) { line.words.clone() } else { spread(line) };
        if words.is_empty() {
            continue;
        }
        let height = line.bounds.height.max(1.0);
        let mut run: Vec<OcrWord> = Vec::new();
        for word in words {
            if let Some(last) = run.last() {
                let gap = word.bounds.x - last.bounds.right();
                if gap > height {
                    segments.push(Segment::new(std::mem::take(&mut run), line.confidence));
                }
            }
            run.push(word);
        }
        if !run.is_empty() {
            segments.push(Segment::new(run, line.confidence));
        }
    }
    segments
}

// ----------------------------------------------------------------------------------- geometry

fn vertical_overlap(a: &Bounds, b: &Bounds) -> f64 {
    (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0)
}

/// Whether two boxes sit on one row: they share at least `share` of the smaller height.
fn same_row(a: &Bounds, b: &Bounds, share: f64) -> bool {
    vertical_overlap(a, b) >= share * a.height.min(b.height).max(1.0)
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    Some(values[values.len() / 2])
}

fn median_height(segments: &[Segment], ids: &[usize]) -> f64 {
    let mut heights: Vec<f64> = ids.iter().map(|&i| segments[i].height()).collect();
    median(&mut heights).unwrap_or(1.0)
}

/// The gaps between the projections of `ids` on one axis, as (start, end) pairs.
fn gaps(segments: &[Segment], ids: &[usize], horizontal: bool) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = ids
        .iter()
        .map(|&i| {
            let b = &segments[i].bounds;
            if horizontal {
                (b.x, b.right())
            } else {
                (b.y, b.bottom())
            }
        })
        .collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let mut out = Vec::new();
    let mut reach = f64::NEG_INFINITY;
    for (start, end) in spans {
        if reach > f64::NEG_INFINITY && start > reach {
            out.push((reach, start));
        }
        reach = reach.max(end);
    }
    out
}

// ----------------------------------------------------------------------------------- channels

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gutter {
    Column,
    Table,
}

/// A vertical strip of white space between text on its left and right.
#[derive(Clone, Debug)]
struct Channel {
    lo: f64,
    hi: f64,
    top: f64,
    bottom: f64,
    /// Where the text on each side starts, for growing the channel through aligned rows.
    left_edge: f64,
    right_edge: f64,
    min_width: f64,
    kind: Gutter,
}

/// The segment nearest to `a` on its right, on the same row, at least one text height away.
fn right_neighbour(segments: &[Segment], a: usize) -> Option<usize> {
    let sa = &segments[a].bounds;
    let mut best: Option<usize> = None;
    for (b, sb) in segments.iter().enumerate() {
        let sb = &sb.bounds;
        if b == a || sb.x < sa.right() || !same_row(sa, sb, 0.4) {
            continue;
        }
        if best.is_none_or(|c| sb.x < segments[c].bounds.x) {
            best = Some(b);
        }
    }
    let b = best?;
    let gap = segments[b].bounds.x - sa.right();
    (gap >= segments[a].height().max(segments[b].height())).then_some(b)
}

/// Whether any segment crosses the strip `lo..hi` between `y0` and `y1`.
fn crossed(segments: &[Segment], lo: f64, hi: f64, y0: f64, y1: f64) -> bool {
    segments.iter().any(|s| {
        let b = &s.bounds;
        b.bottom() > y0 && b.y < y1 && b.x < hi && b.right() > lo
    })
}

fn channels_of(segments: &[Segment]) -> Vec<Channel> {
    // Seeds: a wide gap between two neighbours on one row.
    let mut seeds: Vec<(usize, usize)> =
        (0..segments.len()).filter_map(|a| right_neighbour(segments, a).map(|b| (a, b))).collect();
    seeds.sort_by(|x, y| segments[x.0].bounds.y.partial_cmp(&segments[y.0].bounds.y).unwrap_or(Ordering::Equal));

    struct Draft {
        lo: f64,
        hi: f64,
        top: f64,
        bottom: f64,
        min_width: f64,
        lefts: Vec<f64>,
        rights: Vec<f64>,
    }
    let mut drafts: Vec<Draft> = Vec::new();
    for (a, b) in seeds {
        let (sa, sb) = (&segments[a].bounds, &segments[b].bounds);
        let (lo, hi) = (sa.right(), sb.x);
        let (top, bottom) = (sa.y.min(sb.y), sa.bottom().max(sb.bottom()));
        let min_width = segments[a].height().max(segments[b].height());
        let joined = drafts.iter_mut().find(|d| {
            let (l, h) = (d.lo.max(lo), d.hi.min(hi));
            h - l >= d.min_width.min(min_width) && !crossed(segments, l, h, d.bottom.min(top), top.max(d.bottom))
        });
        match joined {
            Some(d) => {
                d.lo = d.lo.max(lo);
                d.hi = d.hi.min(hi);
                d.top = d.top.min(top);
                d.bottom = d.bottom.max(bottom);
                d.min_width = d.min_width.min(min_width);
                d.lefts.push(sa.x);
                d.rights.push(sb.x);
            }
            None => drafts.push(Draft { lo, hi, top, bottom, min_width, lefts: vec![sa.x], rights: vec![sb.x] }),
        }
    }

    drafts
        .into_iter()
        .map(|mut d| {
            let mut channel = Channel {
                lo: d.lo,
                hi: d.hi,
                top: d.top,
                bottom: d.bottom,
                left_edge: median(&mut d.lefts).unwrap_or(d.lo),
                right_edge: median(&mut d.rights).unwrap_or(d.hi),
                min_width: d.min_width,
                kind: Gutter::Column,
            };
            grow(segments, &mut channel, true);
            grow(segments, &mut channel, false);
            channel.kind = classify(segments, &channel);
            channel
        })
        .collect()
}

/// Grows a channel up or down through rows whose text lines up with the column edges and that
/// do not cross it; a row may narrow the channel as long as it stays wide enough.
fn grow(segments: &[Segment], channel: &mut Channel, up: bool) {
    let mut candidates: Vec<usize> = (0..segments.len())
        .filter(|&i| {
            let b = &segments[i].bounds;
            if up {
                b.center_y() < channel.top
            } else {
                b.center_y() > channel.bottom
            }
        })
        .collect();
    candidates.sort_by(|&x, &y| {
        let (bx, by) = (&segments[x].bounds, &segments[y].bounds);
        let order = bx.center_y().partial_cmp(&by.center_y()).unwrap_or(Ordering::Equal);
        if up {
            order.reverse()
        } else {
            order
        }
    });
    let mut i = 0;
    while i < candidates.len() {
        // One row: the next segment and those that share its row.
        let first = segments[candidates[i]].bounds;
        let mut row = vec![candidates[i]];
        let mut j = i + 1;
        while j < candidates.len() && same_row(&first, &segments[candidates[j]].bounds, 0.3) {
            row.push(candidates[j]);
            j += 1;
        }
        let (mut lo, mut hi) = (channel.lo, channel.hi);
        for &s in &row {
            let b = &segments[s].bounds;
            if b.x < hi && b.right() > lo {
                if b.x <= lo {
                    lo = lo.max(b.right());
                } else if b.right() >= hi {
                    hi = hi.min(b.x);
                } else {
                    return;
                }
            }
        }
        if hi - lo < channel.min_width {
            return;
        }
        let tolerance = 0.75 * first.height.max(1.0);
        let aligned = row.iter().any(|&s| {
            let b = &segments[s].bounds;
            (b.right() <= lo + 0.5 && (b.x - channel.left_edge).abs() <= tolerance)
                || (b.x >= hi - 0.5 && (b.x - channel.right_edge).abs() <= tolerance)
        });
        if !aligned {
            return;
        }
        channel.lo = lo;
        channel.hi = hi;
        for &s in &row {
            let b = &segments[s].bounds;
            channel.top = channel.top.min(b.y);
            channel.bottom = channel.bottom.max(b.bottom());
        }
        i = j;
    }
}

/// The segments that flank the strip `lo..hi` between `top` and `bottom`: on each row, the one
/// nearest to the strip on each side.
fn flanks(segments: &[Segment], ids: &[usize], lo: f64, hi: f64, top: f64, bottom: f64) -> (Vec<usize>, Vec<usize>) {
    let inside = |i: usize| {
        let c = segments[i].bounds.center_y();
        c >= top && c <= bottom
    };
    let left: Vec<usize> =
        ids.iter().copied().filter(|&i| inside(i) && segments[i].bounds.right() <= lo + 0.5).collect();
    let right: Vec<usize> = ids.iter().copied().filter(|&i| inside(i) && segments[i].bounds.x >= hi - 0.5).collect();
    let nearest_left = left
        .iter()
        .copied()
        .filter(|&s| {
            !left.iter().any(|&t| {
                t != s
                    && same_row(&segments[s].bounds, &segments[t].bounds, 0.4)
                    && segments[t].bounds.x > segments[s].bounds.x
            })
        })
        .collect();
    let nearest_right = right
        .iter()
        .copied()
        .filter(|&s| {
            !right.iter().any(|&t| {
                t != s
                    && same_row(&segments[s].bounds, &segments[t].bounds, 0.4)
                    && segments[t].bounds.x < segments[s].bounds.x
            })
        })
        .collect();
    (nearest_left, nearest_right)
}

fn classify(segments: &[Segment], channel: &Channel) -> Gutter {
    let all: Vec<usize> = (0..segments.len()).collect();
    let (left, right) = flanks(segments, &all, channel.lo, channel.hi, channel.top, channel.bottom);
    classify_flanks(segments, &left, &right)
}

/// Column or table, from the text on both sides of a gap: prose on both sides is a column
/// gutter; short rows that share their baselines are a table; anything else is a column gutter.
fn classify_flanks(segments: &[Segment], left: &[usize], right: &[usize]) -> Gutter {
    if left.is_empty() || right.is_empty() {
        return Gutter::Column;
    }
    let words = |ids: &[usize]| {
        let mut counts: Vec<f64> = ids.iter().map(|&i| segments[i].word_count() as f64).collect();
        median(&mut counts).unwrap_or(0.0)
    };
    if words(left) >= 4.0 && words(right) >= 4.0 {
        return Gutter::Column;
    }
    // Only the stretch where both sides have text says whether their rows line up.
    let span = |ids: &[usize]| {
        ids.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(t, b), &i| {
            (t.min(segments[i].bounds.y), b.max(segments[i].bounds.bottom()))
        })
    };
    let ((lt, lb), (rt, rb)) = (span(left), span(right));
    let (top, bottom) = (lt.max(rt), lb.min(rb));
    let within = |ids: &[usize]| -> Vec<usize> {
        ids.iter()
            .copied()
            .filter(|&i| {
                let c = segments[i].bounds.center_y();
                c >= top - 0.5 && c <= bottom + 0.5
            })
            .collect()
    };
    let (left, right) = (within(left), within(right));
    if left.is_empty() || right.is_empty() {
        return Gutter::Column;
    }
    let partnered = |from: &[usize], to: &[usize]| {
        let hits = from
            .iter()
            .filter(|&&a| {
                let sa = &segments[a];
                to.iter().any(|&b| {
                    let sb = &segments[b];
                    // Loose: a cell without descenders ("On") has a shorter box than its
                    // label ("Keep line breaks"), and small text has looser boxes still.
                    same_row(&sa.bounds, &sb.bounds, 0.5)
                        && sa.height().max(sb.height()) <= 2.0 * sa.height().min(sb.height())
                })
            })
            .count();
        hits as f64 / from.len() as f64
    };
    if partnered(&left, &right) >= 0.6 && partnered(&right, &left) >= 0.6 {
        Gutter::Table
    } else {
        Gutter::Column
    }
}

// --------------------------------------------------------------------------------------- cuts

enum Tree {
    /// Segments that cannot be cut any further: one row, or a tangle read row by row.
    Leaf(Vec<usize>),
    /// Parts read left to right, each in its own column.
    Columns(Vec<Tree>),
    /// Parts read top to bottom within one column.
    Stack(Vec<Tree>),
}

fn cut(segments: &[Segment], channels: &[Channel], ids: &[usize], depth: usize) -> Tree {
    if ids.len() <= 1 || depth > 64 {
        return Tree::Leaf(ids.to_vec());
    }
    let height = median_height(segments, ids);
    let (top, bottom) = ids.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(t, b), &i| {
        (t.min(segments[i].bounds.y), b.max(segments[i].bounds.bottom()))
    });

    // Vertical cuts at column gutters.
    let columns: Vec<(f64, f64)> = gaps(segments, ids, true)
        .into_iter()
        .filter(|&(lo, hi)| hi - lo >= height)
        .filter(|&(lo, hi)| {
            let (left, right) = flanks(segments, ids, lo, hi, top, bottom);
            classify_flanks(segments, &left, &right) == Gutter::Column
        })
        .collect();

    // Horizontal cuts everywhere except inside a column gutter that runs through this node.
    let (left, right) = ids.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, r), &i| {
        (l.min(segments[i].bounds.x), r.max(segments[i].bounds.right()))
    });
    let rows: Vec<(f64, f64)> = gaps(segments, ids, false)
        .into_iter()
        .filter(|&(y0, y1)| {
            !channels
                .iter()
                .any(|c| c.kind == Gutter::Column && c.lo > left && c.hi < right && c.top < y0 && y1 < c.bottom)
        })
        .collect();

    // A gutter whose channel ends before the node does (a window title above a sidebar and its
    // content) waits until the horizontal cuts have taken off what lies above or below it, so
    // that text is read first or last instead of being glued to one of the columns.
    let whole = |&(lo, hi): &(f64, f64)| {
        let channel = channels
            .iter()
            .filter(|c| c.kind == Gutter::Column && c.lo < hi && c.hi > lo)
            .max_by(|a, b| (a.bottom - a.top).partial_cmp(&(b.bottom - b.top)).unwrap_or(Ordering::Equal));
        channel.is_none_or(|c| {
            let slack = 0.5 * height;
            ids.iter().all(|&i| {
                let y = segments[i].bounds.center_y();
                y >= c.top - slack && y <= c.bottom + slack
            })
        })
    };
    if !columns.is_empty() && (rows.is_empty() || columns.iter().all(whole)) {
        let mut parts: Vec<Vec<usize>> = vec![Vec::new(); columns.len() + 1];
        for &i in ids {
            let x = segments[i].bounds.x;
            let part = columns.iter().take_while(|&&(lo, _)| x >= lo).count();
            parts[part].push(i);
        }
        return Tree::Columns(
            parts.into_iter().filter(|p| !p.is_empty()).map(|p| cut(segments, channels, &p, depth + 1)).collect(),
        );
    }
    if !rows.is_empty() {
        let mut parts: Vec<Vec<usize>> = vec![Vec::new(); rows.len() + 1];
        for &i in ids {
            let y = segments[i].bounds.y;
            let part = rows.iter().take_while(|&&(y0, _)| y >= y0).count();
            parts[part].push(i);
        }
        return Tree::Stack(
            parts.into_iter().filter(|p| !p.is_empty()).map(|p| cut(segments, channels, &p, depth + 1)).collect(),
        );
    }
    Tree::Leaf(ids.to_vec())
}

// ----------------------------------------------------------------------------------------- rows

/// Segments on one row, left to right.
#[derive(Clone, Debug)]
struct Row {
    segments: Vec<usize>,
    bounds: Bounds,
    height: f64,
}

fn rows_of(segments: &[Segment], ids: &[usize]) -> Vec<Row> {
    let mut sorted = ids.to_vec();
    sorted.sort_by(|&a, &b| {
        segments[a].bounds.center_y().partial_cmp(&segments[b].bounds.center_y()).unwrap_or(Ordering::Equal)
    });
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for i in sorted {
        // Two pieces that overlap side by side are two rows, however much the engine's loose
        // boxes overlap in height.
        let joins = rows.last().is_some_and(|row| {
            let span = row.iter().skip(1).fold(segments[row[0]].bounds, |b, &j| b.union(&segments[j].bounds));
            let b = &segments[i].bounds;
            same_row(&span, b, 0.5)
                && !row.iter().any(|&j| {
                    let o = &segments[j].bounds;
                    b.x < o.right() - 0.25 * o.height && b.right() > o.x + 0.25 * o.height
                })
        });
        match rows.last_mut() {
            Some(row) if joins => row.push(i),
            _ => rows.push(vec![i]),
        }
    }
    rows.into_iter()
        .map(|mut row| {
            row.sort_by(|&a, &b| segments[a].bounds.x.partial_cmp(&segments[b].bounds.x).unwrap_or(Ordering::Equal));
            let bounds = row.iter().skip(1).fold(segments[row[0]].bounds, |b, &j| b.union(&segments[j].bounds));
            let mut heights: Vec<f64> =
                row.iter().flat_map(|&j| segments[j].words.iter().map(|w| w.bounds.height)).collect();
            let height = median(&mut heights).unwrap_or(bounds.height).max(1.0);
            Row { segments: row, bounds, height }
        })
        .collect()
}

/// Flattens the cut tree into runs: each run is the rows of one column, top to bottom.
fn collect(segments: &[Segment], tree: &Tree, runs: &mut Vec<Vec<Row>>, current: &mut Vec<Row>) {
    match tree {
        Tree::Leaf(ids) => current.extend(rows_of(segments, ids)),
        Tree::Stack(parts) => {
            for part in parts {
                collect(segments, part, runs, current);
            }
        }
        Tree::Columns(parts) => {
            if !current.is_empty() {
                runs.push(std::mem::take(current));
            }
            for part in parts {
                let mut inner = Vec::new();
                collect(segments, part, runs, &mut inner);
                if !inner.is_empty() {
                    runs.push(inner);
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------------- paragraphs

/// A paragraph: consecutive rows of one run.
struct Paragraph {
    rows: Vec<Row>,
    kind: BlockKind,
    /// The glyph advance and left edge of a code block, for its spaces.
    code: Option<Monospace>,
}

#[derive(Clone, Copy, Debug)]
struct Monospace {
    /// The width of one character cell in pixels.
    advance: f64,
    /// Where column 0 starts: the left edge of the least indented row.
    left: f64,
    /// The distance between the tops of consecutive lines, for blank lines.
    pitch: f64,
}

fn paragraphs_of(segments: &[Segment], run: Vec<Row>) -> Vec<Paragraph> {
    // Engines box lines loosely (one line's box can be half again as tall as its neighbour's),
    // so the rhythm is measured between line centres, which wobble far less than edges.
    let mut steps: Vec<f64> = run
        .windows(2)
        .filter(|p| similar(p[0].height, p[1].height))
        .map(|p| p[1].bounds.center_y() - p[0].bounds.center_y())
        .filter(|s| *s > 0.0)
        .collect();
    let usual = if steps.len() >= 2 { median(&mut steps) } else { None };

    let mut groups: Vec<Vec<Row>> = Vec::new();
    for row in run {
        let breaks = match groups.last().and_then(|g| g.last()) {
            None => true,
            Some(prev) => {
                let h = prev.height.max(row.height);
                let wide = match usual {
                    Some(usual) => row.bounds.center_y() - prev.bounds.center_y() > usual * 1.3,
                    None => row.bounds.y - prev.bounds.bottom() > 0.8 * h,
                };
                let overlap = row.bounds.x < prev.bounds.right() && row.bounds.right() > prev.bounds.x;
                wide || !similar(prev.height, row.height) || !overlap
            }
        };
        if breaks {
            groups.push(vec![row]);
        } else if let Some(group) = groups.last_mut() {
            group.push(row);
        }
    }

    let mut paragraphs: Vec<Paragraph> = groups
        .into_iter()
        .map(|rows| {
            let kind = if rows.iter().any(|r| cells(segments, r) > 1) {
                BlockKind::Table
            } else {
                match code_verdict(segments, &rows) {
                    Some(true) => BlockKind::Code,
                    _ => BlockKind::Prose,
                }
            };
            Paragraph { rows, kind, code: None }
        })
        .collect();

    // A short paragraph that cannot tell by itself ("else:", "running 3 tests") is code when it
    // sits next to code of the same size in the same column.
    for _ in 0..2 {
        for i in 0..paragraphs.len() {
            if paragraphs[i].kind != BlockKind::Prose || code_verdict(segments, &paragraphs[i].rows).is_some() {
                continue;
            }
            let height = paragraphs[i].rows[0].height;
            let near = |j: usize| {
                paragraphs
                    .get(j)
                    .is_some_and(|p: &Paragraph| p.kind == BlockKind::Code && similar(p.rows[0].height, height))
            };
            if near(i + 1) || (i > 0 && near(i - 1)) {
                paragraphs[i].kind = BlockKind::Code;
            }
        }
    }

    // Code that a blank line split in two is one block: it shares one left edge for its indentation.
    let mut merged: Vec<Paragraph> = Vec::new();
    for paragraph in paragraphs.drain(..) {
        match merged.last_mut() {
            Some(last) if last.kind == BlockKind::Code && paragraph.kind == BlockKind::Code => {
                last.rows.extend(paragraph.rows)
            }
            _ => merged.push(paragraph),
        }
    }
    for paragraph in &mut merged {
        if paragraph.kind == BlockKind::Code {
            paragraph.code = monospace(segments, &paragraph.rows);
        }
    }
    merged
}

/// Whether two text heights belong to one size of type; engines' boxes wobble by a third.
fn similar(a: f64, b: f64) -> bool {
    a.max(b) / a.min(b).max(1.0) <= 1.5
}

/// How many cells a row has: segments separated by more than one text height.
fn cells(segments: &[Segment], row: &Row) -> usize {
    1 + row.segments.windows(2).filter(|p| segments[p[1]].bounds.x - segments[p[0]].bounds.right() > row.height).count()
}

/// Whether the words' widths grow with their length at one fixed advance, as in a monospaced
/// font: the median word deviates from the fitted width by less than 7 %.
/// Whether a paragraph is code: `Some(true)` or `Some(false)` when it can tell, `None` when it
/// is too short to say. Three signals count:
///
/// - the glyph ratio ([`glyph_ratio`]): 0.85 or more is a monospaced font, 0.7 or less a
///   proportional one;
/// - the share of characters that prose rarely uses (brackets, `=`, `_`, `#`, `$`, `/`, quotes);
/// - indentation: rows that start at two or more distinct steps from the left edge.
fn code_verdict(segments: &[Segment], rows: &[Row]) -> Option<bool> {
    let text: String = rows
        .iter()
        .flat_map(|r| r.segments.iter().flat_map(|&s| segments[s].words.iter().map(|w| w.text.as_str())))
        .collect();
    let total = text.chars().count().max(1) as f64;
    let symbols = text.chars().filter(|c| "(){}[]<>=;_#$/\\|&*+\"`~@%^".contains(*c)).count() as f64;
    let density = symbols / total;
    let ratio = glyph_ratio(segments, rows);
    let indented = indent_levels(rows) >= 2;
    match ratio {
        Some(r) if r >= 0.85 => Some(true),
        _ if density >= 0.10 || (density >= 0.05 && indented) => Some(true),
        Some(r) if r <= 0.7 => Some(false),
        Some(_) => Some(density >= 0.03),
        None => None,
    }
}

/// How many distinct left edges the rows start at, one text height apart or more.
fn indent_levels(rows: &[Row]) -> usize {
    let mut lefts: Vec<(f64, f64)> = rows.iter().map(|r| (r.bounds.x, r.height)).collect();
    lefts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let mut levels = 0;
    let mut last = f64::NEG_INFINITY;
    for (x, h) in lefts {
        if x - last > h {
            levels += 1;
            last = x;
        }
    }
    levels
}

/// Letters that a proportional font sets at about half the width of an ordinary letter, and a
/// monospaced font sets at the same width as every other one.
fn narrow(c: char) -> bool {
    matches!(c, 'i' | 'l' | 'j' | 't' | 'f' | 'r' | 'I' | 'ı' | 'İ' | 'í' | 'ì' | 'î' | 'ï' | '1')
}

/// How wide a narrow glyph is next to an ordinary one in this text: about 1 in a monospaced
/// font and about 0.5 in a proportional one. Fitted by least squares over the words as
/// `width = narrow × a + other × b − bearing`, and returned as `a / b`. Words differ enough in
/// their share of narrow glyphs to tell the two apart, which whole lines do not.
fn glyph_ratio(segments: &[Segment], rows: &[Row]) -> Option<f64> {
    let words: Vec<([f64; 3], f64)> = rows
        .iter()
        .flat_map(|r| r.segments.iter().flat_map(|&s| segments[s].words.iter()))
        // Engines pad the boxes of one- and two-glyph words, and punctuation at a word's end
        // leaves part of its cell empty even in a monospaced font, so only words of three or
        // more letters and digits say how wide a glyph is.
        .filter(|w| w.text.chars().count() >= 3 && w.text.chars().all(char::is_alphanumeric))
        .map(|w| {
            let n1 = w.text.chars().filter(|&c| narrow(c)).count();
            let n2 = w.text.chars().count() - n1;
            ([n1 as f64, n2 as f64, -1.0], w.bounds.width)
        })
        .collect();
    let fit = |chosen: &[&([f64; 3], f64)]| -> Option<[f64; 3]> {
        let narrow_total: f64 = chosen.iter().map(|(x, _)| x[0]).sum();
        if narrow_total < 5.0 || chosen.len() < 6 {
            return None;
        }
        let mut m = [[0.0f64; 3]; 3];
        let mut v = [0.0f64; 3];
        for (x, w) in chosen {
            for i in 0..3 {
                for j in 0..3 {
                    m[i][j] += x[i] * x[j];
                }
                v[i] += x[i] * w;
            }
        }
        solve3(m, v)
    };
    let all: Vec<&([f64; 3], f64)> = words.iter().collect();
    let first = fit(&all)?;
    // A word the engine misread ("" for three quotes) does not match its own box; the
    // fit is made again without the words that stray furthest from the first one.
    let residual = |(x, w): &&([f64; 3], f64)| (first[0] * x[0] + first[1] * x[1] - first[2] - w).abs();
    let mut residuals: Vec<f64> = all.iter().map(residual).collect();
    let limit = 2.0 * median(&mut residuals).unwrap_or(0.0) + 1.0;
    let kept: Vec<&([f64; 3], f64)> = all.iter().copied().filter(|x| residual(x) <= limit).collect();
    let [a, b, _] = fit(&kept).unwrap_or(first);
    (b > 0.0).then(|| a / b)
}

/// Solves a 3 × 3 linear system by Cramer's rule.
fn solve3(m: [[f64; 3]; 3], v: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(&m);
    if d.abs() < 1e-9 {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, slot) in out.iter_mut().enumerate() {
        let mut mk = m;
        for (row, value) in mk.iter_mut().zip(v) {
            row[k] = value;
        }
        *slot = det(&mk) / d;
    }
    Some(out)
}

/// The character grid of a code block. Engines pad word boxes by a few pixels, but they pad
/// every word alike, so the advance comes from where words *start*: in a monospaced font, a word
/// that starts `k` characters after the first word of its row starts `k` advances after it.
fn monospace(segments: &[Segment], rows: &[Row]) -> Option<Monospace> {
    // Word starts and their character offsets in each row, fitted as `x = row start + advance ×
    // offset` with one start per row; a second pass drops the words a misread or a run of
    // several spaces put off the line.
    let rows_words: Vec<Vec<(f64, f64)>> = rows
        .iter()
        .map(|row| {
            let mut offset = 0usize;
            row.segments
                .iter()
                .flat_map(|&s| segments[s].words.iter())
                .map(|w| {
                    let point = (offset as f64, w.bounds.x);
                    offset += w.text.chars().count() + 1;
                    point
                })
                .collect()
        })
        .filter(|w: &Vec<(f64, f64)>| w.len() >= 2)
        .collect();
    let fit = |keep: &dyn Fn(f64, f64, f64) -> bool| -> Option<f64> {
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for words in &rows_words {
            let n = words.len() as f64;
            let (mk, mx) = words.iter().fold((0.0, 0.0), |(a, b), (k, x)| (a + k / n, b + x / n));
            for &(k, x) in words {
                if keep(k - mk, x - mx, mk) {
                    sxy += (k - mk) * (x - mx);
                    sxx += (k - mk) * (k - mk);
                }
            }
        }
        (sxx > 0.0).then(|| sxy / sxx).filter(|a| *a > 0.0)
    };
    let advance = match fit(&|_, _, _| true) {
        Some(first) => fit(&|dk, dx, _| (dx - first * dk).abs() <= first).unwrap_or(first),
        None => {
            let mut widths: Vec<f64> = rows
                .iter()
                .flat_map(|r| r.segments.iter().flat_map(|&s| segments[s].words.iter()))
                .filter(|w| w.text.chars().count() >= 4)
                .map(|w| w.bounds.width / w.text.chars().count() as f64)
                .collect();
            median(&mut widths)?
        }
    };
    let left = rows.iter().map(|r| r.bounds.x).fold(f64::INFINITY, f64::min);
    let mut steps: Vec<f64> = rows.windows(2).map(|p| p[1].bounds.y - p[0].bounds.y).filter(|s| *s > 0.0).collect();
    let pitch = median(&mut steps).unwrap_or(rows[0].height * 1.4);
    Some(Monospace { advance, left, pitch })
}

// ----------------------------------------------------------------------------------------- text

fn write(segments: &[Segment], runs: &[Vec<Row>], options: &LayoutOptions) -> Layout {
    let mut lines = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut block = 0u32;
    for run in runs {
        for paragraph in paragraphs_of(segments, run.clone()) {
            let mut texts = Vec::with_capacity(paragraph.rows.len());
            let mut previous: Option<&Row> = None;
            for row in &paragraph.rows {
                let text = match paragraph.code {
                    Some(mono) => code_line(segments, row, &mono),
                    None => row_text(segments, row, paragraph.kind),
                };
                if let (Some(mono), Some(prev)) = (paragraph.code, previous) {
                    // Blank lines come back from the gap they left.
                    let steps = ((row.bounds.y - prev.bounds.y) / mono.pitch).round() as i64;
                    for _ in 1..steps.clamp(1, 4) {
                        texts.push(String::new());
                    }
                }
                let words: Vec<OcrWord> =
                    row.segments.iter().flat_map(|&s| segments[s].words.iter().cloned()).collect();
                let confidence = weighted_confidence(segments, row);
                lines.push(OcrLine {
                    text: text.clone(),
                    bounds: row.bounds,
                    confidence,
                    words,
                    block,
                    kind: paragraph.kind,
                });
                texts.push(text);
                previous = Some(row);
            }
            let joined = if paragraph.kind == BlockKind::Prose && !options.keep_line_breaks {
                join_prose(&texts)
            } else {
                texts.join("\n")
            };
            blocks.push(joined);
            block += 1;
        }
    }
    Layout { lines, text: blocks.join("\n\n") }
}

fn weighted_confidence(segments: &[Segment], row: &Row) -> f64 {
    let (sum, weight) = row.segments.iter().fold((0.0, 0.0), |(s, w), &i| {
        let chars = segments[i].words.iter().map(|w| w.text.chars().count()).sum::<usize>().max(1) as f64;
        (s + segments[i].confidence * chars, w + chars)
    });
    if weight > 0.0 {
        sum / weight
    } else {
        0.0
    }
}

/// A row as text: words with single spaces, and a tab between cells.
fn row_text(segments: &[Segment], row: &Row, kind: BlockKind) -> String {
    let mut out = String::new();
    for (n, &s) in row.segments.iter().enumerate() {
        if n > 0 {
            let gap = segments[s].bounds.x - segments[row.segments[n - 1]].bounds.right();
            out.push(if kind == BlockKind::Table && gap > row.height { '\t' } else { ' ' });
        }
        let words: Vec<&str> = segments[s].words.iter().map(|w| w.text.as_str()).collect();
        out.push_str(&words.join(" "));
    }
    out
}

/// A row of code: indentation and inner spaces rebuilt from the glyph advance.
fn code_line(segments: &[Segment], row: &Row, mono: &Monospace) -> String {
    let words: Vec<&OcrWord> = row.segments.iter().flat_map(|&s| segments[s].words.iter()).collect();
    let mut out = String::new();
    let mut end = 0usize;
    for (n, word) in words.iter().enumerate() {
        // A parenthesis or a brace sits in the middle of its cell, so its ink, and the engine's
        // box, starts more than half a cell late. A square bracket's ink starts at its cell's
        // edge (measured on the code sample), so it needs no correction.
        let lead = if word.text.starts_with(['(', '{']) { 0.6 } else { 0.0 };
        let position = ((word.bounds.x - mono.left) / mono.advance - lead).max(0.0);
        let at = position.round() as usize;
        if n == 0 {
            out.push_str(&" ".repeat(at.min(200)));
        } else {
            // Each word sits in the column its position gives. When the engine put a space
            // between two words that touch on the grid ("overlaps (block"), the space goes, but
            // only if the word before fills the cells its text says, so after a misread word
            // ("" for three quotes) the engine's space stays.
            let previous = words[n - 1];
            let cells = previous.bounds.width / mono.advance;
            let exact = (cells - previous.text.chars().count() as f64).abs() < 0.75;
            let gap = position - end as f64;
            // Two words with a letter or digit on both sides of the seam are two tokens: the
            // engine splits a run of letters only at a space. (It does split at `_`.)
            let seam = previous.text.ends_with(char::is_alphanumeric) && word.text.starts_with(char::is_alphanumeric);
            let spaces = if gap >= 0.5 {
                gap.round() as usize
            } else if exact && !seam {
                0
            } else {
                1
            };
            out.push_str(&" ".repeat(spaces.min(200)));
        }
        out.push_str(&word.text);
        end = out.chars().count();
    }
    out
}

/// Joins the lines of one prose paragraph with spaces, rejoining hyphenated words where safe.
fn join_prose(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if out.is_empty() {
            out.push_str(line);
            continue;
        }
        match join_hyphenated(&out, line) {
            Some(joined) => out = joined,
            None => {
                let cjk = out.chars().last().is_some_and(is_cjk) && line.chars().next().is_some_and(is_cjk);
                if !cjk {
                    out.push(' ');
                }
                out.push_str(line);
            }
        }
    }
    out
}

/// Joins `next` to `text` when `text` ends in a hyphen. Returns `None` when there is no hyphen.
///
/// The hyphen is removed ("recog-" + "nition" → "recognition") only when the part before it is
/// at least three letters, the next line starts with a lowercase letter, and the part before it
/// is not a common prefix of real compounds ("self-", "well-", "non-"…). Otherwise the hyphen
/// stays and only the line break goes ("well-" + "known" → "well-known"), which never loses a
/// character the image showed.
pub fn join_hyphenated(text: &str, next: &str) -> Option<String> {
    let stem = text.strip_suffix('-').or_else(|| text.strip_suffix('\u{2010}'))?;
    let before: String =
        stem.chars().rev().take_while(|c| c.is_alphabetic()).collect::<Vec<_>>().into_iter().rev().collect();
    let next_starts_lower = next.chars().next().is_some_and(|c| c.is_lowercase());
    if before.is_empty() || !next_starts_lower {
        // "2019-" or "-" before a capital: a dash or a real hyphen; the line break goes only.
        return Some(format!("{text}{next}"));
    }
    const COMPOUND: &[&str] = &[
        "self", "well", "non", "co", "e", "ex", "re", "pre", "post", "anti", "multi", "cross", "high", "low", "long",
        "short", "full", "half", "all", "so", "over", "under", "third", "first", "second", "real", "end", "open",
        "built", "read", "write", "user", "client", "server", "sign", "log", "opt", "pop", "drop", "up", "down",
    ];
    let lower = before.to_lowercase();
    if before.chars().count() < 3 || COMPOUND.contains(&lower.as_str()) {
        return Some(format!("{text}{next}"));
    }
    Some(format!("{stem}{next}"))
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xAC00..=0xD7AF | 0x3000..=0x303F | 0xFF00..=0xFFEF)
}
