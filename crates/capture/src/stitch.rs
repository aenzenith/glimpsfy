//! The scrolling capture's stitcher: the frames of one area, taken while the page under it moves,
//! become one tall image (D27).
//!
//! - Every physical row of a frame is signed with a 64-bit hash. A row of one colour says nothing
//!   about where it is, so flat rows never vote and never count towards a match.
//! - A row is signed over the columns that move with the page alone. An area often holds more
//!   than the page: a side bar that stays in place, or a scroll bar whose knob runs its own way,
//!   and either keeps every row from matching the place the page moved it to. The first movement
//!   is read in strips of a few columns, the strips that did not move with it leave the signature,
//!   and the result keeps the columns between the outermost strips that did.
//! - A frame's band is what changed since the frame before. Rows that stay in place at the top and
//!   the bottom while the rest moves are a sticky header and a bottom bar: they leave the band,
//!   enter the result once, and count as found once a second move shows them again. A spinner or
//!   a clock inside such a bar does not end it.
//! - The band is placed on the result by a vote: each distinctive row looks its hash up in the
//!   result, and the offset most rows agree on wins. A placement holds when the band overlaps the
//!   result by max(24 px, a fifth of the band) and 90 % of its distinctive rows there match, so a
//!   clock or a caret that changed in between does not break it. A row the result holds in several
//!   places splits its vote, and a row that contradicts a placement outweighs four that agree, so
//!   a page of near-identical items does not slip back by one item.
//! - The result grows in both directions. A band that fits nowhere opens a new piece next to the
//!   current one, kept only if the next band fits it (a tooltip or a menu passes without a trace),
//!   and a band that fits two neighbouring pieces joins them, which closes the gap.
//! - `undo` takes back the rows of the last movement. Rows are kept compressed in strips of 512
//!   (PNG at the fastest setting), with a narrow thumbnail of each row for the live preview.
//!
//! Nothing here touches the system: the app feeds frames in from the stream and reads the
//! progress, the preview and the finished frame.

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::ops::Range;

use crate::error::{CaptureError, Result};
use crate::frame::{swizzle_bgra_rgba, ColorInfo, Frame, PixelFormat, PngEffort};
use crate::geometry::{PhysicalSize, ScaleFactor};

/// The tallest result in physical pixels. Rows past it are not taken, and the app finishes the
/// capture when the result reaches it.
pub const MAX_HEIGHT: u32 = 32_000;
/// Rows per compressed strip.
const STRIP_ROWS: usize = 512;
/// The smallest overlap of a band with the result; a fifth of the band when that is more.
const MIN_OVERLAP: usize = 24;
/// The share of the distinctive rows in the overlap that must match.
const MATCH_SHARE: f64 = 0.9;
/// An overlap with fewer distinctive rows than this proves nothing.
const MIN_DISTINCT: usize = 4;
/// A row whose hash the result holds more often than this cannot tell where it belongs.
const COMMON: usize = 12;
/// How many movements `undo` can take back.
const UNDO_DEPTH: usize = 32;
/// How many of the best-voted placements are checked.
const CANDIDATES: usize = 6;
/// The vote of a row the result holds once.
const VOTE: u32 = 5040;
/// The rows inside a sticky edge that may change from frame to frame (a spinner, a clock) while
/// the edge still counts as one: rows that held still lie beyond them.
const EDGE_CHANGE: usize = 48;
/// A row that contradicts a placement outweighs this many rows that agree with it. A page that
/// repeats itself (cards, a skeleton) agrees with a placement one item off almost everywhere, but
/// the few rows that tell the items apart contradict it.
const MISMATCH_WEIGHT: usize = 4;
/// The width in pixels of the strips the first movement is read in.
const COLUMN_STRIP: usize = 4;
/// A strip moves with the page when this share of its distinctive rows agree with the move. A
/// scroll bar's knob changes a few rows in a hundred, so the share sits above that.
const MOVING_SHARE: f64 = 0.97;
/// The strip rows that must agree on a shift before it counts as the page's move.
const MIN_SUPPORT: f64 = 48.0;

/// A place on the result: a piece's id and a position in its rows.
type Spot = (u32, i64);

/// One row's signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sig {
    hash: u64,
    /// Every pixel of the row is the same.
    flat: bool,
}

/// The signature of one row of 4-byte pixels, read over the pixel ranges `columns` alone.
fn sign(row: &[u8], columns: &[Range<usize>]) -> Sig {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let len: usize = columns.iter().map(|c| c.len() * 4).sum();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ len as u64;
    let mut first: Option<&[u8]> = None;
    let mut flat = true;
    for range in columns {
        let part = &row[range.start * 4..range.end * 4];
        let mut chunks = part.chunks_exact(8);
        for chunk in &mut chunks {
            let value =
                u64::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7]]);
            hash = (hash.rotate_left(5) ^ value).wrapping_mul(K);
        }
        for &byte in chunks.remainder() {
            hash = (hash.rotate_left(5) ^ byte as u64).wrapping_mul(K);
        }
        if flat {
            let first = *first.get_or_insert_with(|| part.get(..4).unwrap_or(&[]));
            flat = part.chunks_exact(4).all(|pixel| pixel == first);
        }
    }
    // The finaliser of MurmurHash3, so neighbouring rows spread over the whole range.
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    Sig { hash, flat }
}

/// The columns of the area that move with the page.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Columns {
    /// The pixel ranges a row is signed over.
    signed: Vec<Range<usize>>,
    /// The columns the result keeps.
    kept: Range<usize>,
}

impl Columns {
    fn all(width: usize) -> Columns {
        Columns { signed: std::iter::once(0..width).collect(), kept: 0..width }
    }
}

/// What a strip of columns did during a move.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StripKind {
    /// It moved with the page.
    Moving,
    /// It showed too little to tell (a margin of one colour).
    Blank,
    /// It stayed in place or ran its own way: a side bar, a scroll bar's knob, a video.
    Apart,
}

/// The columns that moved with the page from `before` to `now`, read in strips of `COLUMN_STRIP`
/// pixels. The page's move is the shift the most strip rows agree on (rows that stayed in place
/// across the whole width, a sticky edge, say nothing about it), and a strip moved with it when
/// `MOVING_SHARE` of its distinctive rows agree. `None` when the frames show no such move.
fn moving_columns(before: &Frame, now: &Frame) -> Option<Columns> {
    let (w, h) = (now.size.width as usize, now.size.height as usize);
    let n = w.div_ceil(COLUMN_STRIP);
    let strip = |s: usize| s * COLUMN_STRIP..((s + 1) * COLUMN_STRIP).min(w);
    let signs = |frame: &Frame| -> Vec<Sig> {
        let mut out = Vec::with_capacity(n * h);
        for y in 0..h {
            let row = row_of(frame, y);
            out.extend((0..n).map(|s| sign(row, &[strip(s)])));
        }
        out
    };
    let (was, is) = (signs(before), signs(now));
    let at = |sigs: &[Sig], y: usize, s: usize| sigs[y * n + s];
    let changed: Vec<bool> = (0..h).map(|y| (0..n).any(|s| at(&was, y, s) != at(&is, y, s))).collect();

    // Row y of `now` shows what `before` showed at y + shift.
    let mut votes: HashMap<i64, f64> = HashMap::new();
    let mut rows: HashMap<u64, Vec<usize>> = HashMap::new();
    for s in 0..n {
        rows.clear();
        for y in (0..h).filter(|&y| changed[y]) {
            let sig = at(&was, y, s);
            if !sig.flat {
                rows.entry(sig.hash).or_default().push(y);
            }
        }
        for y in (0..h).filter(|&y| changed[y]) {
            let sig = at(&is, y, s);
            if sig.flat {
                continue;
            }
            // A few columns repeat often (the stem of a letter); such a row cannot tell where it was.
            let Some(found) = rows.get(&sig.hash).filter(|f| f.len() <= MIN_DISTINCT) else { continue };
            for &p in found {
                let shift = p as i64 - y as i64;
                if shift != 0 {
                    *votes.entry(shift).or_default() += 1.0 / found.len() as f64;
                }
            }
        }
    }
    let (shift, support) =
        votes.into_iter().max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.abs().cmp(&a.0.abs())).then(b.0.cmp(&a.0)))?;
    if support < MIN_SUPPORT {
        return None;
    }
    // The rows the page carried: a row no strip of which moved with the shift (a spinner, a menu
    // that opened) tells nothing about the columns.
    let overlap: Vec<(usize, usize)> = (0..h)
        .filter_map(|y| {
            let p = usize::try_from(y as i64 + shift).ok().filter(|&p| p < h)?;
            let carried = (0..n).any(|s| {
                let now = at(&is, y, s);
                !now.flat && at(&was, p, s) == now
            });
            (changed[y] && changed[p] && carried).then_some((y, p))
        })
        .collect();
    if overlap.len() < MIN_OVERLAP {
        return None;
    }
    let kinds: Vec<StripKind> = (0..n)
        .map(|s| {
            let (mut distinct, mut agree, mut moved) = (0usize, 0usize, 0usize);
            for &(y, p) in &overlap {
                let (then, now) = (at(&was, p, s), at(&is, y, s));
                if then == now && now.flat {
                    continue;
                }
                distinct += 1;
                agree += usize::from(then == now);
                moved += usize::from(at(&was, y, s) != now);
            }
            // A strip whose rows are all alike (a window's border) agrees with any shift without
            // having moved; it tells nothing either way.
            if distinct < MIN_DISTINCT || (moved < MIN_DISTINCT && agree as f64 >= MOVING_SHARE * distinct as f64) {
                StripKind::Blank
            } else if agree as f64 >= MOVING_SHARE * distinct as f64 {
                StripKind::Moving
            } else {
                StripKind::Apart
            }
        })
        .collect();
    if kinds.iter().filter(|k| **k == StripKind::Moving).count() < MIN_DISTINCT {
        return None;
    }
    let first = kinds.iter().position(|k| *k == StripKind::Moving)?;
    let last = kinds.iter().rposition(|k| *k == StripKind::Moving)?;
    // A margin of the page's own colour belongs to it. One of another colour (the empty part of a
    // side bar) or one past a strip that stood apart belongs to the window around it.
    let ground = |s: usize| background(now, strip(s));
    let joins = |s: usize, page: Option<[u8; 4]>| {
        kinds[s] == StripKind::Blank && (page.is_none() || ground(s).is_none_or(|c| Some(c) == page))
    };
    let mut start = first;
    let page = ground(first);
    while start > 0 && joins(start - 1, page) {
        start -= 1;
    }
    let mut end = last + 1;
    let page = ground(last);
    while end < n && joins(end, page) {
        end += 1;
    }
    // A strip that joined may still start with a few columns of the side bar's colour.
    let mut from = strip(start).start;
    let page = ground(first);
    while from < strip(first).start && column_colour(now, from).is_some_and(|c| Some(c) != page) {
        from += 1;
    }
    let mut to = strip(end - 1).end;
    let page = ground(last);
    while to > strip(last).end && column_colour(now, to - 1).is_some_and(|c| Some(c) != page) {
        to -= 1;
    }
    // A strip inside the page that ran its own way (a video, a spinner) stays in the result but
    // signs nothing.
    let mut signed: Vec<Range<usize>> = Vec::new();
    for s in (start..end).filter(|&s| kinds[s] != StripKind::Apart) {
        let range = strip(s).start.max(from)..strip(s).end.min(to);
        if range.is_empty() {
            continue;
        }
        match signed.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => signed.push(range),
        }
    }
    Some(Columns { signed, kept: from..to })
}

/// The colour most rows of `columns` in `frame` are filled with, when half of them are of one
/// colour; `None` for columns that hold a picture on most rows.
fn background(frame: &Frame, columns: Range<usize>) -> Option<[u8; 4]> {
    let h = frame.size.height as usize;
    let mut counts: HashMap<[u8; 4], usize> = HashMap::new();
    for y in 0..h {
        let part = &row_of(frame, y)[columns.start * 4..columns.end * 4];
        let first = [part[0], part[1], part[2], part[3]];
        if part.chunks_exact(4).all(|pixel| pixel == first) {
            *counts.entry(first).or_default() += 1;
        }
    }
    let (colour, count) = counts.into_iter().max_by_key(|&(colour, count)| (count, colour))?;
    (count * 2 >= h).then_some(colour)
}

/// The colour of column `x` of `frame` when nine rows in ten have it.
fn column_colour(frame: &Frame, x: usize) -> Option<[u8; 4]> {
    let h = frame.size.height as usize;
    let mut counts: HashMap<[u8; 4], usize> = HashMap::new();
    for y in 0..h {
        let p = &row_of(frame, y)[x * 4..x * 4 + 4];
        *counts.entry([p[0], p[1], p[2], p[3]]).or_default() += 1;
    }
    let (colour, count) = counts.into_iter().max_by_key(|&(colour, count)| (count, colour))?;
    (count * 10 >= h * 9).then_some(colour)
}

/// The rows at the top and the bottom that stayed in place from `before` to `now`. Each counts only
/// when `MIN_DISTINCT` of its rows held still and occur once in the frame, and it ends at the
/// innermost such row: flat or repeated rows on its inner side belong to the moving content as
/// much as to the bar.
fn static_edges(before: &[Sig], now: &[Sig]) -> (usize, usize) {
    let h = now.len();
    // How often each distinctive row of the frame occurs in it.
    let mut counts: HashMap<u64, u32> = HashMap::new();
    for sig in now.iter().filter(|s| !s.flat) {
        *counts.entry(sig.hash).or_default() += 1;
    }
    // A row that held still and occurs once in the frame: moving content shows one only by
    // chance, at the very edge, where the row it repeats has just left the frame. Rows that repeat
    // (a border, the stem of a letter one pixel down) hold still while a page creeps by.
    let held = |y: usize| before[y] == now[y] && !now[y].flat && counts.get(&now[y].hash) == Some(&1);
    let mut top = edge_rows(before, now, 0..h, &counts).min(h);
    let mut bottom = edge_rows(before, now, (top..h).rev(), &counts).min(h - top);
    while top > 0 && !held(top - 1) {
        top -= 1;
    }
    if (0..top).filter(|&y| held(y)).count() < MIN_DISTINCT {
        top = 0;
    }
    let mut start = h - bottom;
    while start < h && !held(start) {
        start += 1;
    }
    bottom = h - start;
    if (start..h).filter(|&y| held(y)).count() < MIN_DISTINCT {
        bottom = 0;
    }
    (top, bottom)
}

/// How many rows, from the edge inwards in the order of `rows`, belong to a sticky edge. The rows
/// that stayed in place from the edge on do. Past them, rows that changed (a spinner, a clock) and
/// rows that held still may alternate: the edge reaches as far as a run of rows that held still
/// and occur once in the frame, while at most `EDGE_CHANGE` rows changed on the way. Moving content
/// never holds such a run, so the edge stops where the content begins.
fn edge_rows(before: &[Sig], now: &[Sig], rows: impl Iterator<Item = usize>, counts: &HashMap<u64, u32>) -> usize {
    let (mut len, mut seen, mut changed, mut run) = (0, 0, 0, 0);
    let mut plain = true;
    for y in rows {
        seen += 1;
        if before[y] != now[y] {
            plain = false;
            run = 0;
            changed += 1;
            if changed > EDGE_CHANGE {
                break;
            }
        } else if plain {
            len = seen;
        } else if !now[y].flat && counts.get(&now[y].hash) == Some(&1) {
            run += 1;
            if run >= MIN_DISTINCT {
                len = seen;
            }
        }
    }
    len
}

// ----------------------------------------------------------------------------------- the rows

/// One strip of rows, compressed, or kept as it was when the encoder failed.
enum Strip {
    Png(Vec<u8>),
    Raw(Vec<u8>),
}

/// Every row any frame brought in, in the order they came, each once: compressed in strips, with
/// a thumbnail per row. Pieces refer to rows by their number here.
struct Store {
    width: usize,
    thumb_width: usize,
    sigs: Vec<Sig>,
    strips: Vec<Strip>,
    /// The RGBA rows of the strip being filled.
    pending: Vec<u8>,
    /// RGBA, `thumb_width` pixels per row.
    thumbs: Vec<u8>,
    /// How far inside its band each row was when it was taken, in rows from the nearer edge.
    depth: Vec<u16>,
    /// Whether a later frame showed the row again as it was.
    confirmed: Vec<bool>,
}

impl Store {
    fn new(width: usize, thumb_width: usize) -> Store {
        Store {
            width,
            thumb_width: thumb_width.clamp(1, width.max(1)),
            sigs: Vec::new(),
            strips: Vec::new(),
            pending: Vec::new(),
            thumbs: Vec::new(),
            depth: Vec::new(),
            confirmed: Vec::new(),
        }
    }

    /// Keeps one row of `format` pixels, taken `depth` rows inside its band, and gives its number.
    fn push(&mut self, row: &[u8], format: PixelFormat, sig: Sig, depth: u16) -> u32 {
        let bytes = self.width * 4;
        let at = self.pending.len();
        self.pending.resize(at + bytes, 0);
        match format {
            PixelFormat::Bgra8 => swizzle_bgra_rgba(&row[..bytes], &mut self.pending[at..]),
            PixelFormat::Rgba8 => self.pending[at..].copy_from_slice(&row[..bytes]),
        }
        // The thumbnail averages the pixels each of its pixels covers.
        let (w, tw) = (self.width, self.thumb_width);
        let source = &self.pending[at..at + bytes];
        for x in 0..tw {
            let x0 = x * w / tw;
            let x1 = ((x + 1) * w / tw).max(x0 + 1).min(w);
            let mut sum = [0u32; 4];
            for pixel in source[x0 * 4..x1 * 4].chunks_exact(4) {
                for c in 0..4 {
                    sum[c] += pixel[c] as u32;
                }
            }
            let n = (x1 - x0) as u32;
            self.thumbs.extend(sum.iter().map(|s| ((s + n / 2) / n) as u8));
        }
        let number = self.sigs.len() as u32;
        self.sigs.push(sig);
        self.depth.push(depth);
        self.confirmed.push(false);
        if self.pending.len() == STRIP_ROWS * bytes {
            self.compress();
        }
        number
    }

    fn compress(&mut self) {
        let rows = self.pending.len() / (self.width * 4);
        let data = std::mem::take(&mut self.pending);
        let strip = Frame {
            size: PhysicalSize::new(self.width as u32, rows as u32),
            bytes_per_row: self.width * 4,
            format: PixelFormat::Rgba8,
            color: ColorInfo::srgb(),
            scale: ScaleFactor::ONE,
            data,
        };
        self.strips.push(match strip.encode_png(PngEffort::Fastest) {
            Ok(png) => Strip::Png(png),
            Err(_) => Strip::Raw(strip.data),
        });
    }

    /// The RGBA rows of strip `i`; the last one may still be filling.
    fn strip(&self, i: usize) -> Result<Vec<u8>> {
        match self.strips.get(i) {
            None => Ok(self.pending.clone()),
            Some(Strip::Raw(rows)) => Ok(rows.clone()),
            Some(Strip::Png(png)) => {
                let mut reader = png::Decoder::new(Cursor::new(png.as_slice()))
                    .read_info()
                    .map_err(|e| CaptureError::Encode(e.to_string()))?;
                let mut rows = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
                reader.next_frame(&mut rows).map_err(|e| CaptureError::Encode(e.to_string()))?;
                Ok(rows)
            }
        }
    }

    /// Signs every row again over `columns`, laid out as `format` as the frames were, so the rows
    /// kept so far match the frames that come next. The signatures stay as they were when a strip
    /// cannot be read.
    fn resign(&mut self, columns: &[Range<usize>], format: PixelFormat) -> Result<()> {
        let bytes = self.width * 4;
        let mut row = vec![0u8; bytes];
        let mut sigs = Vec::with_capacity(self.sigs.len());
        for i in 0..=self.strips.len() {
            for source in self.strip(i)?.chunks_exact(bytes) {
                match format {
                    PixelFormat::Bgra8 => swizzle_bgra_rgba(source, &mut row),
                    PixelFormat::Rgba8 => row.copy_from_slice(source),
                }
                sigs.push(sign(&row, columns));
            }
        }
        if sigs.len() != self.sigs.len() {
            return Err(CaptureError::Backend("the scrolling capture lost rows while it read them again".into()));
        }
        self.sigs = sigs;
        Ok(())
    }

    fn thumb(&self, number: u32) -> &[u8] {
        let size = self.thumb_width * 4;
        let at = number as usize * size;
        &self.thumbs[at..at + size]
    }

    fn bytes(&self) -> u64 {
        let strips: usize = self
            .strips
            .iter()
            .map(|s| match s {
                Strip::Png(b) | Strip::Raw(b) => b.len(),
            })
            .sum();
        let per_row = std::mem::size_of::<Sig>() + std::mem::size_of::<u16>() + std::mem::size_of::<bool>();
        (strips + self.pending.len() + self.thumbs.len() + self.sigs.len() * per_row) as u64
    }
}

// --------------------------------------------------------------------------------- the pieces

/// A run of rows that belong together. Positions are the piece's own: they mean nothing in
/// another piece, and a row keeps its position while the piece grows.
#[derive(Clone, Debug)]
struct Piece {
    id: u32,
    /// The position of the first row.
    top: i64,
    rows: VecDeque<u32>,
    /// Opened by a band that fit nowhere, and not confirmed by another band yet.
    tentative: bool,
}

impl Piece {
    fn end(&self) -> i64 {
        self.top + self.rows.len() as i64
    }

    fn at(&self, position: i64) -> Option<u32> {
        if position < self.top {
            return None;
        }
        self.rows.get((position - self.top) as usize).copied()
    }
}

/// Everything `undo` puts back.
#[derive(Clone, Debug, Default)]
struct Layout {
    /// In the order they are shown, top to bottom.
    pieces: Vec<Piece>,
    /// The piece the last band was placed on, and where: frame row `y` is at `placed + y`.
    current: Option<(u32, i64)>,
    header: Vec<u32>,
    header_found: bool,
    footer: Vec<u32>,
    footer_found: bool,
    /// The way the content last moved: 1 down the page, -1 up it.
    direction: i8,
    frames: u32,
}

/// What a frame did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The first frame: the result starts with it.
    First,
    /// Nothing moved.
    Still,
    /// The band was placed, and the result grew or the view moved.
    Moved,
    /// The band fit nowhere and opened a piece of its own.
    Opened,
    /// The band joined two pieces and closed the gap between them.
    Joined,
    /// The result reached `MAX_HEIGHT`; later frames are not taken.
    Full,
    /// The frame has another size than the stitcher's.
    Ignored,
}

/// Where the gap next to the piece in view is, so the user knows which way to scroll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapSide {
    Above,
    Below,
}

/// The state of the result, for the live strip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub width: u32,
    pub height: u32,
    /// Frames that brought rows in or moved the view.
    pub frames: u32,
    pub header: u32,
    pub header_found: bool,
    pub footer: u32,
    pub footer_found: bool,
    /// Where one piece ends and the next begins, in rows of the result.
    pub gaps: Vec<u32>,
    /// The part of the result the selection shows now: its first row and its height.
    pub view: Option<(u32, u32)>,
    pub gap: Option<GapSide>,
    /// The memory the rows take.
    pub bytes: u64,
    pub full: bool,
}

pub struct Stitcher {
    size: PhysicalSize,
    color: ColorInfo,
    scale: ScaleFactor,
    store: Store,
    layout: Layout,
    /// Distinctive rows of every piece by hash: the piece and the position.
    index: HashMap<u64, Vec<(u32, i64)>>,
    /// The previous frame's rows.
    last: Vec<Sig>,
    /// The band of the previous placed frame.
    band: Range<usize>,
    undo: Vec<Layout>,
    /// The next change starts a movement, so `undo` can take it back.
    movement_due: bool,
    next_id: u32,
    full: bool,
    /// The columns rows are signed over and the result keeps.
    columns: Columns,
    /// The frame before, until a move showed which columns move with the page.
    learning: Option<Frame>,
    /// The layout of the frames' pixels, which signing the kept rows again repeats.
    format: PixelFormat,
    /// How many rows the last move brought in at the band's edge.
    brought: usize,
}

impl Stitcher {
    /// A stitcher for frames of `size`, whose result has `color` and `scale`; the preview is
    /// `thumb_width` pixels wide.
    pub fn new(size: PhysicalSize, thumb_width: u32, color: ColorInfo, scale: ScaleFactor) -> Stitcher {
        Stitcher {
            size,
            color,
            scale,
            store: Store::new(size.width as usize, thumb_width as usize),
            layout: Layout::default(),
            index: HashMap::new(),
            last: Vec::new(),
            band: 0..size.height as usize,
            undo: Vec::new(),
            movement_due: false,
            next_id: 1,
            full: false,
            columns: Columns::all(size.width as usize),
            learning: None,
            format: PixelFormat::Rgba8,
            brought: 0,
        }
    }

    pub fn size(&self) -> PhysicalSize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.layout.pieces.is_empty()
    }

    /// The next change starts a new movement: a scroll after a pause, or an automatic step.
    pub fn begin_movement(&mut self) {
        if !self.layout.pieces.is_empty() {
            self.movement_due = true;
        }
    }

    /// Takes back the rows of the last movement; `false` when there is none.
    pub fn undo(&mut self) -> bool {
        let Some(layout) = self.undo.pop() else {
            return false;
        };
        self.layout = layout;
        self.full = false;
        self.movement_due = true;
        self.brought = 0;
        self.reindex();
        true
    }

    /// The height of the result in rows.
    pub fn height(&self) -> u32 {
        let pieces: usize = self.layout.pieces.iter().map(|p| p.rows.len()).sum();
        (self.layout.header.len() + pieces + self.layout.footer.len()) as u32
    }

    /// Adds one frame of the area.
    pub fn push(&mut self, frame: &Frame) -> Step {
        if self.full {
            return Step::Full;
        }
        if frame.size != self.size {
            return Step::Ignored;
        }
        let h = self.size.height as usize;
        let mut sigs = self.sign_frame(frame);
        if self.layout.pieces.is_empty() {
            return self.first(frame, sigs);
        }
        if sigs == self.last {
            return Step::Still;
        }
        if self.learn(frame) {
            sigs = self.sign_frame(frame);
            if sigs == self.last {
                return Step::Still;
            }
        }
        if self.repainted(frame, &sigs) {
            self.last = sigs;
            return Step::Still;
        }
        let before = std::mem::replace(&mut self.last, sigs);
        let sigs = self.last.clone();
        let (top, bottom) = static_edges(&before, &sigs);
        let t = top.max(self.layout.header.len());
        let b = bottom.max(self.layout.footer.len());
        // Only a thin band changed: a clock, a caret or a hover, not a move, or the page painted
        // the rows the last move brought in.
        if t + b + MIN_OVERLAP > h {
            self.refresh_view(frame, &sigs);
            return Step::Still;
        }
        let band = t..h - b;
        let mut placement = self.place(&sigs, band.clone());
        let mut lifted = false;
        if placement.is_none() {
            placement = self.place_lifted(frame, &before, (top, bottom), &sigs, band.clone());
            lifted = placement.is_some();
        }
        let Some((id, placed, others)) = placement else {
            if self.in_place(&sigs, &band) {
                return Step::Still;
            }
            return self.open(frame, &sigs, band);
        };
        if self.layout.current == Some((id, placed)) {
            self.refresh_view(frame, &sigs);
            return Step::Still;
        }

        self.start_movement();
        let previous = self.layout.current;
        // A piece opened for a band that never fit again was a passing picture.
        let dropped = self.drop_tentative(id);
        let lifted = lifted || ((top > 0 || bottom > 0) && self.lift_edges(frame, &before, top, bottom));
        let Some(at) = self.layout.pieces.iter().position(|p| p.id == id) else {
            return Step::Still;
        };
        self.layout.pieces[at].tentative = false;
        self.brought = 0;
        if let Some((was, from)) = previous {
            if was == id && placed != from {
                self.layout.direction = if placed > from { 1 } else { -1 };
                self.brought = placed.abs_diff(from) as usize;
            }
        }
        self.refresh(at, placed, frame, &sigs, band.clone(), false);
        let grown = self.extend(at, placed, frame, &sigs, band.clone());
        let joined = self.join(at, placed, &others);
        self.layout.current = Some((id, placed));
        self.layout.frames += 1;
        self.band = band;
        if dropped || joined || lifted {
            self.reindex();
        }
        if grown.full {
            self.full = true;
            return Step::Full;
        }
        if joined {
            Step::Joined
        } else {
            Step::Moved
        }
    }

    fn first(&mut self, frame: &Frame, sigs: Vec<Sig>) -> Step {
        let id = self.new_id();
        let room = MAX_HEIGHT as usize;
        let rows = (0..sigs.len().min(room))
            .map(|y| self.store.push(row_of(frame, y), frame.format, sigs[y], depth(&(0..sigs.len()), y)))
            .collect();
        self.layout.pieces.push(Piece { id, top: 0, rows, tentative: false });
        self.layout.current = Some((id, 0));
        self.layout.frames = 1;
        self.band = 0..sigs.len();
        self.last = sigs;
        self.format = frame.format;
        self.learning = Some(frame.clone());
        self.reindex();
        Step::First
    }

    fn sign_frame(&self, frame: &Frame) -> Vec<Sig> {
        (0..self.size.height as usize).map(|y| sign(row_of(frame, y), &self.columns.signed)).collect()
    }

    /// Until a move showed which columns move with the page, each changed frame is compared with
    /// the one before. Once one did, the rows kept so far and the frame before are signed again
    /// over those columns; whether that happened.
    fn learn(&mut self, frame: &Frame) -> bool {
        let Some(before) = self.learning.take() else {
            return false;
        };
        let Some(columns) = moving_columns(&before, frame) else {
            self.learning = Some(frame.clone());
            return false;
        };
        if columns == self.columns {
            return false;
        }
        // A store that cannot be read again keeps every column, as before the move.
        if self.store.resign(&columns.signed, self.format).is_err() {
            return false;
        }
        self.columns = columns;
        self.last = self.sign_frame(&before);
        self.reindex();
        true
    }

    fn new_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn start_movement(&mut self) {
        if self.movement_due {
            self.undo.push(self.layout.clone());
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
            self.movement_due = false;
        }
    }

    /// The rows the result can still take.
    fn room(&self) -> usize {
        (MAX_HEIGHT as usize).saturating_sub(self.height() as usize)
    }

    /// The best placement of the band, and the other pieces it also fits; `None` when it fits
    /// nowhere.
    fn place(&self, sigs: &[Sig], band: Range<usize>) -> Option<(u32, i64, Vec<Spot>)> {
        let mut votes: HashMap<(u32, i64), u32> = HashMap::new();
        for y in band.clone() {
            let sig = sigs[y];
            if sig.flat {
                continue;
            }
            let Some(found) = self.index.get(&sig.hash) else { continue };
            if found.len() > COMMON {
                continue;
            }
            // A row the result holds in several places splits its vote among them, so the rows
            // that tell repeated items apart decide between their placements.
            let weight = VOTE / found.len() as u32;
            for &(piece, position) in found {
                *votes.entry((piece, position - y as i64)).or_default() += weight;
            }
        }
        let mut ranked: Vec<((u32, i64), u32)> = votes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let need = MIN_OVERLAP.max(band.len() / 5);
        let mut verified: Vec<(u32, i64, usize)> = Vec::new();
        for ((id, placed), _) in ranked.into_iter().take(CANDIDATES) {
            let Some(piece) = self.layout.pieces.iter().find(|p| p.id == id) else { continue };
            if let Some((matched, mismatched)) = self.verify(piece, placed, sigs, band.clone(), need) {
                // Verified placements agree on nine rows in ten at least, so this stays positive.
                verified.push((id, placed, matched.saturating_sub(MISMATCH_WEIGHT * mismatched)));
            }
        }
        let most = verified.iter().map(|v| v.2).max()?;
        // Among placements about as good as the best, the one nearest the last one wins, so a
        // page that repeats itself does not make the view jump.
        let best = *verified.iter().filter(|v| v.2 * 20 >= most * 19).min_by_key(|v| match self.layout.current {
            Some((id, placed)) if id == v.0 => (v.1 - placed).abs(),
            _ => i64::MAX,
        })?;
        let mut others: Vec<(u32, i64)> = Vec::new();
        for v in &verified {
            if v.0 != best.0 && !others.iter().any(|o| o.0 == v.0) {
                others.push((v.0, v.1));
            }
        }
        Some((best.0, best.1, others))
    }

    /// The rows that match and the rows that do not when the band at `placed` overlaps `piece`
    /// enough and agrees with it.
    fn verify(
        &self,
        piece: &Piece,
        placed: i64,
        sigs: &[Sig],
        band: Range<usize>,
        need: usize,
    ) -> Option<(usize, usize)> {
        let (mut overlap, mut distinct, mut matched) = (0usize, 0usize, 0usize);
        for y in band {
            let Some(number) = piece.at(placed + y as i64) else { continue };
            overlap += 1;
            let sig = sigs[y];
            let kept = self.store.sigs[number as usize];
            // An empty row no frame confirmed may be one the page had not painted yet.
            if sig.flat || (kept.flat && !self.store.confirmed[number as usize]) {
                continue;
            }
            distinct += 1;
            if kept.hash == sig.hash {
                matched += 1;
            }
        }
        let agrees = matched as f64 >= MATCH_SHARE * distinct as f64;
        (overlap >= need && distinct >= MIN_DISTINCT && agrees).then_some((matched, distinct - matched))
    }

    /// A band that fits nowhere: a new piece next to the current one, in the way the content was
    /// moving. It stays tentative until another band fits it.
    fn open(&mut self, frame: &Frame, sigs: &[Sig], band: Range<usize>) -> Step {
        self.start_movement();
        self.drop_tentative(0);
        self.brought = 0;
        let room = self.room();
        if room == 0 {
            self.full = true;
            return Step::Full;
        }
        let id = self.new_id();
        let take = band.start..band.end.min(band.start + room);
        let rows =
            take.clone().map(|y| self.store.push(row_of(frame, y), frame.format, sigs[y], depth(&band, y))).collect();
        let piece = Piece { id, top: band.start as i64, rows, tentative: true };
        let current = self.layout.current.and_then(|(cid, _)| self.layout.pieces.iter().position(|p| p.id == cid));
        let at = match current {
            Some(i) if self.layout.direction < 0 => i,
            Some(i) => i + 1,
            None => self.layout.pieces.len(),
        };
        self.layout.pieces.insert(at, piece);
        self.layout.current = Some((id, 0));
        self.layout.frames += 1;
        self.band = band;
        self.reindex();
        if take.len() < self.band.len() {
            self.full = true;
            return Step::Full;
        }
        Step::Opened
    }

    /// Removes a tentative piece other than `keep`; whether one went.
    fn drop_tentative(&mut self, keep: u32) -> bool {
        let before = self.layout.pieces.len();
        self.layout.pieces.retain(|p| !p.tentative || p.id == keep);
        before != self.layout.pieces.len()
    }

    /// The first move that shows a sticky header or bottom bar: its rows leave the pieces the
    /// earlier frames brought them into, where they would repeat, and go to the header or the
    /// footer, which the result shows once. A second move confirms it.
    /// Whether rows left a piece.
    fn lift_edges(&mut self, frame: &Frame, before: &[Sig], top: usize, bottom: usize) -> bool {
        let h = self.size.height as usize;
        let mut moved = false;
        if top > 0 {
            if !self.layout.header.is_empty() {
                self.layout.header_found = true;
            } else {
                let taken = self.take_edge(before, 0..top, true);
                moved |= taken.is_some();
                self.layout.header = match taken {
                    Some(rows) => rows,
                    None => (0..top)
                        .map(|y| self.store.push(row_of(frame, y), frame.format, self.last[y], u16::MAX))
                        .collect(),
                };
            }
        }
        if bottom > 0 {
            if !self.layout.footer.is_empty() {
                self.layout.footer_found = true;
            } else {
                let taken = self.take_edge(before, h - bottom..h, false);
                moved |= taken.is_some();
                self.layout.footer = match taken {
                    Some(rows) => rows,
                    None => (h - bottom..h)
                        .map(|y| self.store.push(row_of(frame, y), frame.format, self.last[y], u16::MAX))
                        .collect(),
                };
            }
        }
        // A piece the header and the footer emptied is gone; nothing of it shows.
        self.layout.pieces.retain(|p| !p.rows.is_empty());
        moved
    }

    /// Takes the frame rows `edge` of a sticky edge out of every piece that begins (`start`) or
    /// ends with them: each row that held still from `before` to this frame matches, and rows that
    /// changed (a spinner) are let through. The rows of the first such piece, or `None`.
    fn take_edge(&mut self, before: &[Sig], edge: Range<usize>, start: bool) -> Option<Vec<u32>> {
        let n = edge.len();
        let want: Vec<Option<Sig>> = edge.map(|y| (before[y] == self.last[y]).then_some(self.last[y])).collect();
        let mut taken = None;
        for at in 0..self.layout.pieces.len() {
            let piece = &self.layout.pieces[at];
            if piece.rows.len() < n {
                continue;
            }
            let skip = if start { 0 } else { piece.rows.len() - n };
            let holds = want
                .iter()
                .enumerate()
                .all(|(i, sig)| sig.is_none_or(|sig| self.store.sigs[piece.rows[skip + i] as usize] == sig));
            if !holds {
                continue;
            }
            let piece = &mut self.layout.pieces[at];
            let rows: Vec<u32> = if start {
                piece.top += n as i64;
                piece.rows.drain(..n).collect()
            } else {
                piece.rows.drain(skip..).collect()
            };
            taken.get_or_insert(rows);
        }
        taken
    }

    /// A band that fits nowhere on the first move that shows a sticky edge may fit once the edge's
    /// rows leave the piece: a move longer than a tenth of the band puts the earlier frame's bar
    /// where the band's content now is, and that contradicts the placement. The edges stay lifted
    /// only when the band fits then.
    fn place_lifted(
        &mut self,
        frame: &Frame,
        before: &[Sig],
        (top, bottom): (usize, usize),
        sigs: &[Sig],
        band: Range<usize>,
    ) -> Option<(u32, i64, Vec<Spot>)> {
        let fresh = (top > 0 && self.layout.header.is_empty()) || (bottom > 0 && self.layout.footer.is_empty());
        if !fresh {
            return None;
        }
        let saved = self.layout.clone();
        let due = self.movement_due;
        self.start_movement();
        if self.lift_edges(frame, before, top, bottom) {
            self.reindex();
            if let Some(placement) = self.place(sigs, band) {
                return Some(placement);
            }
        }
        self.layout = saved;
        if due {
            self.undo.pop();
            self.movement_due = true;
        }
        self.reindex();
        None
    }

    /// Takes the band's view of rows of piece `at` that no frame confirmed yet. A row taken at a
    /// band's edge may be one the page had not painted yet, or a line of the window that stays at
    /// the edge (a border one row high), and either would stay in the result between rows of the
    /// page. A row is confirmed once a later frame shows it again as it was; until then, a view of
    /// it farther inside a band that differs replaces it. A confirmed row stays, so a button that
    /// floats in the middle of the page never enters the rows that pass under it.
    ///
    /// A frame of the same view (`same`, nothing moved) neither confirms a row, since it may
    /// show the page before it painted as well, nor needs to see it farther inside: it is newer.
    fn refresh(&mut self, at: usize, placed: i64, frame: &Frame, sigs: &[Sig], band: Range<usize>, same: bool) {
        let id = self.layout.pieces[at].id;
        for y in band.clone() {
            let position = placed + y as i64;
            let Some(number) = self.layout.pieces[at].at(position) else { continue };
            let n = number as usize;
            if self.store.confirmed[n] {
                continue;
            }
            let inside = depth(&band, y);
            // An empty row stays open: the page may not have painted it on either frame.
            if self.store.sigs[n] == sigs[y] {
                self.store.confirmed[n] |= !same && !sigs[y].flat;
                self.store.depth[n] = self.store.depth[n].max(inside);
                continue;
            }
            if inside < self.store.depth[n] || (inside == self.store.depth[n] && !same) {
                continue;
            }
            let old = self.store.sigs[n];
            if !old.flat {
                if let Some(spots) = self.index.get_mut(&old.hash) {
                    spots.retain(|&spot| spot != (id, position));
                    if spots.is_empty() {
                        self.index.remove(&old.hash);
                    }
                }
            }
            let fresh = self.store.push(row_of(frame, y), frame.format, sigs[y], inside);
            let piece = &mut self.layout.pieces[at];
            let i = (position - piece.top) as usize;
            piece.rows[i] = fresh;
            self.remember(id, position, sigs[y]);
        }
    }

    /// Whether the frame only painted, in place, rows the last move brought in: every row that
    /// changed lies no deeper inside the band than that move reached, where the result holds a
    /// row no frame confirmed and no deeper view took, and less than half the frame changed. A page paints what
    /// scrolled in a moment late, and with a still header above it that would look like a move.
    /// The new rows replace the old ones.
    fn repainted(&mut self, frame: &Frame, sigs: &[Sig]) -> bool {
        let Some((id, placed)) = self.layout.current else { return false };
        let Some(at) = self.layout.pieces.iter().position(|p| p.id == id) else { return false };
        let changed: Vec<usize> = (0..sigs.len()).filter(|&y| self.last[y] != sigs[y]).collect();
        if changed.is_empty() || changed.len() * 2 > sigs.len() {
            return false;
        }
        let piece = &self.layout.pieces[at];
        let fresh = changed.iter().all(|&y| {
            let inside = depth(&self.band, y);
            self.band.contains(&y)
                && inside as usize <= self.brought
                && piece
                    .at(placed + y as i64)
                    .is_some_and(|n| !self.store.confirmed[n as usize] && self.store.depth[n as usize] <= inside)
        });
        if fresh {
            self.refresh(at, placed, frame, sigs, self.band.clone(), true);
        }
        fresh
    }

    /// Whether a band that fits nowhere changed in place: more distinctive rows of the view stayed
    /// where they were, matching the result there, than the band holds. A caret that blinks on
    /// the last line or a line being typed is not a move, and a piece of its own would repeat it.
    fn in_place(&self, sigs: &[Sig], band: &Range<usize>) -> bool {
        let Some((id, placed)) = self.layout.current else { return false };
        let Some(piece) = self.layout.pieces.iter().find(|p| p.id == id) else { return false };
        let held = self
            .band
            .clone()
            .filter(|y| !band.contains(y))
            .filter(|&y| {
                let sig = sigs[y];
                !sig.flat && piece.at(placed + y as i64).is_some_and(|n| self.store.sigs[n as usize] == sig)
            })
            .count();
        held >= MIN_OVERLAP.max(band.len())
    }

    /// A frame that did not move the view: the rows of the view it shows anew replace those no
    /// frame confirmed yet.
    fn refresh_view(&mut self, frame: &Frame, sigs: &[Sig]) {
        let Some((id, placed)) = self.layout.current else { return };
        let Some(at) = self.layout.pieces.iter().position(|p| p.id == id) else { return };
        self.refresh(at, placed, frame, sigs, self.band.clone(), true);
    }

    /// Adds the band's rows that lie outside piece `at` to it, above and below.
    fn extend(&mut self, at: usize, placed: i64, frame: &Frame, sigs: &[Sig], band: Range<usize>) -> Grown {
        let mut room = self.room();
        let mut grown = Grown { full: false };
        let (top, end, id) = {
            let piece = &self.layout.pieces[at];
            (piece.top, piece.end(), piece.id)
        };
        // Above: from the row next to the piece upwards, so each one is pushed onto the front.
        let above: Vec<usize> = band.clone().filter(|&y| placed + (y as i64) < top).collect();
        for &y in above.iter().rev() {
            if room == 0 {
                grown.full = true;
                break;
            }
            let number = self.store.push(row_of(frame, y), frame.format, sigs[y], depth(&band, y));
            let piece = &mut self.layout.pieces[at];
            piece.rows.push_front(number);
            piece.top -= 1;
            let position = piece.top;
            room -= 1;
            self.remember(id, position, sigs[y]);
        }
        for y in band.clone().filter(|&y| placed + (y as i64) >= end) {
            if room == 0 {
                grown.full = true;
                break;
            }
            let number = self.store.push(row_of(frame, y), frame.format, sigs[y], depth(&band, y));
            let piece = &mut self.layout.pieces[at];
            let position = piece.end();
            piece.rows.push_back(number);
            room -= 1;
            self.remember(id, position, sigs[y]);
        }
        if room == 0 && !grown.full && self.height() >= MAX_HEIGHT {
            grown.full = true;
        }
        grown
    }

    /// Joins the piece next to piece `at` that the band also fits into it, closing the gap.
    fn join(&mut self, at: usize, placed: i64, others: &[(u32, i64)]) -> bool {
        for &(other, other_placed) in others {
            let Some(ot) = self.layout.pieces.iter().position(|p| p.id == other) else { continue };
            if ot + 1 != at && ot != at + 1 {
                continue;
            }
            // A row at `position` of the other piece is at `position + shift` of this one.
            let shift = placed - other_placed;
            let piece = self.layout.pieces[at].clone();
            let next = &self.layout.pieces[ot];
            let mut rows = piece.rows.clone();
            let mut top = piece.top;
            if ot == at + 1 {
                if next.top + shift > piece.end() {
                    continue;
                }
                for (i, &number) in next.rows.iter().enumerate() {
                    if next.top + i as i64 + shift >= piece.end() {
                        rows.push_back(number);
                    }
                }
            } else {
                if next.end() + shift < piece.top {
                    continue;
                }
                for (i, &number) in next.rows.iter().enumerate().rev() {
                    if next.top + i as i64 + shift < piece.top {
                        rows.push_front(number);
                        top -= 1;
                    }
                }
            }
            // Only what the result can still take joins; the rest of the other piece is left out.
            let added = rows.len() - piece.rows.len();
            let room = self.room() + next.rows.len();
            if added > room {
                let cut = added - room;
                if ot == at + 1 {
                    rows.truncate(rows.len() - cut);
                } else {
                    rows.drain(..cut);
                    top += cut as i64;
                }
            }
            let joined = &mut self.layout.pieces[at];
            joined.rows = rows;
            joined.top = top;
            joined.tentative = false;
            self.layout.pieces.remove(ot);
            return true;
        }
        false
    }

    fn remember(&mut self, piece: u32, position: i64, sig: Sig) {
        if !sig.flat {
            self.index.entry(sig.hash).or_default().push((piece, position));
        }
    }

    fn reindex(&mut self) {
        self.index.clear();
        for piece in &self.layout.pieces {
            for (i, &number) in piece.rows.iter().enumerate() {
                let sig = self.store.sigs[number as usize];
                if !sig.flat {
                    self.index.entry(sig.hash).or_default().push((piece.id, piece.top + i as i64));
                }
            }
        }
    }

    /// The rows of the result in order: the header, the pieces, the footer.
    fn order(&self) -> Vec<u32> {
        let mut order = Vec::with_capacity(self.height() as usize);
        order.extend(&self.layout.header);
        for piece in &self.layout.pieces {
            order.extend(&piece.rows);
        }
        order.extend(&self.layout.footer);
        order.truncate(MAX_HEIGHT as usize);
        order
    }

    pub fn progress(&self) -> Progress {
        let header = self.layout.header.len() as u32;
        let mut gaps = Vec::new();
        let mut y = header;
        let mut view = None;
        let mut gap = None;
        let count = self.layout.pieces.len();
        for (i, piece) in self.layout.pieces.iter().enumerate() {
            if i > 0 {
                gaps.push(y);
            }
            if let Some((_, placed)) = self.layout.current.filter(|(id, _)| *id == piece.id) {
                // The moving part of the selection: the band, where it was placed.
                let first = (y as i64 + placed + self.band.start as i64 - piece.top).max(0) as u32;
                view = Some((first, self.band.len() as u32));
                if count > 1 {
                    gap = Some(if i > 0 { GapSide::Above } else { GapSide::Below });
                }
            }
            y += piece.rows.len() as u32;
        }
        Progress {
            width: self.columns.kept.len() as u32,
            height: self.height().min(MAX_HEIGHT),
            frames: self.layout.frames,
            header,
            header_found: self.layout.header_found,
            footer: self.layout.footer.len() as u32,
            footer_found: self.layout.footer_found,
            gaps,
            view,
            gap,
            bytes: self.store.bytes(),
            full: self.full,
        }
    }

    /// The result shrunk to the thumbnail width, for the strip beside the selection.
    pub fn preview(&self) -> Frame {
        let order = self.order();
        let tw = self.store.thumb_width;
        let w = self.size.width as usize;
        let kept = &self.columns.kept;
        let from = (kept.start * tw / w).min(tw - 1);
        let to = (kept.end * tw).div_ceil(w).clamp(from + 1, tw);
        let pw = to - from;
        let height = (order.len() * tw).div_ceil(w).max(1);
        let mut data = Vec::with_capacity(height * pw * 4);
        for j in 0..height {
            let row = ((j * w + w / 2) / tw).min(order.len().saturating_sub(1));
            match order.get(row) {
                Some(&number) => data.extend_from_slice(&self.store.thumb(number)[from * 4..to * 4]),
                None => data.resize(data.len() + pw * 4, 0),
            }
        }
        Frame {
            size: PhysicalSize::new(pw as u32, height as u32),
            bytes_per_row: pw * 4,
            format: PixelFormat::Rgba8,
            color: self.color.clone(),
            scale: self.scale,
            data,
        }
    }

    /// The whole result as one frame, of the columns that moved with the page. Pieces a gap still
    /// parts follow each other end to end.
    pub fn finish(&self) -> Result<Frame> {
        let order = self.order();
        if order.is_empty() {
            return Err(CaptureError::InvalidArgument("nothing was captured".into()));
        }
        let bytes = self.size.width as usize * 4;
        let kept = self.columns.kept.start * 4..self.columns.kept.end * 4;
        let out_bytes = kept.len();
        let mut data = vec![0u8; out_bytes * order.len()];
        // Each strip is decoded once, for all of its rows.
        let mut by_strip: Vec<Vec<(usize, usize)>> = Vec::new();
        for (out, &number) in order.iter().enumerate() {
            let strip = number as usize / STRIP_ROWS;
            if by_strip.len() <= strip {
                by_strip.resize_with(strip + 1, Vec::new);
            }
            by_strip[strip].push((number as usize % STRIP_ROWS, out));
        }
        for (strip, rows) in by_strip.iter().enumerate() {
            if rows.is_empty() {
                continue;
            }
            let pixels = self.store.strip(strip)?;
            for &(row, out) in rows {
                let from = row * bytes;
                let source = pixels.get(from..from + bytes).ok_or_else(|| {
                    CaptureError::Backend(format!("strip {strip} is missing row {row} of the scrolling capture"))
                })?;
                data[out * out_bytes..(out + 1) * out_bytes].copy_from_slice(&source[kept.clone()]);
            }
        }
        Frame::new(
            PhysicalSize::new(self.columns.kept.len() as u32, order.len() as u32),
            out_bytes,
            PixelFormat::Rgba8,
            self.color.clone(),
            self.scale,
            data,
        )
    }
}

/// What extending a piece did.
struct Grown {
    /// The result reached `MAX_HEIGHT` and rows were left out.
    full: bool,
}

/// How far row `y` lies inside `band`, in rows from the nearer edge.
fn depth(band: &Range<usize>, y: usize) -> u16 {
    (y - band.start).min(band.end - 1 - y).min(u16::MAX as usize) as u16
}

/// Row `y` of `frame`, without its padding.
fn row_of(frame: &Frame, y: usize) -> &[u8] {
    let at = y * frame.bytes_per_row;
    &frame.data[at..at + frame.size.width as usize * 4]
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 64;

    fn mix(mut x: u64) -> u64 {
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^ (x >> 33)
    }

    /// A row of glyph-like runs drawn from `seed`, or a blank one; `border` adds a 1 px rule at the
    /// left, so blank rows repeat without being flat.
    fn row(seed: Option<u64>, border: bool) -> Vec<u8> {
        let mut r = vec![255u8; W * 4];
        if let Some(seed) = seed {
            for x in 6..W - 6 {
                let v = mix(seed.wrapping_mul(1_000_003) ^ (x as u64 / 3));
                if v.is_multiple_of(3) {
                    let c = (v >> 8) as u8 % 120;
                    r[x * 4..x * 4 + 3].copy_from_slice(&[c, c, c.wrapping_add(30)]);
                }
            }
        }
        if border {
            r[8..11].copy_from_slice(&[200, 200, 200]);
        }
        r
    }

    /// Lines of text: 14 rows of ink, then 6 blank rows.
    fn text(rows: usize, salt: u64, border: bool) -> Vec<Vec<u8>> {
        (0..rows).map(|y| row((y % 20 < 14).then_some(salt * 1_000_000 + y as u64), border)).collect()
    }

    /// A toolbar: ink on every row.
    fn bar(rows: usize, salt: u64) -> Vec<Vec<u8>> {
        (0..rows).map(|y| row(Some(salt * 1_000_000 + y as u64), false)).collect()
    }

    fn frame(rows: &[Vec<u8>]) -> Frame {
        let data = rows.concat();
        Frame::new(
            PhysicalSize::new(W as u32, rows.len() as u32),
            W * 4,
            PixelFormat::Rgba8,
            ColorInfo::srgb(),
            ScaleFactor(2.0),
            data,
        )
        .unwrap()
    }

    /// Stitches `frames` and gives the rows of the result and its gaps.
    fn stitch(frames: &[Vec<Vec<u8>>]) -> (Vec<Vec<u8>>, Vec<u32>) {
        let h = frames[0].len() as u32;
        let mut stitcher = Stitcher::new(PhysicalSize::new(W as u32, h), 8, ColorInfo::srgb(), ScaleFactor(2.0));
        for rows in frames {
            stitcher.begin_movement();
            stitcher.push(&frame(rows));
        }
        let out = stitcher.finish().unwrap();
        let width = out.size.width as usize * 4;
        let rows = out.data.chunks_exact(out.bytes_per_row).map(|r| r[..width].to_vec()).collect();
        (rows, stitcher.progress().gaps)
    }

    /// The selection over a page with a sticky header and bottom bar, scrolled to `offset`.
    fn view(header: &[Vec<u8>], content: &[Vec<u8>], footer: &[Vec<u8>], h: usize, offset: usize) -> Vec<Vec<u8>> {
        let mid = h - header.len() - footer.len();
        [header, &content[offset..offset + mid], footer].concat()
    }

    // A first move longer than a tenth of the band put the first frame's bottom bar where content
    // now was, so the band fit nowhere: the result held the first frame as a piece of its own, and
    // scrolling back over the gap brought the header in twice.
    #[test]
    fn a_sticky_bar_enters_the_result_once_after_a_long_first_move() {
        let (header, footer, content) = (bar(30, 3), bar(20, 4), text(1500, 5, false));
        let h = 200;
        let mid = h - 50;
        let automatic: Vec<usize> = (0..=1200).step_by(70).collect();
        let mut flick = vec![0, 600];
        flick.extend((1..=5).map(|i| 600 + 40 * i));
        flick.extend((0..20).rev().map(|i| 40 * i));
        flick.extend((1..=25).map(|i| 40 * i));
        for (name, offsets) in [("automatic", automatic), ("flick and back", flick)] {
            let frames: Vec<_> = offsets.iter().map(|&o| view(&header, &content, &footer, h, o)).collect();
            let end = offsets.iter().max().unwrap() + mid;
            let expected = [&header[..], &content[..end], &footer[..]].concat();
            let (rows, gaps) = stitch(&frames);
            assert!(gaps.is_empty(), "{name}: gaps at {gaps:?}");
            assert_eq!(rows.len(), expected.len(), "{name}");
            assert!(rows == expected, "{name}: the rows differ from the page");
        }
    }

    // Rows that changed on every frame inside a sticky bar (a spinner) cut the bar short, so its
    // upper rows travelled with the content and opened a gap on every move.
    #[test]
    fn a_spinner_in_a_sticky_bar_does_not_split_the_result() {
        let content = text(1500, 5, false);
        let h = 200;
        let offsets: Vec<usize> = (0..=1200).step_by(30).collect();
        let footer = |tick: u64| -> Vec<Vec<u8>> {
            let mut rows = bar(40, 4);
            for r in &mut rows[10..20] {
                *r = row(Some(7_000_000 + tick), false);
            }
            rows
        };
        let frames: Vec<_> =
            offsets.iter().enumerate().map(|(i, &o)| view(&[], &content, &footer(i as u64), h, o)).collect();
        let end = offsets.last().unwrap() + h - 40;
        let expected = [&content[..end], &footer(0)[..]].concat();
        let (rows, gaps) = stitch(&frames);
        assert!(gaps.is_empty(), "gaps at {gaps:?}");
        assert_eq!(rows.len(), expected.len());
        assert!(rows == expected, "the rows differ from the page");
    }

    // A page that creeps by a pixel or two shows rows that repeat (a rule, the stem of a letter)
    // in the same place on two frames; they made a sticky header of a few content rows.
    #[test]
    fn a_page_creeping_by_a_pixel_keeps_every_row() {
        let content = text(800, 7, true);
        let h = 200;
        for step in [1, 2, 3] {
            for down in [true, false] {
                let mut offsets: Vec<usize> = (0..=300).step_by(step).collect();
                if !down {
                    offsets.reverse();
                }
                let frames: Vec<_> = offsets.iter().map(|&o| content[o..o + h].to_vec()).collect();
                let expected = &content[..offsets.iter().max().unwrap() + h];
                let (rows, gaps) = stitch(&frames);
                assert!(gaps.is_empty(), "step {step}, down {down}: gaps at {gaps:?}");
                assert_eq!(rows.len(), expected.len(), "step {step}, down {down}");
                assert!(rows == expected, "step {step}, down {down}: the rows differ from the page");
            }
        }
    }

    // An area over a window holds more than the page: a side bar that stays in place and a scroll
    // bar whose knob runs its own way. Signed over the whole width, no row matched where the page
    // moved it, so the result was the first frame and the last one end to end.
    #[test]
    fn a_side_bar_and_a_scroll_bar_stay_out_of_the_result() {
        let (content, side) = (text(1500, 5, false), text(200, 9, false));
        let h = 200;
        let page = |offset: usize, sidebar: bool, knob: bool| -> Vec<Vec<u8>> {
            (0..h)
                .map(|y| {
                    let mut r = content[offset + y].clone();
                    if sidebar {
                        r[..16 * 4].copy_from_slice(&side[y][..16 * 4]);
                    }
                    if knob {
                        let on = (offset * h / 1500..(offset + h) * h / 1500).contains(&y);
                        let c = if on { 90 } else { 250 };
                        for x in W - 4..W {
                            r[x * 4..x * 4 + 3].copy_from_slice(&[c, c, c]);
                        }
                    }
                    r
                })
                .collect()
        };
        let offsets: Vec<usize> = (0..=1200).step_by(30).collect();
        // The side bar's columns and the knob's go; the page's empty margin stays.
        for (sidebar, knob, kept) in [(true, false, 16..W), (false, true, 0..W - 4), (true, true, 16..W - 4)] {
            let frames: Vec<_> = offsets.iter().map(|&o| page(o, sidebar, knob)).collect();
            let expected: Vec<Vec<u8>> =
                content[..1200 + h].iter().map(|r| r[kept.start * 4..kept.end * 4].to_vec()).collect();
            let (rows, gaps) = stitch(&frames);
            assert!(gaps.is_empty(), "side bar {sidebar}, knob {knob}: gaps at {gaps:?}");
            assert_eq!(rows.len(), expected.len(), "side bar {sidebar}, knob {knob}");
            assert!(rows == expected, "side bar {sidebar}, knob {knob}: the rows differ from the page");
        }
    }

    // A row the window keeps at the band's edge (its border) entered the result on every move,
    // between rows of the page, and a page that painted what scrolled in a moment late left those
    // rows empty. Both split the result into pieces once enough of them gathered in a band.
    #[test]
    fn an_edge_row_and_rows_painted_late_leave_the_result() {
        let content = text(1500, 5, false);
        let h = 200;
        let step = 30;
        let border = vec![150u8; W * 4];
        let offsets: Vec<usize> = (0..=900).step_by(step).collect();
        let view = |offset: usize, painted: bool| -> Vec<Vec<u8>> {
            let mut rows = content[offset..offset + h - 1].to_vec();
            if !painted {
                for r in &mut rows[h - 1 - step..] {
                    *r = vec![255u8; W * 4];
                }
            }
            rows.push(border.clone());
            rows
        };
        let mut frames: Vec<_> = offsets.iter().enumerate().map(|(i, &o)| view(o, i == 0)).collect();
        frames.push(view(900, true));
        let expected = [&content[..900 + h - 1], std::slice::from_ref(&border)].concat();
        let (rows, gaps) = stitch(&frames);
        assert!(gaps.is_empty(), "gaps at {gaps:?}");
        assert_eq!(rows.len(), expected.len());
        assert!(rows == expected, "the rows differ from the page");
    }
}
