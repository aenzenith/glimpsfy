//! Text recognition with a CTC model: every detected box is cut out of the image and straightened,
//! the cuts are read side by side on the model's lanes, and the model's per-column answers are
//! decoded into text with word boxes.
//!
//! # Speed
//!
//! Every line is read in a run of its own. On the CPU, batches of lines read slower than the same
//! lines one by one (a dense 1440p screen on ONNX Runtime, which the engine first ran on: 9.7 s
//! in batches of eight, 5.5 s line by line, on one thread), because a single line's tensors stay
//! in the processor's caches, and nothing is spent on padding short lines to the batch's widest.
//! The lines are spread over one lane of the model per thread, which scales with the cores where
//! one run's own threads do not.
//!
//! # Word boxes
//!
//! The model reads a whole line and reports no boxes. It does answer once per eight pixel columns
//! of its input, so every decoded character has a position along the line (this is also how
//! PaddleOCR's `return_word_box` works). A word reaches from its first character's position to
//! its last; where the line's background is plain, the ends are then moved to the edges of the
//! empty gap between the words, which is exact to a pixel. The layout step needs that precision
//! to rebuild the indentation of code and to tell table cells apart, and the text layer needs it
//! to put the selection where the words are.

use std::sync::Mutex;

use super::detect::TextBox;
use super::pixels::Pixels;
use super::runtime::Model;
use crate::{Bounds, CancelToken, OcrError, OcrWord, RawLine, Result};

/// The height the recognition models read at.
const HEIGHT: usize = 48;
/// A cut is at least this wide, as in PaddleOCR, whose models saw short text padded to it.
const MIN_WIDTH: usize = 320;
/// A line that would be wider than this is squeezed to it.
const MAX_WIDTH: usize = 3200;
/// The model answers once per this many pixel columns; it stands in for a character's width
/// where a line gives none.
const COLUMN: usize = 8;
/// A reading the model is less sure of than this is dropped: it is an icon or a picture.
const MIN_CONFIDENCE: f32 = 0.5;
/// A reading without two letters or digits in a row is kept only when the model is at least this
/// sure of it. Junk from a QR code read at 0.78 in the sample set; real lines, even of 6 px text,
/// read above 0.82 and hold words.
const MIN_CONFIDENCE_WITHOUT_WORD: f32 = 0.9;

/// How far outside a box its cut-off glyphs are looked for, as a share of the box's height.
const FRAME_REACH: f32 = 0.25;
/// The empty band that still joins ink outside a box to the line, as a share of the box's height
/// (the gap under a dot or an accent).
const FRAME_SKIP: f32 = 0.08;
/// The margin a cut keeps above and below the glyphs, as a share of the glyphs' height.
const FRAME_MARGIN: f32 = 0.12;
/// The margin a line's box keeps above and below its glyphs, as a share of their height. Boxes
/// of the bare ink would put the words of a code line further apart than the line is high (the
/// gaps between words are exact to the ink), which the layout step takes for a column gap.
const ROW_MARGIN: f32 = 0.08;

/// One character of a decoded line with its position along the cut, in pixels of the cut.
struct Glyph {
    character: char,
    at: f32,
}

/// One reading of a box: the box as it was read (the direction matters for upright boxes that
/// hold vertical text) and what came out.
struct Reading {
    source: usize,
    /// The detector's box in the direction it is read, which the line's and the words' boxes
    /// are made from.
    area: TextBox,
    /// What is cut out for the model: the same stretch of the line, framed around its glyphs.
    view: TextBox,
    /// The rows the glyphs take, from the top of the first to the bottom of the last, when
    /// [`framed`] found them; they are the line's height in its box.
    rows: Option<(f32, f32)>,
    /// Image pixels per pixel of the cut, along the reading axis.
    per_pixel: f32,
    width: usize,
    ink: Option<Vec<bool>>,
    glyphs: Vec<Glyph>,
    confidence: f32,
}

/// Reads the text in every box. `alphabet` is the model's dictionary; the model's answer has one
/// more class before it (the CTC blank) and one after it (the space).
///
/// Every lane of the model reads lines at once, the widest first, so that the narrow ones fill
/// the gaps at the end and the lanes finish at about the same time.
pub fn read(
    model: &Model,
    alphabet: &[char],
    pixels: &Pixels<'_>,
    boxes: &[TextBox],
    cancel: &CancelToken,
) -> Result<Vec<RawLine>> {
    let mut readings = candidates(pixels, boxes);
    readings.sort_by_key(|r| std::cmp::Reverse(r.width));
    let workers = model.lanes().min(readings.len());
    let queue = Mutex::new(readings.iter_mut());
    let work = |lane: usize| -> Result<()> {
        loop {
            let next = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).next();
            let Some(reading) = next else { return Ok(()) };
            let done = cancel.check().and_then(|()| read_one(model, lane, alphabet, pixels, reading));
            if done.is_err() {
                // The other lanes stop after the line they are reading.
                queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).by_ref().for_each(drop);
                return done;
            }
        }
    };
    std::thread::scope(|scope| {
        let others: Vec<_> = (1..workers).map(|lane| scope.spawn(move || work(lane))).collect();
        // The calling thread reads on the first lane instead of waiting.
        let mine = work(0);
        others.into_iter().fold(mine, |result, other| {
            let theirs = other.join().unwrap_or_else(|_| Err(OcrError::Engine("a recognition thread failed".into())));
            result.and(theirs)
        })
    })?;
    cancel.check()?;

    // A box that was read in two directions keeps the reading the model is surer of.
    let mut best: Vec<Option<Reading>> = (0..boxes.len()).map(|_| None).collect();
    for reading in readings {
        let slot = &mut best[reading.source];
        if slot.as_ref().is_none_or(|kept| reading.confidence > kept.confidence) {
            *slot = Some(reading);
        }
    }
    let image = (pixels.width, pixels.height);
    Ok(best.into_iter().flatten().filter_map(|reading| line(&reading, image)).collect())
}

/// Cuts one line out of the image, runs it through the model on lane `lane` and decodes the
/// answer into the reading.
fn read_one(model: &Model, lane: usize, alphabet: &[char], pixels: &Pixels<'_>, reading: &mut Reading) -> Result<()> {
    let width = reading.width.max(MIN_WIDTH);
    let mut data = vec![0f32; 3 * HEIGHT * width];
    cut(pixels, &reading.view, reading.width, width, &mut data);
    reading.ink = reading.area.upright().then(|| ink(&data, reading.width, width)).flatten();
    model.run(lane, [1, 3, HEIGHT, width], data, |shape, answer| decode(shape, answer, alphabet, width, reading))
}

/// The readings to make: one per box, and two for a box that holds vertical text, whose direction
/// only the reading itself can tell.
fn candidates(pixels: &Pixels<'_>, boxes: &[TextBox]) -> Vec<Reading> {
    let mut short: Vec<f32> = boxes.iter().map(|b| b.width.min(b.height)).collect();
    short.sort_by(f32::total_cmp);
    let usual = short.get(short.len() / 2).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(boxes.len());
    for (source, area) in boxes.iter().enumerate() {
        // A tall upright box is vertical text only when it is much taller than a line is high;
        // otherwise it is a single narrow character such as "1" or "l".
        let vertical = area.upright() && area.height >= 1.5 * area.width && area.height >= 2.0 * usual;
        let steep = !area.upright() && area.sin.abs() > area.cos.abs();
        let areas: [Option<TextBox>; 2] = if vertical {
            let turned = |sin: f32| TextBox { width: area.height, height: area.width, cos: 0.0, sin, ..*area };
            [Some(turned(1.0)), Some(turned(-1.0))]
        } else if steep {
            [Some(*area), Some(TextBox { cos: -area.cos, sin: -area.sin, ..*area })]
        } else {
            [Some(*area), None]
        };
        for area in areas.into_iter().flatten() {
            if area.width < 1.0 || area.height < 1.0 {
                continue;
            }
            let (view, rows) = framed(pixels, &area);
            let ideal = (view.width * HEIGHT as f32 / view.height).ceil() as usize;
            let width = ideal.clamp(HEIGHT / 4, MAX_WIDTH);
            out.push(Reading {
                source,
                area,
                view,
                rows,
                per_pixel: area.width / width as f32,
                width,
                ink: None,
                glyphs: Vec::new(),
                confidence: 0.0,
            });
        }
    }
    out
}

/// The box to cut for the model: the detector's box with its top and bottom moved to a fixed
/// margin around the line's glyphs, and the rows the glyphs take.
///
/// The detector's boxes are not equally snug: around small text they touch the glyphs, around
/// large text they leave a quarter of their height free. The model reads `ı` as `i` (and loses
/// other marks above letters) when the glyphs touch the top of its input, so every line is given
/// the same frame. The glyphs' rows also become the line's height, because the layout step tells
/// rows and cells apart by how lines overlap, which wants boxes as tight as Vision's. A box whose
/// background is not plain, or that is turned, is cut as it is and has no rows.
fn framed(pixels: &Pixels<'_>, area: &TextBox) -> (TextBox, Option<(f32, f32)>) {
    let unchanged = (*area, None);
    if !area.upright() {
        return unchanged;
    }
    let (left, right) = (area.cx - area.width / 2.0, area.cx + area.width / 2.0);
    let (top, bottom) = (area.cy - area.height / 2.0, area.cy + area.height / 2.0);
    let (x0, x1) = ((left.max(0.0) as usize).min(pixels.width), (right.ceil().max(0.0) as usize).min(pixels.width));
    // Glyphs that the box cuts off are looked for a little outside it.
    let reach = (area.height * FRAME_REACH).ceil();
    let (y0, y1) = (
        ((top - reach).max(0.0) as usize).min(pixels.height),
        ((bottom + reach).ceil().max(0.0) as usize).min(pixels.height),
    );
    let (inner0, inner1) = ((top.max(0.0) as usize).clamp(y0, y1), (bottom.ceil().max(0.0) as usize).clamp(y0, y1));
    if x1 <= x0 + 1 || inner1 <= inner0 + 1 {
        return unchanged;
    }

    // The background is the usual colour of the box: text covers the smaller part of a line.
    let step = ((x1 - x0) * (inner1 - inner0) / 4000).max(1);
    let mut background = [0f32; 3];
    for (channel, value) in background.iter_mut().enumerate() {
        let mut seen: Vec<f32> =
            (inner0..inner1).flat_map(|y| (x0..x1).step_by(step).map(move |x| pixels.bgr(x, y)[channel])).collect();
        seen.sort_by(f32::total_cmp);
        *value = seen[seen.len() / 2];
    }
    let rows: Vec<f32> = (y0..y1)
        .map(|y| {
            (x0..x1)
                .map(|x| {
                    let p = pixels.bgr(x, y);
                    (0..3).map(|c| (p[c] - background[c]).abs()).fold(0.0, f32::max)
                })
                .fold(0.0, f32::max)
        })
        .collect();
    let strongest = rows[inner0 - y0..inner1 - y0].iter().copied().fold(0.0, f32::max);
    // On the 0…255 scale: with less contrast than this there is no ink to frame.
    if strongest < 16.0 {
        return unchanged;
    }
    let inked: Vec<bool> = rows.iter().map(|v| *v > strongest * 0.3).collect();
    let (Some(first), Some(last)) =
        ((inner0..inner1).find(|y| inked[y - y0]), (inner0..inner1).rev().find(|y| inked[y - y0]))
    else {
        return unchanged;
    };
    // Outside the box, ink that continues the glyphs belongs to the line (a descender, a dot or
    // an accent the box cut off); a wider empty band ends the line.
    let skip = (area.height * FRAME_SKIP) as usize + 1;
    let (mut first, mut last) = (first, last);
    if first == inner0 {
        let mut empty = 0;
        for y in (y0..inner0).rev() {
            if inked[y - y0] {
                (first, empty) = (y, 0);
            } else {
                empty += 1;
                if empty > skip {
                    break;
                }
            }
        }
    }
    if last + 1 == inner1 {
        let mut empty = 0;
        for y in inner1..y1 {
            if inked[y - y0] {
                (last, empty) = (y, 0);
            } else {
                empty += 1;
                if empty > skip {
                    break;
                }
            }
        }
    }
    let glyphs = (last + 1 - first) as f32;
    // Ink that fills the whole search band is not a line of text on a plain background.
    if first == y0 && last + 1 == y1 && y0 > 0 && y1 < pixels.height {
        return unchanged;
    }
    let margin = glyphs * FRAME_MARGIN;
    let (top, bottom) = (first as f32 - margin, (last + 1) as f32 + margin);
    (TextBox { cy: (top + bottom) / 2.0, height: bottom - top, ..*area }, Some((first as f32, (last + 1) as f32)))
}

/// Cuts a box out of the image into `out`: three planes (blue, green, red) of `HEIGHT` rows and
/// `stride` columns, of which the first `width` hold the box, scaled to -1…1. The rest stays
/// zero, which is the padding PaddleOCR uses.
fn cut(pixels: &Pixels<'_>, area: &TextBox, width: usize, stride: usize, out: &mut [f32]) {
    let plane = HEIGHT * stride;
    let (along, across) = (area.width / width as f32, area.height / HEIGHT as f32);
    // A line that is reduced averages several samples per pixel, so thin strokes survive.
    let taps = |step: f32| -> Vec<f32> {
        let n = (step - 0.25).ceil().clamp(1.0, 4.0) as usize;
        (0..n).map(|i| (i as f32 + 0.5) / n as f32).collect()
    };
    let (taps_x, taps_y) = (taps(along), taps(across));
    let share = 1.0 / (taps_x.len() * taps_y.len()) as f32;
    for row in 0..HEIGHT {
        for column in 0..width {
            let mut sum = [0f32; 3];
            for ty in &taps_y {
                for tx in &taps_x {
                    let (x, y) = area.point((column as f32 + tx) * along, (row as f32 + ty) * across);
                    let p = pixels.sample(x, y);
                    sum = [sum[0] + p[0], sum[1] + p[1], sum[2] + p[2]];
                }
            }
            let at = row * stride + column;
            for (channel, value) in sum.iter().enumerate() {
                out[channel * plane + at] = value * share / 127.5 - 1.0;
            }
        }
    }
}

/// Which columns of a cut hold ink, or `None` when the cut has no plain background to tell ink
/// from (a picture, a gradient, an underline through the whole line).
fn ink(cut: &[f32], width: usize, stride: usize) -> Option<Vec<bool>> {
    let plane = HEIGHT * stride;
    // The detector's box has a margin above and below the glyphs, so the outermost rows show the
    // background; their median ignores the few ascenders and descenders that reach them.
    let mut background = [0f32; 3];
    for (channel, value) in background.iter_mut().enumerate() {
        let mut edge: Vec<f32> = Vec::with_capacity(width * 4);
        for row in [0, 1, HEIGHT - 2, HEIGHT - 1] {
            let at = channel * plane + row * stride;
            edge.extend_from_slice(&cut[at..at + width]);
        }
        edge.sort_by(f32::total_cmp);
        *value = edge[edge.len() / 2];
    }
    let differs = |row: usize, column: usize| -> f32 {
        let at = row * stride + column;
        (0..3).map(|c| (cut[c * plane + at] - background[c]).abs()).fold(0.0, f32::max)
    };
    let mut most = vec![0f32; width];
    for row in 0..HEIGHT {
        for (column, value) in most.iter_mut().enumerate() {
            *value = value.max(differs(row, column));
        }
    }
    let strongest = most.iter().copied().fold(0.0, f32::max);
    // On the cut's -1…1 scale: text with less contrast than this cannot be told from noise.
    if strongest < 0.12 {
        return None;
    }
    let threshold = (strongest * 0.3).max(0.08);
    let columns: Vec<bool> = most.iter().map(|v| *v > threshold).collect();
    let inked = columns.iter().filter(|c| **c).count();
    (inked * 10 < width * 9).then_some(columns)
}

/// Decodes the answer for one line (`[1, columns, classes]`) into the reading's characters: the
/// likeliest class of every column, repeats merged and blanks dropped (greedy CTC).
fn decode(shape: &[usize], answer: &[f32], alphabet: &[char], width: usize, reading: &mut Reading) -> Result<()> {
    let classes = alphabet.len() + 2;
    let columns = match shape {
        [1, columns, c] if *c == classes && *columns > 0 => *columns,
        [_, _, c] if *c != classes => {
            return Err(OcrError::Engine(format!(
                "the recognition model answers with {c} classes, and its dictionary names {classes}"
            )))
        }
        _ => return Err(OcrError::Engine("the recognition model's answer has an unexpected shape".into())),
    };
    let step = width as f32 / columns as f32;
    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut sure = 0.0f32;
    // The class of the run of equal columns that is open, with its first column and the
    // highest probability inside it.
    let mut open: Option<(usize, usize, f32)> = None;
    let mut close = |open: Option<(usize, usize, f32)>, until: usize| {
        let Some((class, from, probability)) = open else { return };
        let at = (from + until) as f32 / 2.0 * step;
        // Whatever the model says about the padding beside the line is not text.
        if class == 0 || at > reading.width as f32 + step {
            return;
        }
        let character = if class == classes - 1 { ' ' } else { alphabet[class - 1] };
        glyphs.push(Glyph { character, at });
        sure += probability;
    };
    for column in 0..columns {
        let scores = &answer[column * classes..][..classes];
        let (class, probability) =
            scores.iter().copied().enumerate().fold((0, f32::MIN), |best, s| if s.1 > best.1 { s } else { best });
        match open {
            Some((current, from, most)) if current == class => open = Some((current, from, most.max(probability))),
            _ => {
                close(open, column);
                open = Some((class, column, probability));
            }
        }
    }
    close(open, columns);
    reading.confidence = if glyphs.is_empty() { 0.0 } else { sure / glyphs.len() as f32 };
    reading.glyphs = glyphs;
    Ok(())
}

/// A word of a line: its text and the span it takes along the cut, in pixels of the cut.
struct Word {
    text: String,
    from: f32,
    to: f32,
}

/// The line a reading makes, or `None` when it read nothing or is not sure enough.
fn line(reading: &Reading, image: (usize, usize)) -> Option<RawLine> {
    if reading.confidence < MIN_CONFIDENCE {
        return None;
    }
    // What the model reads in a QR code or an icon is a few symbols without a word between them.
    let word = reading.glyphs.windows(2).any(|pair| pair.iter().all(|g| g.character.is_alphanumeric()));
    if reading.confidence < MIN_CONFIDENCE_WITHOUT_WORD && !word {
        return None;
    }
    let words = words(reading);
    let (first, last) = (words.first()?, words.last()?);
    let place = |from: f32, to: f32| -> Bounds {
        let limit = reading.width as f32;
        let mut bounds = reading.area.enclosing(
            from.clamp(0.0, limit) * reading.per_pixel,
            to.clamp(0.0, limit) * reading.per_pixel,
            image,
        );
        if let Some((top, bottom)) = reading.rows {
            let margin = (bottom - top) * ROW_MARGIN;
            (bounds.y, bounds.height) = ((top - margin) as f64, (bottom - top + 2.0 * margin) as f64);
        }
        bounds
    };
    let confidence = reading.confidence.clamp(0.0, 1.0) as f64;
    Some(RawLine {
        text: words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
        bounds: place(first.from, last.to),
        confidence,
        words: words
            .iter()
            .map(|w| OcrWord { text: w.text.clone(), bounds: place(w.from, w.to), confidence })
            .collect(),
    })
}

/// Splits a reading's characters into words and gives every word its span along the cut.
fn words(reading: &Reading) -> Vec<Word> {
    // Each word as its text and the positions of its first and last character.
    let mut spans: Vec<(String, f32, f32)> = Vec::new();
    let mut open = false;
    for glyph in &reading.glyphs {
        // The dictionaries hold a second, full-width space; both end a word.
        if glyph.character.is_whitespace() {
            open = false;
        } else if open {
            if let Some(span) = spans.last_mut() {
                span.0.push(glyph.character);
                span.2 = glyph.at;
            }
        } else {
            spans.push((glyph.character.to_string(), glyph.at, glyph.at));
            open = true;
        }
    }
    if spans.is_empty() {
        return Vec::new();
    }

    // The usual distance between two characters of a word, which stands in for a character's
    // width wherever no gap in the ink gives a word's edge.
    let mut steps: Vec<f32> = Vec::new();
    let mut previous: Option<&Glyph> = None;
    for glyph in &reading.glyphs {
        if let Some(before) = previous.filter(|b| !b.character.is_whitespace() && !glyph.character.is_whitespace()) {
            steps.push(glyph.at - before.at);
        }
        previous = Some(glyph);
    }
    steps.sort_by(f32::total_cmp);
    let advance = steps.get(steps.len() / 2).copied().unwrap_or(HEIGHT as f32 * 0.45).max(COLUMN as f32);
    let half = advance / 2.0;

    let limit = reading.width as f32;
    let mut out: Vec<Word> = spans
        .iter()
        .map(|(text, first, last)| Word {
            text: text.clone(),
            from: (first - half).max(0.0),
            to: (last + half).min(limit),
        })
        .collect();
    // Without a gap to go by, two neighbours share the space between their characters.
    for i in 1..out.len() {
        if out[i - 1].to > out[i].from {
            let middle = (spans[i - 1].2 + spans[i].1) / 2.0;
            (out[i - 1].to, out[i].from) = (middle, middle);
        }
    }
    let Some(ink) = reading.ink.as_deref() else {
        return out;
    };

    // The empty stretches of the line, as (first empty column, first inked column after it).
    let mut gaps: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (column, inked) in ink.iter().enumerate() {
        match (inked, start) {
            (false, None) => start = Some(column),
            (true, Some(from)) => {
                gaps.push((from, column));
                start = None;
            }
            _ => {}
        }
    }
    // Between two words, the widest empty stretch between the last character of one and the
    // first character of the other is the space, and its edges are the words' edges. The
    // stretch must lie between the two characters: one that merely is near would move a
    // one-letter word onto its neighbour.
    for i in 1..out.len() {
        let (after, before) = (spans[i - 1].2, spans[i].1);
        let widest = gaps
            .iter()
            .filter(|(from, to)| (*to as f32) > after && (*from as f32) < before)
            .max_by_key(|(from, to)| to - from);
        if let Some((from, to)) = widest.filter(|(from, to)| to - from >= 2) {
            (out[i - 1].to, out[i].from) = (*from as f32, *to as f32);
        }
    }
    // The line's own ends are the first and the last inked column, when they lie where the
    // first and the last character were read and not at a stray mark beside the line.
    let reach = advance * 2.0;
    if let Some(first) = ink.iter().position(|c| *c).map(|c| c as f32) {
        if first <= spans[0].1 && spans[0].1 - first <= reach {
            out[0].from = first;
        }
    }
    if let (Some(last), Some(word)) = (ink.iter().rposition(|c| *c).map(|c| c as f32 + 1.0), out.last_mut()) {
        let read = spans[spans.len() - 1].2;
        if last >= read && last - read <= reach {
            word.to = last;
        }
    }
    // A stray edge can never turn a word inside out.
    for word in &mut out {
        if word.to <= word.from {
            word.to = (word.from + 1.0).min(limit.max(word.from + 1.0));
        }
    }
    out
}
