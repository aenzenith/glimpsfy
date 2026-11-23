//! Windows.Media.Ocr: the text recognition built into Windows 10 and 11, which reads the
//! languages whose recogniser is installed (a language's "Optical character recognition"
//! feature in Settings › Time & language). Codes are found by [`crate::barcodes`], because
//! Windows has no detector for them.
//!
//! # Languages
//!
//! One recogniser reads one language. The languages of the request are matched against the
//! installed recognisers the way the Vision engine matches its own (`tr` finds `tr-TR`), and
//! the ones without a recogniser are dropped; when none is left, the recogniser of the user's
//! profile languages reads, and only a machine with no recogniser at all answers
//! [`OcrError::LanguageUnavailable`]. With several languages the first part of the image that
//! holds text is read with each of at most [`MAX_RECOGNIZERS`] recognisers, the reading that
//! kept the most text decides ([`better`]), and the rest of the image is read with that
//! recogniser alone.
//!
//! # Large images
//!
//! A recogniser takes at most `OcrEngine.MaxImageDimension` pixels a side (2600). Shrinking a
//! 4K capture to that would blur its small text, so a larger image is read in tiles that
//! overlap by at least [`OVERLAP`] pixels, and [`seams`] puts the tiles' words together: a word
//! in an overlap is taken from the tile that sees it whole and nearer its middle, and the two
//! halves of a line that crosses a seam become one line again.
//!
//! # Boxes
//!
//! Windows boxes words, not lines, and has no confidence, so a line's box is the union of its
//! words' and every confidence is 1. When the recogniser straightened the image first
//! (`OcrResult.TextAngle`), its boxes are turned back by that angle around the middle of the
//! image, and each word gets the upright box that encloses it, as the crate's contract asks.
//!
//! # Threads
//!
//! The calls block the worker thread they are made on: a WinRT operation is waited for with its
//! completion handler and a channel, and the wait looks at the [`CancelToken`] every few
//! milliseconds. WinRT needs an apartment. The process is given a multithreaded one that no
//! thread owns (`CoIncrementMTAUsage`), which every thread without an apartment of its own then
//! belongs to; a thread that already chose one keeps it. `RoInitialize` on the calling thread
//! would have to be undone on that same thread and fails on a thread in a single-threaded
//! apartment, so it is not used.
//!
//! Nothing in this file could be run where it was written; it is checked against Microsoft's
//! documentation and the `windows` crate's signatures only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use ::windows::core::{Interface, RuntimeType};
use ::windows::Globalization::Language;
use ::windows::Graphics::Imaging::{BitmapAlphaMode, BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap};
use ::windows::Media::Ocr::{OcrEngine as Recognizer, OcrResult as Reading};
use ::windows::System::UserProfile::GlobalizationPreferences;
use ::windows::Win32::System::Com::CoIncrementMTAUsage;
use ::windows::Win32::System::WinRT::IMemoryBufferByteAccess;
use glimpsfy_capture::{Frame, PixelFormat};
use windows_future::{AsyncOperationCompletedHandler, AsyncStatus, IAsyncOperation};

use self::seams::{Piece, Tile};
use crate::{
    barcodes, Bounds, CancelToken, CodeResult, EngineId, LanguageInfo, LanguageTag, OcrEngine, OcrError, OcrWord,
    RawText, ReadOptions, Result,
};

/// How many recognisers may read the same pixels to find the one that fits. Every one of them
/// costs a whole reading, so the list of the settings is cut here.
const MAX_RECOGNIZERS: usize = 3;

/// The least two neighbouring tiles share, in pixels. A word or a line of text has to lie whole
/// inside one tile to be read right, so the overlap is wider than a long word and taller than a
/// line at the sizes text has on a screen, also after the pipeline enlarged small text.
const OVERLAP: usize = 400;

/// The least side of the bitmap a recogniser is given; a smaller image is padded with white.
/// The engine refuses images under 40 pixels a side, and text that touches the edge reads worse.
const MIN_SIDE: usize = 64;

/// How often a waiting call looks at the cancel token.
const POLL: Duration = Duration::from_millis(15);

/// Windows reports no confidence, so every word gets the same one.
const CONFIDENCE: f64 = 1.0;

/// Reads text with the engine built into Windows.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsOcrEngine;

impl OcrEngine for WindowsOcrEngine {
    fn id(&self) -> EngineId {
        EngineId::WindowsMediaOcr
    }

    fn read(&self, image: &Frame, options: &ReadOptions, cancel: &CancelToken) -> Result<RawText> {
        cancel.check()?;
        let (width, height) = (image.size.width as usize, image.size.height as usize);
        if width == 0 || height == 0 {
            return Ok(RawText::default());
        }
        // A frame is checked when it is made, but its fields are public, and the rows are
        // sliced below.
        let needed = image.bytes_per_row * (height - 1) + width * 4;
        if image.bytes_per_row < width * 4 || image.data.len() < needed {
            return Err(OcrError::InvalidImage("the pixel buffer is shorter than its size".into()));
        }
        apartment()?;
        let recognizers = recognizers(options)?;
        let limit = Recognizer::MaxImageDimension().map_err(unavailable)? as usize;
        if limit < MIN_SIDE {
            return Err(OcrError::Engine(format!("Windows reads images of at most {limit} pixels a side")));
        }

        let tiles = seams::tiles(width, height, limit, OVERLAP);
        // The recogniser that reads this image, once a part with text has shown which one fits.
        let mut chosen = (recognizers.len() == 1).then_some(0);
        let mut pieces: Vec<Piece> = Vec::new();
        for (index, tile) in tiles.iter().enumerate() {
            cancel.check()?;
            let bitmap = bitmap_of(image, tile)?;
            let lines = match chosen {
                Some(which) => recognize(&recognizers[which].engine, &bitmap, tile, image, cancel)?,
                None => {
                    let mut best: Option<(usize, Vec<Vec<OcrWord>>)> = None;
                    for (which, recognizer) in recognizers.iter().enumerate() {
                        cancel.check()?;
                        let lines = recognize(&recognizer.engine, &bitmap, tile, image, cancel)?;
                        if best.as_ref().is_none_or(|(_, kept)| better(&lines, kept)) {
                            best = Some((which, lines));
                        }
                    }
                    let (which, lines) = best.unwrap_or_default();
                    // A part without text says nothing about the language; the next part asks again.
                    if !lines.is_empty() {
                        chosen = Some(which);
                    }
                    lines
                }
            };
            // Closing frees the tile's pixels now instead of when the last reference goes. A
            // bitmap that cannot be closed is freed then, so the answer does not matter.
            let _ = bitmap.Close();
            pieces.extend(seams::owned(lines, index, tile));
        }
        cancel.check()?;
        // The recogniser's language is reported only when the text chose it among several.
        let language = match chosen {
            Some(which) if recognizers.len() > 1 => Some(recognizers[which].tag.clone()),
            _ => None,
        };
        Ok(RawText { lines: seams::lines(pieces, tiles.len() > 1, CONFIDENCE), language })
    }

    fn supported_languages(&self) -> Result<Vec<LanguageInfo>> {
        apartment()?;
        // Only installed recognisers are listed. Windows can install more, but it does not say
        // which or how large they are, and a language without a size to download has no place
        // in the list.
        Ok(installed()?
            .into_iter()
            .map(|(tag, _)| LanguageInfo { tag, available: true, download_bytes: None })
            .collect())
    }

    fn detect_codes(&self, image: &Frame, cancel: &CancelToken) -> Result<Vec<CodeResult>> {
        barcodes::detect(image, cancel)
    }

    fn corrects_language(&self) -> bool {
        // Windows offers no switch for its language model, so a second reading without it would
        // read the same.
        false
    }
}

// ----------------------------------------------------------------------------------- languages

/// A recogniser and the language it reads.
struct Candidate {
    engine: Recognizer,
    tag: LanguageTag,
}

/// The recognisers that read a request, the most wanted first; never empty.
fn recognizers(options: &ReadOptions) -> Result<Vec<Candidate>> {
    let installed = installed()?;
    let mut candidates: Vec<Candidate> = Vec::new();
    let tags: Vec<LanguageTag> = installed.iter().map(|(tag, _)| tag.clone()).collect();
    for index in resolve(&options.languages, &tags).into_iter().take(MAX_RECOGNIZERS) {
        let (tag, language) = &installed[index];
        // A recogniser that is listed and cannot be made is skipped like one that is missing.
        if let Ok(engine) = Recognizer::TryCreateFromLanguage(language) {
            candidates.push(Candidate { engine, tag: tag.clone() });
        }
    }
    // The user's own languages read when the request names none that is installed, and join
    // the candidates when the request lets the engine look for the language itself.
    if candidates.is_empty() || (options.detect_language && candidates.len() < MAX_RECOGNIZERS) {
        if let Ok(engine) = Recognizer::TryCreateFromUserProfileLanguages() {
            let tag = engine.RecognizerLanguage().and_then(|language| language.LanguageTag());
            if let Ok(tag) = tag.map(|tag| LanguageTag(tag.to_string_lossy())) {
                if !candidates.iter().any(|c| c.tag.0.eq_ignore_ascii_case(&tag.0)) {
                    candidates.push(Candidate { engine, tag });
                }
            }
        }
    }
    // None of the user's languages has a recogniser: any installed one reads better than none.
    if candidates.is_empty() {
        for (tag, language) in &installed {
            if let Ok(engine) = Recognizer::TryCreateFromLanguage(language) {
                candidates.push(Candidate { engine, tag: tag.clone() });
                break;
            }
        }
    }
    if candidates.is_empty() {
        return Err(OcrError::LanguageUnavailable(missing_language(&options.languages)));
    }
    Ok(candidates)
}

/// The recognisers installed on this machine with their BCP 47 tags.
fn installed() -> Result<Vec<(LanguageTag, Language)>> {
    // This is the first call that needs the engine's class, so a Windows without it (an N
    // edition without the media features, a stripped-down image) fails here.
    let languages = Recognizer::AvailableRecognizerLanguages().map_err(unavailable)?;
    let mut installed = Vec::new();
    for index in 0..languages.Size().map_err(failed)? {
        let language = languages.GetAt(index).map_err(failed)?;
        let tag = language.LanguageTag().map_err(failed)?.to_string_lossy();
        installed.push((LanguageTag(tag), language));
    }
    Ok(installed)
}

/// The language to name when no recogniser exists at all: the first one asked for, or the first
/// language of the user's profile, which is the one Windows would have read with.
fn missing_language(wanted: &[LanguageTag]) -> LanguageTag {
    if let Some(tag) = wanted.first() {
        return tag.clone();
    }
    let profile = GlobalizationPreferences::Languages().and_then(|languages| languages.GetAt(0));
    LanguageTag(profile.map(|tag| tag.to_string_lossy()).unwrap_or_else(|_| "und".into()))
}

/// Maps the settings' languages ("tr", "en-GB", "zh-Hans") to the installed recognisers
/// ("tr-TR", "en-US", "zh-Hans-CN"), keeping their order and dropping the ones without a
/// recogniser. Answers indices into `installed`.
fn resolve(wanted: &[LanguageTag], installed: &[LanguageTag]) -> Vec<usize> {
    let extends = |long: &str, short: &str| {
        long.len() > short.len()
            && long.as_bytes()[short.len()] == b'-'
            && long.get(..short.len()).is_some_and(|start| start.eq_ignore_ascii_case(short))
    };
    let mut out: Vec<usize> = Vec::new();
    for tag in wanted {
        let exact = installed.iter().position(|i| i.0.eq_ignore_ascii_case(&tag.0));
        let nearest = || installed.iter().position(|i| extends(&i.0, &tag.0) || extends(&tag.0, &i.0));
        let related = || installed.iter().position(|i| i.primary() == tag.primary());
        if let Some(index) = exact.or_else(nearest).or_else(related) {
            if !out.contains(&index) {
                out.push(index);
            }
        }
    }
    out
}

/// Whether `candidate` is the better reading of the same pixels than `best`, which an earlier
/// (more wanted) recogniser made. Windows gives no confidence, so the readings are compared by
/// what they kept. Clearly more letters and digits win, because a recogniser reads little of a
/// script it does not know. With about as many (within 5 %), the candidate wins only when
/// clearly more of its letters lie outside ASCII (3 % of the text): the recognisers of two
/// Latin languages read the same words, and the one that cannot write ç, ğ or ü drops them
/// silently, so the reading with more such letters kept more of what is on the screen.
fn better(candidate: &[Vec<OcrWord>], best: &[Vec<OcrWord>]) -> bool {
    let amount = |lines: &[Vec<OcrWord>]| {
        let characters = lines.iter().flatten().flat_map(|word| word.text.chars());
        let letters = characters.clone().filter(|c| c.is_alphanumeric()).count();
        let beyond_ascii = characters.filter(|c| c.is_alphabetic() && !c.is_ascii()).count();
        (letters, beyond_ascii)
    };
    let ((letters, beyond), (best_letters, best_beyond)) = (amount(candidate), amount(best));
    if letters * 20 > best_letters * 21 {
        return true;
    }
    if letters * 21 < best_letters * 20 {
        return false;
    }
    beyond > best_beyond + (letters.max(best_letters) * 3 / 100).max(1)
}

// ---------------------------------------------------------------------------------- recognition

/// Reads one tile with one recogniser. Answers its lines as words with boxes in pixels of the
/// frame; a line without words is left out.
fn recognize(
    engine: &Recognizer,
    bitmap: &SoftwareBitmap,
    tile: &Tile,
    frame: &Frame,
    cancel: &CancelToken,
) -> Result<Vec<Vec<OcrWord>>> {
    let operation = engine.RecognizeAsync(bitmap).map_err(failed)?;
    let reading: Reading = wait(&operation, cancel)?;

    // No angle (a null reference, which the bindings turn into an error) means the text was not
    // turned, or that Windows could not tell.
    let angle = reading.TextAngle().and_then(|angle| angle.Value()).unwrap_or(0.0);
    // The recogniser saw the padded bitmap, and its angle turns around that bitmap's middle.
    let middle = (tile.x.length().max(MIN_SIDE) as f64 / 2.0, tile.y.length().max(MIN_SIDE) as f64 / 2.0);
    let (frame_width, frame_height) = (frame.size.width as f64, frame.size.height as f64);

    let lines = reading.Lines().map_err(failed)?;
    let mut out = Vec::new();
    for line in 0..lines.Size().map_err(failed)? {
        let words = lines.GetAt(line).map_err(failed)?.Words().map_err(failed)?;
        let mut read = Vec::new();
        for word in 0..words.Size().map_err(failed)? {
            let word = words.GetAt(word).map_err(failed)?;
            let text = word.Text().map_err(failed)?.to_string_lossy();
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let rect = word.BoundingRect().map_err(failed)?;
            let upright = seams::turned(
                Bounds::new(rect.X as f64, rect.Y as f64, rect.Width as f64, rect.Height as f64),
                angle,
                middle,
            );
            // From the tile's pixels to the frame's, and never outside the frame.
            let left = (upright.x + tile.x.start as f64).clamp(0.0, frame_width);
            let top = (upright.y + tile.y.start as f64).clamp(0.0, frame_height);
            let right = (upright.right() + tile.x.start as f64).clamp(0.0, frame_width);
            let bottom = (upright.bottom() + tile.y.start as f64).clamp(0.0, frame_height);
            let bounds = Bounds::new(left, top, right - left, bottom - top);
            read.push(OcrWord { text: text.to_string(), bounds, confidence: CONFIDENCE });
        }
        if !read.is_empty() {
            out.push(read);
        }
    }
    Ok(out)
}

/// Waits on the calling thread for a WinRT operation, and cancels it when the token is
/// cancelled. The completion handler runs on a thread of Windows' pool and only sends a note
/// through the channel, so nothing of this thread is touched from there.
fn wait<T: RuntimeType + 'static>(operation: &IAsyncOperation<T>, cancel: &CancelToken) -> Result<T> {
    if operation.Status().map_err(failed)? == AsyncStatus::Started {
        let (done, finished) = mpsc::channel::<()>();
        let handler = AsyncOperationCompletedHandler::new(move |_, _| {
            // The receiver is gone when the wait was cancelled; then nobody needs the note.
            let _ = done.send(());
            Ok(())
        });
        // An operation that finished in the meantime calls the handler at once.
        operation.SetCompleted(&handler).map_err(failed)?;
        loop {
            match finished.recv_timeout(POLL) {
                // A closed channel means the handler was dropped; the status below tells why.
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) if cancel.is_cancelled() => {
                    // The operation may be past the point where it can stop; either way its
                    // result is not wanted any more, so the answer does not matter.
                    let _ = operation.Cancel();
                    return Err(OcrError::Cancelled);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
    match operation.Status().map_err(failed)? {
        AsyncStatus::Completed => operation.GetResults().map_err(failed),
        AsyncStatus::Canceled => Err(OcrError::Cancelled),
        AsyncStatus::Error => {
            let code = operation.ErrorCode().map_err(failed)?;
            Err(failed(::windows::core::Error::from_hresult(code)))
        }
        _ => Err(OcrError::Engine("Windows ended the recognition without a result".into())),
    }
}

/// Copies one tile of the frame into a bitmap for the recogniser: BGRA, as Microsoft's own
/// sample hands it over, with every pixel opaque and transparent pixels laid on white (so the
/// alpha mode the bitmap states is true whichever one the engine reads it as), and padded with
/// white up to [`MIN_SIDE`]. The pixels go straight from the frame into the bitmap's own
/// memory; no copy is made in between.
fn bitmap_of(frame: &Frame, tile: &Tile) -> Result<SoftwareBitmap> {
    let (columns, rows) = (tile.x.length(), tile.y.length());
    let (width, height) = (columns.max(MIN_SIDE), rows.max(MIN_SIDE));
    let (Ok(pixel_width), Ok(pixel_height)) = (i32::try_from(width), i32::try_from(height)) else {
        return Err(OcrError::InvalidImage("the image is too large for a bitmap".into()));
    };
    let bitmap = SoftwareBitmap::CreateWithAlpha(
        BitmapPixelFormat::Bgra8,
        pixel_width,
        pixel_height,
        BitmapAlphaMode::Premultiplied,
    )
    .map_err(failed)?;

    let buffer = bitmap.LockBuffer(BitmapBufferAccessMode::Write).map_err(failed)?;
    let plane = buffer.GetPlaneDescription(0).map_err(failed)?;
    let reference = buffer.CreateReference().map_err(failed)?;
    let access: IMemoryBufferByteAccess = reference.cast().map_err(failed)?;
    let (mut data, mut capacity) = (std::ptr::null_mut::<u8>(), 0u32);
    // SAFETY: `GetBuffer` writes a pointer and a length through the two pointers it is given,
    // which point to live local variables of exactly those types.
    unsafe { access.GetBuffer(&mut data, &mut capacity) }.map_err(failed)?;

    // The layout Windows chose for the plane is checked before a byte is written, so a stride
    // or a start that is not what this code expects becomes an error and not a stray write.
    let (Ok(start), Ok(stride)) = (usize::try_from(plane.StartIndex), usize::try_from(plane.Stride)) else {
        return Err(OcrError::Engine("Windows laid the bitmap out in an unexpected way".into()));
    };
    let fits = plane.Width == pixel_width
        && plane.Height == pixel_height
        && stride >= width * 4
        && start + stride * (height - 1) + width * 4 <= capacity as usize;
    if data.is_null() || !fits {
        return Err(OcrError::Engine("Windows laid the bitmap out in an unexpected way".into()));
    }
    // SAFETY: `GetBuffer` answered `capacity` bytes at `data`, the memory of the bitmap's locked
    // buffer. It stays valid while `reference` and `buffer` are open, which they are until after
    // the last use of this slice, and nothing else reads or writes it meanwhile: the bitmap is
    // new, known to this function alone, and locked for writing.
    let pixels = unsafe { std::slice::from_raw_parts_mut(data, capacity as usize) };
    for row in 0..height {
        let target = &mut pixels[start + row * stride..][..width * 4];
        if row >= rows {
            target.fill(255);
            continue;
        }
        let from = (tile.y.start + row) * frame.bytes_per_row + tile.x.start * 4;
        let source = &frame.data[from..from + columns * 4];
        let (image, padding) = target.split_at_mut(columns * 4);
        for (pixel, out) in source.chunks_exact(4).zip(image.chunks_exact_mut(4)) {
            let (blue, green, red, alpha) = match frame.format {
                PixelFormat::Bgra8 => (pixel[0], pixel[1], pixel[2], pixel[3]),
                PixelFormat::Rgba8 => (pixel[2], pixel[1], pixel[0], pixel[3]),
            };
            if alpha == 255 {
                out.copy_from_slice(&[blue, green, red, 255]);
            } else {
                // A frame's alpha is straight; the pixel is laid on white, as `load_image` does.
                let over = |c: u8| ((c as u32 * alpha as u32 + 255 * (255 - alpha as u32) + 127) / 255) as u8;
                out.copy_from_slice(&[over(blue), over(green), over(red), 255]);
            }
        }
        padding.fill(255);
    }
    // The bitmap stays locked until both are closed, and a locked bitmap cannot be read.
    drop(access);
    reference.Close().map_err(failed)?;
    buffer.Close().map_err(failed)?;
    Ok(bitmap)
}

// ------------------------------------------------------------------------------------- plumbing

/// Makes sure the process has a multithreaded apartment, so WinRT works on a worker thread that
/// never chose an apartment (see the module's notes on threads).
fn apartment() -> Result<()> {
    static KEPT: AtomicBool = AtomicBool::new(false);
    if KEPT.load(Ordering::Acquire) {
        return Ok(());
    }
    // SAFETY: the function has no preconditions. The cookie it answers is never given back on
    // purpose: the apartment has to live as long as the process, and two threads that both get
    // here only count it up twice.
    unsafe { CoIncrementMTAUsage() }.map_err(unavailable)?;
    KEPT.store(true, Ordering::Release);
    Ok(())
}

/// The error for a Windows on which the engine cannot be reached at all.
fn unavailable(_: ::windows::core::Error) -> OcrError {
    OcrError::EngineUnavailable(EngineId::WindowsMediaOcr)
}

/// The error for a call that failed, with Windows' own words and the HRESULT.
fn failed(error: ::windows::core::Error) -> OcrError {
    OcrError::Engine(format!("{} (0x{:08X})", error.message(), error.code().0))
}

// ---------------------------------------------------------------------------------------- seams

/// Tiles and what happens where two of them meet. Everything here is plain geometry over boxes
/// in pixels of the frame.
mod seams {
    use crate::{Bounds, OcrWord, RawLine};

    /// A box nearer than this to the edge of a tile may be cut off there.
    const EDGE: f64 = 2.0;

    /// Two readings of one word place its middle a few pixels apart. A word whose middle is
    /// this near a seam is kept by both tiles, and the double is taken out afterwards, which is
    /// safer than a word neither tile keeps.
    const BAND: f64 = 4.0;

    /// A stretch of the frame along one axis: from `start` up to `end`, which is not part of it.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Span {
        pub start: usize,
        pub end: usize,
    }

    impl Span {
        pub fn length(&self) -> usize {
            self.end - self.start
        }
    }

    /// One part of the frame that is read on its own, with the neighbours it overlaps.
    #[derive(Clone, Copy, Debug)]
    pub struct Tile {
        pub x: Span,
        pub y: Span,
        left: Option<Span>,
        right: Option<Span>,
        above: Option<Span>,
        below: Option<Span>,
    }

    /// The words of one line as one tile read it, with boxes in pixels of the frame.
    pub struct Piece {
        tile: usize,
        words: Vec<OcrWord>,
        /// Whether a neighbouring tile may hold the same words or the rest of the line.
        shared: bool,
    }

    /// The stretches that cover `length` pixels with at most `limit` each: one when it fits,
    /// otherwise as few as give every pair of neighbours `overlap` shared pixels, spread evenly.
    fn spans(length: usize, limit: usize, overlap: usize) -> Vec<Span> {
        if length <= limit {
            return vec![Span { start: 0, end: length }];
        }
        let overlap = overlap.min(limit / 4);
        let count = (length - overlap).div_ceil(limit - overlap);
        (0..count)
            .map(|index| {
                let start = index * (length - limit) / (count - 1);
                Span { start, end: start + limit }
            })
            .collect()
    }

    /// The tiles of a frame, row by row from the top-left one.
    pub fn tiles(width: usize, height: usize, limit: usize, overlap: usize) -> Vec<Tile> {
        let (columns, rows) = (spans(width, limit, overlap), spans(height, limit, overlap));
        let mut tiles = Vec::with_capacity(columns.len() * rows.len());
        for (row, y) in rows.iter().enumerate() {
            for (column, x) in columns.iter().enumerate() {
                tiles.push(Tile {
                    x: *x,
                    y: *y,
                    left: column.checked_sub(1).map(|c| columns[c]),
                    right: columns.get(column + 1).copied(),
                    above: row.checked_sub(1).map(|r| rows[r]),
                    below: rows.get(row + 1).copied(),
                });
            }
        }
        tiles
    }

    /// Whether a box from `low` to `high` along one axis is this tile's to report. Where the
    /// tile overlaps a neighbour, both read the box. It belongs to the tile that sees it whole;
    /// when both do, to the one whose side of the seam (the middle of the overlap) its middle
    /// lies on; and when neither does (a word wider or a line taller than the overlap), both
    /// keep their part, because half a word is better than none.
    fn owns_along(low: f64, high: f64, own: Span, before: Option<Span>, after: Option<Span>) -> bool {
        let middle = (low + high) / 2.0;
        let (start, end) = (own.start as f64, own.end as f64);
        if let Some(after) = after {
            let whole_here = high < end - EDGE;
            let whole_there = low > after.start as f64 + EDGE;
            let mine = middle < (after.start as f64 + end) / 2.0 + BAND;
            if whole_there && !(whole_here && mine) {
                return false;
            }
        }
        if let Some(before) = before {
            let whole_here = low > start + EDGE;
            let whole_there = high < before.end as f64 - EDGE;
            let mine = middle >= (start + before.end as f64) / 2.0 - BAND;
            if whole_there && !(whole_here && mine) {
                return false;
            }
        }
        true
    }

    impl Tile {
        /// Whether a word is this tile's to report, by where it lies from left to right.
        fn owns_word(&self, bounds: &Bounds) -> bool {
            owns_along(bounds.x, bounds.right(), self.x, self.left, self.right)
        }

        /// Whether a line is this tile's to report, by where it lies from top to bottom. The
        /// whole line is asked and not its words, so a line on a seam stays in one piece.
        fn owns_line(&self, bounds: &Bounds) -> bool {
            owns_along(bounds.y, bounds.bottom(), self.y, self.above, self.below)
        }

        /// Whether a box reaches into the part a neighbour reads as well.
        fn shares(&self, bounds: &Bounds) -> bool {
            self.left.is_some_and(|left| bounds.x < left.end as f64)
                || self.right.is_some_and(|right| bounds.right() > right.start as f64)
                || self.above.is_some_and(|above| bounds.y < above.end as f64)
                || self.below.is_some_and(|below| bounds.bottom() > below.start as f64)
        }
    }

    /// The lines of one tile, reduced to the words that are this tile's to report.
    pub fn owned(lines: Vec<Vec<OcrWord>>, index: usize, tile: &Tile) -> Vec<Piece> {
        lines
            .into_iter()
            .filter_map(|line| {
                let whole = line.iter().map(|word| word.bounds).reduce(|all, word| all.union(&word))?;
                if !tile.owns_line(&whole) {
                    return None;
                }
                let words: Vec<OcrWord> = line.into_iter().filter(|word| tile.owns_word(&word.bounds)).collect();
                let shared = words.iter().any(|word| tile.shares(&word.bounds));
                (!words.is_empty()).then_some(Piece { tile: index, words, shared })
            })
            .collect()
    }

    /// The engine's lines from the pieces of every tile. With several tiles, a word two tiles
    /// both kept is taken out of the later one, and a piece that continues a piece of another
    /// tile on the same row becomes one line with it.
    pub fn lines(mut pieces: Vec<Piece>, several: bool, confidence: f64) -> Vec<RawLine> {
        if several {
            without_doubles(&mut pieces);
            pieces = rejoined(pieces);
        }
        pieces
            .into_iter()
            .filter(|piece| !piece.words.is_empty())
            .map(|piece| {
                let words = unspaced(piece.words);
                let bounds = words.iter().skip(1).fold(words[0].bounds, |all, word| all.union(&word.bounds));
                let text = words.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" ");
                RawLine { text, bounds, confidence, words }
            })
            .collect()
    }

    /// Takes a word out of a piece when a piece of another tile, earlier in the list, holds a
    /// word in the same place: more than half of the smaller box is covered by the other.
    fn without_doubles(pieces: &mut [Piece]) {
        let same_place = |a: &Bounds, b: &Bounds| {
            let width = a.right().min(b.right()) - a.x.max(b.x);
            let height = a.bottom().min(b.bottom()) - a.y.max(b.y);
            let smaller = (a.width * a.height).min(b.width * b.height);
            width > 0.0 && height > 0.0 && width * height > 0.5 * smaller
        };
        for later in 1..pieces.len() {
            if !pieces[later].shared {
                continue;
            }
            let (earlier, rest) = pieces.split_at_mut(later);
            let piece = &mut rest[0];
            piece.words.retain(|word| {
                !earlier
                    .iter()
                    .filter(|other| other.shared && other.tile != piece.tile)
                    .any(|other| other.words.iter().any(|kept| same_place(&kept.bounds, &word.bounds)))
            });
        }
    }

    /// Joins a piece to the piece of another tile that ends just left of it on the same row.
    /// The tiles are read from left to right, so the left half of a line always comes first.
    fn rejoined(pieces: Vec<Piece>) -> Vec<Piece> {
        let extent =
            |piece: &Piece| piece.words.iter().skip(1).fold(piece.words[0].bounds, |all, word| all.union(&word.bounds));
        let mut out: Vec<(Piece, Bounds)> = Vec::new();
        for piece in pieces {
            if piece.words.is_empty() {
                continue;
            }
            let bounds = extent(&piece);
            let mut nearest: Option<(usize, f64)> = None;
            if piece.shared {
                for (index, (other, kept)) in out.iter().enumerate() {
                    if !other.shared || other.tile == piece.tile {
                        continue;
                    }
                    let height = kept.height.min(bounds.height).max(1.0);
                    let shared_height = kept.bottom().min(bounds.bottom()) - kept.y.max(bounds.y);
                    let gap = bounds.x - kept.right();
                    // The same row, and no further apart than the widest space between words.
                    let continues = shared_height >= 0.5 * height && gap >= -0.5 * height && gap <= height;
                    if continues && nearest.is_none_or(|(_, best)| gap.abs() < best) {
                        nearest = Some((index, gap.abs()));
                    }
                }
            }
            match nearest {
                Some((index, _)) => {
                    let (other, kept) = &mut out[index];
                    *kept = kept.union(&bounds);
                    other.words.extend(piece.words);
                }
                None => out.push((piece, bounds)),
            }
        }
        out.into_iter().map(|(piece, _)| piece).collect()
    }

    /// Windows reads Chinese and Japanese one character at a time and calls each a word, which
    /// would put a space between every two of them. Neighbouring "words" of those scripts that
    /// touch are one word here, as the other engines give them.
    fn unspaced(words: Vec<OcrWord>) -> Vec<OcrWord> {
        let without_spaces = |c: Option<char>| {
            matches!(
                c,
                Some(
                    '\u{3000}'..='\u{30ff}'   // CJK punctuation, hiragana, katakana
                    | '\u{3400}'..='\u{4dbf}' // CJK ideographs, extension A
                    | '\u{4e00}'..='\u{9fff}' // CJK ideographs
                    | '\u{f900}'..='\u{faff}' // CJK compatibility ideographs
                    | '\u{ff00}'..='\u{ffef}' // full-width forms
                )
            )
        };
        let mut out: Vec<OcrWord> = Vec::with_capacity(words.len());
        for word in words {
            match out.last_mut() {
                Some(last)
                    if without_spaces(last.text.chars().next_back())
                        && without_spaces(word.text.chars().next())
                        && word.bounds.x - last.bounds.right() <= 0.5 * last.bounds.height.max(word.bounds.height) =>
                {
                    last.text.push_str(&word.text);
                    last.bounds = last.bounds.union(&word.bounds);
                }
                _ => out.push(word),
            }
        }
        out
    }

    /// The upright box around a box the recogniser reported in an image it had turned upright:
    /// the box turned back clockwise by `degrees` around `middle`, which is how Windows says its
    /// results lie on the image it was given.
    pub fn turned(bounds: Bounds, degrees: f64, middle: (f64, f64)) -> Bounds {
        if !degrees.is_finite() || degrees.abs() < 0.01 {
            return bounds;
        }
        let (sin, cos) = degrees.to_radians().sin_cos();
        let corners = [
            (bounds.x, bounds.y),
            (bounds.right(), bounds.y),
            (bounds.right(), bounds.bottom()),
            (bounds.x, bounds.bottom()),
        ]
        .map(|(x, y)| {
            let (dx, dy) = (x - middle.0, y - middle.1);
            // The y axis points down, so this turns clockwise on the screen.
            (middle.0 + dx * cos - dy * sin, middle.1 + dx * sin + dy * cos)
        });
        let (mut left, mut top, mut right, mut bottom) = (corners[0].0, corners[0].1, corners[0].0, corners[0].1);
        for (x, y) in corners {
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
        }
        Bounds::new(left, top, right - left, bottom - top)
    }
}
