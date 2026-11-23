//! QR codes and barcodes where Vision does not exist: the code detection of the Windows engine
//! and of PP-OCR, done with `rxing`, a Rust port of ZXing. Nothing here touches the network.
//!
//! The frame is turned into one grey image, which is made black and white once and searched in
//! passes, one per family of symbologies, because each family is found in its own way:
//!
//! 1. QR, Micro QR: one pass finds every code by its finder patterns, whatever its size.
//! 2. Barcodes (EAN, UPC-E, Code 39, 93 and 128, ITF, Codabar): every few rows and columns are
//!    read in both directions, so several barcodes and upright ones are found. A barcode counts
//!    when two lines read the same text and its bars are clearly longer than they are wide,
//!    which keeps a line of text, or a row of modules inside a QR code, from becoming one.
//! 3. PDF417 and Data Matrix: their detectors look at the whole image and report every symbol.
//! 4. Aztec: its detector expects the bull's eye in the middle of the image it is given, so the
//!    bull's eyes are looked for first and the detector then reads a square around each.
//!
//! All four run twice, the second time with black and white swapped, because a code drawn light
//! on dark (a dark mode page) is not a code to the readers until it is inverted. Every box
//! leaves this file in pixels of the frame with the origin at the top-left corner, as the Vision
//! engine's boxes do, and the symbologies map onto [`Symbology`] the way Vision's names do.
//!
//! A full-HD frame takes tens of milliseconds and a 5K frame about a quarter of a second in an
//! optimised build; `examples/codes.rs` measures it.

use std::collections::HashSet;

use glimpsfy_capture::{Frame, PixelFormat};
use rxing::aztec::AztecReader;
use rxing::common::{BitArray, BitMatrix, DetectorRXingResult, HybridBinarizer};
use rxing::datamatrix::decoder::Decoder as DataMatrixDecoder;
use rxing::datamatrix::detector::zxing_cpp_detector;
use rxing::multi::MultipleBarcodeReader;
use rxing::oned::{MultiFormatOneDReader, OneDReader};
use rxing::pdf417::PDF417Reader;
use rxing::qrcode::cpp_port::QrReader;
use rxing::{BarcodeFormat, BinaryBitmap, DecodeHints, Luma8Source, LuminanceSource, Point, Reader};

use crate::{codes, Bounds, CancelToken, CodeResult, Result, Symbology};

/// An image smaller than this on either side cannot hold a code: the smallest symbol with its
/// quiet zone is wider.
const MIN_SIDE: usize = 16;

/// The barcodes of the second pass, which are the one-dimensional symbologies [`Symbology`]
/// names. UPC-A is left out on purpose: it is EAN-13 with a leading zero, and Vision reports it
/// that way, so the EAN-13 reader does too when it may not answer UPC-A.
const BARCODES: [BarcodeFormat; 8] = [
    BarcodeFormat::EAN_8,
    BarcodeFormat::EAN_13,
    BarcodeFormat::UPC_E,
    BarcodeFormat::CODE_39,
    BarcodeFormat::CODE_93,
    BarcodeFormat::CODE_128,
    BarcodeFormat::ITF,
    BarcodeFormat::CODABAR,
];

/// Every how many rows a barcode is looked for. A barcode on a screen is far taller than twice
/// this, so at least two of its rows are read.
const ROW_STEP: usize = 4;

/// Every how many columns an upright barcode is looked for. Upright barcodes are rare and their
/// bars are long, so the columns are read more sparsely than the rows.
const COLUMN_STEP: usize = 8;

/// How many barcodes one line may hold; the search of a line stops after this many.
const BARCODES_PER_LINE: usize = 4;

/// How many lines have to read the same barcode before it counts.
const LINES_TO_COUNT: usize = 2;

/// How long the bars of a barcode have to be, in modules (the width of its narrowest bar). The
/// short symbologies have weak checksums, and a row of modules in a QR code or in any blocky
/// pattern can pass one; such a row is one module tall, and a barcode's bars never are.
const BAR_LENGTH: usize = 4;

/// How alike a line has to be to the line a barcode was read on to count as part of the same
/// barcode, as a share of its pixels.
const SAME_BARS: f64 = 0.9;

/// Every how many rows a bull's eye is looked for. Its middle module is at least two pixels
/// tall in a code that can be read at all, so every second row meets it.
const BULLSEYE_STEP: usize = 2;

/// The most bull's eyes one pass follows up. A real screen has a handful of look-alikes at
/// most; a checkerboard has thousands, and each costs a reading attempt.
const MAX_BULLSEYES: usize = 32;

/// Half the width of the largest Aztec code in modules (151 across), with its quiet zone.
const AZTEC_REACH: usize = 80;

/// Finds the QR codes and barcodes of `image`. Blocks for the time the passes take, and returns
/// [`crate::OcrError::Cancelled`] between two passes once `cancel` is cancelled.
pub fn detect(image: &Frame, cancel: &CancelToken) -> Result<Vec<CodeResult>> {
    cancel.check()?;
    let grey = Grey::of(image);
    if grey.width < MIN_SIDE || grey.height < MIN_SIDE {
        return Ok(Vec::new());
    }
    let (Ok(width), Ok(height)) = (u32::try_from(grey.width), u32::try_from(grey.height)) else {
        return Ok(Vec::new());
    };
    let Ok(source) = Luma8Source::new_with_slice(&grey.pixels, width, height) else {
        return Ok(Vec::new());
    };
    // The bitmap keeps its black-and-white matrix after the first reader asked for it, so the
    // passes share one binarisation, and inverting the bitmap flips that matrix in place.
    let mut bitmap = BinaryBitmap::new(HybridBinarizer::new(source.clone()));
    let hints = DecodeHints { TryHarder: Some(true), ..DecodeHints::default() };

    let mut found = Found { codes: Vec::new(), grey: &grey };
    for inverted in [false, true] {
        if inverted {
            bitmap.invert();
        }
        cancel.check()?;
        // A reader that finds nothing answers with an error, which is not a failure here.
        for code in QrReader.decode_multiple_with_hints(&mut bitmap, &hints).unwrap_or_default() {
            found.add(code.getBarcodeFormat(), code.getText(), found.around(code.getPoints(), 0.0, 0.0));
        }
        cancel.check()?;
        found.barcodes(bitmap.get_black_matrix(), false, inverted);
        cancel.check()?;
        found.barcodes(bitmap.get_black_matrix(), true, inverted);
        cancel.check()?;
        for code in PDF417Reader::new().decode_multiple_with_hints(&mut bitmap, &hints).unwrap_or_default() {
            found.add(code.getBarcodeFormat(), code.getText(), found.around(code.getPoints(), 0.0, 0.0));
        }
        cancel.check()?;
        found.data_matrices(bitmap.get_black_matrix());
        cancel.check()?;
        found.aztecs(bitmap.get_black_matrix(), &source, inverted, &hints);
    }
    cancel.check()?;
    Ok(found.codes)
}

/// The codes found so far, with the grey image their boxes are measured in.
struct Found<'a> {
    codes: Vec<CodeResult>,
    grey: &'a Grey,
}

impl Found<'_> {
    /// Adds a code unless its payload is empty or the same code is already there, which happens
    /// when both the plain and the inverted pass read it.
    fn add(&mut self, format: &BarcodeFormat, payload: &str, bounds: Bounds) {
        let symbology = symbology_of(format);
        let known = self
            .codes
            .iter()
            .any(|code| code.symbology == symbology && code.payload == payload && intersect(&code.bounds, &bounds));
        if !payload.is_empty() && !known {
            self.codes.push(CodeResult {
                symbology,
                payload: payload.to_string(),
                bounds,
                url: codes::link_for(payload),
            });
        }
    }

    /// The box around a symbol's corners, moved by the offset of the crop they were found in.
    fn around(&self, points: &[Point], left: f64, top: f64) -> Bounds {
        let (width, height) = (self.grey.width as f64, self.grey.height as f64);
        let mut corners = points
            .iter()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .map(|p| ((p.x as f64 + left).clamp(0.0, width), (p.y as f64 + top).clamp(0.0, height)));
        let Some((x, y)) = corners.next() else {
            // A reader that cannot place its code still read it somewhere in the image.
            return Bounds::new(0.0, 0.0, width, height);
        };
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x, y);
        for (x, y) in corners {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        Bounds::new(x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0))
    }

    /// Reads barcodes on every few rows of the matrix, or on every few columns when `upright`.
    /// A reader finds one barcode in a line, so what it found is blanked and the line is read
    /// again for the next one; the mirrored line gives the barcodes that stand on their head.
    fn barcodes(&mut self, matrix: &BitMatrix, upright: bool, inverted: bool) {
        let hints = DecodeHints { PossibleFormats: Some(HashSet::from(BARCODES)), ..DecodeHints::default() };
        let mut reader = MultiFormatOneDReader::new(&hints);
        let (lines, length) =
            if upright { (self.grey.width, self.grey.height) } else { (self.grey.height, self.grey.width) };
        let step = if upright { COLUMN_STEP } else { ROW_STEP };
        let mut seen: Vec<Seen> = Vec::new();
        for at in (0..lines).step_by(step) {
            let line = Line { at, upright };
            let mut bits = if upright { matrix.getCol(at as u32) } else { matrix.getRow(at as u32) };
            for mirrored in [false, true] {
                if mirrored {
                    bits.reverse();
                }
                for _ in 0..BARCODES_PER_LINE {
                    let Ok(read) = reader.decode_row(at as u32, &bits, &hints) else {
                        break;
                    };
                    // The two points are the middles of the first and the last pattern.
                    let ends: Vec<usize> = read
                        .getPoints()
                        .iter()
                        .filter(|p| p.x.is_finite())
                        .map(|p| (p.x.max(0.0) as usize).min(length - 1))
                        .map(|x| if mirrored { length - 1 - x } else { x })
                        .collect();
                    let (Some(&from), Some(&to)) = (ends.iter().min(), ends.iter().max()) else {
                        break;
                    };
                    let (first, last, module) = self.grey.bars(line, from, to, inverted);
                    let symbology = symbology_of(read.getBarcodeFormat());
                    let again = seen
                        .iter_mut()
                        .find(|s| s.symbology == symbology && s.payload == read.getText() && s.holds(at, first, last));
                    match again {
                        Some(seen) if seen.last_line != at => {
                            seen.lines += 1;
                            seen.last_line = at;
                        }
                        Some(_) => {}
                        None => {
                            let (start, end) = self.grey.same_bars(line, first, last);
                            seen.push(Seen {
                                format: *read.getBarcodeFormat(),
                                symbology,
                                payload: read.getText().to_string(),
                                along: (first, last),
                                across: (start, end),
                                module,
                                lines: 1,
                                last_line: at,
                            });
                        }
                    }
                    blank(&mut bits, first, last, mirrored);
                }
            }
        }
        let counts = |s: &Seen| s.lines >= LINES_TO_COUNT && s.across.1 - s.across.0 + 1 >= BAR_LENGTH * s.module;
        for seen in seen.into_iter().filter(counts) {
            let along = (seen.along.0 as f64, (seen.along.1 - seen.along.0 + 1) as f64);
            let across = (seen.across.0 as f64, (seen.across.1 - seen.across.0 + 1) as f64);
            let bounds = if upright {
                Bounds::new(across.0, along.0, across.1, along.1)
            } else {
                Bounds::new(along.0, across.0, along.1, across.1)
            };
            self.add(&seen.format, &seen.payload, bounds);
        }
    }

    /// Reads every Data Matrix symbol the detector finds.
    fn data_matrices(&mut self, matrix: &BitMatrix) {
        let decoder = DataMatrixDecoder::new();
        for symbol in zxing_cpp_detector::detect(matrix, true, true).unwrap_or_default() {
            if let Ok(decoded) = decoder.decode(symbol.getBits()) {
                self.add(&BarcodeFormat::DATA_MATRIX, decoded.getText(), self.around(symbol.getPoints(), 0.0, 0.0));
            }
        }
    }

    /// Reads an Aztec code around every bull's eye in the matrix. Each is read from a square
    /// cut out of the grey image with the bull's eye in its middle, which is where the detector
    /// looks for it.
    fn aztecs(&mut self, matrix: &BitMatrix, source: &Luma8Source, inverted: bool, hints: &DecodeHints) {
        for eye in bullseyes(matrix) {
            let already = self
                .codes
                .iter()
                .any(|code| code.symbology == Symbology::Aztec && inside(&code.bounds, eye.x as f64, eye.y as f64));
            if already {
                continue;
            }
            // The square reaches as far as the largest code would, or as far as the image goes.
            let reach = (AZTEC_REACH * eye.module)
                .min(eye.x)
                .min(eye.y)
                .min(self.grey.width - 1 - eye.x)
                .min(self.grey.height - 1 - eye.y);
            let (left, top, side) = (eye.x - reach, eye.y - reach, 2 * reach + 1);
            if side < MIN_SIDE {
                continue;
            }
            let Ok(square) = source.crop(left, top, side, side) else {
                continue;
            };
            let mut square = BinaryBitmap::new(HybridBinarizer::new(square));
            if inverted {
                square.invert();
            }
            if let Ok(code) = AztecReader.decode_with_hints(&mut square, hints) {
                let bounds = self.around(code.getPoints(), left as f64, top as f64);
                self.add(code.getBarcodeFormat(), code.getText(), bounds);
            }
        }
    }
}

/// A barcode some lines have read, until enough lines agree on it.
struct Seen {
    format: BarcodeFormat,
    symbology: Symbology,
    payload: String,
    /// The first and the last pixel of the bars along the line they were read on.
    along: (usize, usize),
    /// The first and the last line that show these bars.
    across: (usize, usize),
    /// The width of the narrowest bar in pixels.
    module: usize,
    lines: usize,
    last_line: usize,
}

impl Seen {
    /// Whether a reading on line `at` between `first` and `last` is this barcode again.
    fn holds(&self, at: usize, first: usize, last: usize) -> bool {
        at >= self.across.0 && at <= self.across.1 && first <= self.along.1 && last >= self.along.0
    }
}

/// Clears a barcode out of a line, so the next reading of the line finds the one after it.
fn blank(bits: &mut BitArray, first: usize, last: usize, mirrored: bool) {
    let size = bits.get_size();
    let (first, last) =
        if mirrored { (size.saturating_sub(last + 1), size.saturating_sub(first + 1)) } else { (first, last) };
    for at in first..=last.min(size.saturating_sub(1)) {
        bits.unset(at);
    }
}

/// The middle of something that looks like the bull's eye of an Aztec code.
struct Bullseye {
    x: usize,
    y: usize,
    /// The width of one module in pixels.
    module: usize,
}

/// Looks for bull's eyes: places where a row and the column through it both cross seven runs of
/// one width, black in the middle, which is what the rings around the middle of an Aztec code
/// look like from every side. Text and lines do not look like that in both directions.
fn bullseyes(matrix: &BitMatrix) -> Vec<Bullseye> {
    let (width, height) = (matrix.getWidth() as usize, matrix.getHeight() as usize);
    let mut eyes: Vec<Bullseye> = Vec::new();
    let mut runs: Vec<(usize, usize)> = Vec::new(); // (start, length), alternating colours
    for y in (0..height).step_by(BULLSEYE_STEP) {
        let row = matrix.getRow(y as u32);
        runs.clear();
        let first_is_black = row.get(0);
        let (mut x, mut black) = (0, first_is_black);
        while x < width {
            let end = if black { row.getNextUnset(x) } else { row.getNextSet(x) }.min(width);
            runs.push((x, end - x));
            (x, black) = (end, !black);
        }
        for (index, window) in runs.windows(7).enumerate() {
            // The run in the middle of the window has to be black: runs alternate from the first.
            let middle_is_black = first_is_black == (index + 3).is_multiple_of(2);
            let Some(module) = one_width(window.iter().map(|run| run.1)).filter(|_| middle_is_black) else {
                continue;
            };
            let x = window[3].0 + window[3].1 / 2;
            if eyes.iter().any(|eye| eye.x.abs_diff(x) <= eye.module && eye.y.abs_diff(y) <= eye.module) {
                continue;
            }
            if let Some(y) = column_of_rings(matrix, x, y, module) {
                eyes.push(Bullseye { x, y, module });
                if eyes.len() >= MAX_BULLSEYES {
                    return eyes;
                }
            }
        }
    }
    eyes
}

/// The width the runs share when all of them are about as wide as each other and at least two
/// pixels, which is the least a module of a readable code takes.
fn one_width(lengths: impl Iterator<Item = usize> + Clone) -> Option<usize> {
    let (narrowest, widest) = (lengths.clone().min()?, lengths.clone().max()?);
    let count = lengths.clone().count();
    (narrowest >= 2 && widest <= narrowest * 3 / 2 + 1).then(|| lengths.sum::<usize>() / count.max(1))
}

/// Whether the column through (`x`, `y`) crosses the same rings as the row did: the black run at
/// `y` and three runs above and below it, each about one module tall. Answers the middle of the
/// black run, which is the middle of the bull's eye.
fn column_of_rings(matrix: &BitMatrix, x: usize, y: usize, module: usize) -> Option<usize> {
    let height = matrix.getHeight() as usize;
    let black = |y: usize| matrix.get(x as u32, y as u32);
    let fits = |length: usize| length * 2 >= module && length <= module * 3 / 2 + 1;
    if !black(y) {
        return None;
    }
    // The ends of the black run at `y`, then three runs upwards and three downwards.
    let (mut top, mut bottom) = (y, y);
    while top > 0 && black(top - 1) && y - top <= 2 * module {
        top -= 1;
    }
    while bottom + 1 < height && black(bottom + 1) && bottom - y <= 2 * module {
        bottom += 1;
    }
    if !fits(bottom - top + 1) {
        return None;
    }
    let (mut above, mut colour) = (top, true);
    for _ in 0..3 {
        colour = !colour;
        let mut length = 0;
        while above > 0 && black(above - 1) == colour && length <= 2 * module {
            above -= 1;
            length += 1;
        }
        if !fits(length) {
            return None;
        }
    }
    let (mut below, mut colour) = (bottom, true);
    for _ in 0..3 {
        colour = !colour;
        let mut length = 0;
        while below + 1 < height && black(below + 1) == colour && length <= 2 * module {
            below += 1;
            length += 1;
        }
        if !fits(length) {
            return None;
        }
    }
    Some((top + bottom) / 2)
}

/// The kind Vision gives the same symbology (`symbology_of` in `engines/vision.rs`).
fn symbology_of(format: &BarcodeFormat) -> Symbology {
    match format {
        BarcodeFormat::QR_CODE => Symbology::Qr,
        BarcodeFormat::MICRO_QR_CODE => Symbology::MicroQr,
        BarcodeFormat::AZTEC => Symbology::Aztec,
        BarcodeFormat::DATA_MATRIX => Symbology::DataMatrix,
        BarcodeFormat::PDF_417 => Symbology::Pdf417,
        BarcodeFormat::EAN_8 => Symbology::Ean8,
        // Vision has no UPC-A of its own and calls it EAN-13.
        BarcodeFormat::EAN_13 | BarcodeFormat::UPC_A => Symbology::Ean13,
        BarcodeFormat::UPC_E => Symbology::UpcE,
        BarcodeFormat::CODE_39 => Symbology::Code39,
        BarcodeFormat::CODE_93 => Symbology::Code93,
        BarcodeFormat::CODE_128 => Symbology::Code128,
        BarcodeFormat::ITF => Symbology::Itf,
        BarcodeFormat::CODABAR => Symbology::Codabar,
        _ => Symbology::Other,
    }
}

fn intersect(a: &Bounds, b: &Bounds) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

fn inside(bounds: &Bounds, x: f64, y: f64) -> bool {
    x >= bounds.x && x < bounds.right() && y >= bounds.y && y < bounds.bottom()
}

/// The frame as one byte of brightness per pixel, in tight rows.
struct Grey {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
}

/// One row of the grey image, or one column of it when `upright`.
#[derive(Clone, Copy)]
struct Line {
    at: usize,
    upright: bool,
}

impl Grey {
    fn of(frame: &Frame) -> Grey {
        let (width, height) = (frame.size.width as usize, frame.size.height as usize);
        let (red, blue) = match frame.format {
            PixelFormat::Rgba8 => (0, 2),
            PixelFormat::Bgra8 => (2, 0),
        };
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            let start = y * frame.bytes_per_row;
            // A frame is checked when it is made, but its fields are public; a buffer that is
            // too short for its size gives an empty image instead of a panic.
            let Some(row) = frame.data.get(start..start + width * 4) else {
                return Grey { pixels: Vec::new(), width: 0, height: 0 };
            };
            pixels.extend(row.chunks_exact(4).map(|px| {
                let luma = (77 * px[red] as u32 + 150 * px[1] as u32 + 29 * px[blue] as u32) >> 8;
                // A transparent pixel is read as if it lay on white, as `load_image` does.
                let alpha = px[3] as u32;
                ((luma * alpha + 255 * (255 - alpha)) / 255) as u8
            }));
        }
        Grey { pixels, width, height }
    }

    /// The pixel `at` steps along a line.
    fn at(&self, line: Line, at: usize) -> u8 {
        let (x, y) = if line.upright { (line.at, at) } else { (at, line.at) };
        self.pixels[y * self.width + x]
    }

    /// How many pixels a line has, or how many lines there are when `upright` is turned round.
    fn length(&self, upright: bool) -> usize {
        if upright {
            self.height
        } else {
            self.width
        }
    }

    /// The brightness that separates bars from spaces between `from` and `to` on a line.
    fn threshold(&self, line: Line, from: usize, to: usize) -> u16 {
        let (mut darkest, mut lightest) = (255u8, 0u8);
        for at in from..=to {
            let value = self.at(line, at);
            darkest = darkest.min(value);
            lightest = lightest.max(value);
        }
        (darkest as u16 + lightest as u16) / 2
    }

    /// The first and the last pixel of a barcode on the line it was read on, and the width of
    /// its modules. `from` and `to` lie inside its first and last pattern; the barcode ends where
    /// a space is wider than any space inside a symbol, which is its quiet zone. `inverted` says
    /// the bars are the light pixels. This is the box Vision gives a barcode: its bars without
    /// the quiet zones.
    fn bars(&self, line: Line, from: usize, to: usize, inverted: bool) -> (usize, usize, usize) {
        let threshold = self.threshold(line, from, to);
        let bar = |at: usize| (self.at(line, at) as u16 <= threshold) != inverted;
        // The narrowest runs are one module wide; the lower quartile of the run lengths names
        // that width without trusting a single stray pixel.
        let mut runs = Vec::new();
        let mut start = from;
        for at in from + 1..=to {
            if bar(at) != bar(at - 1) {
                runs.push(at - start);
                start = at;
            }
        }
        runs.push(to + 1 - start);
        runs.sort_unstable();
        let module = runs[runs.len() / 4].max(1);
        // No symbology has a space wider than four modules inside a symbol.
        let quiet = module * 9 / 2 + 1;

        let (mut first, mut last) = (from, to);
        let mut space = 0;
        for at in (0..from).rev() {
            if bar(at) {
                first = at;
                space = 0;
            } else {
                space += 1;
                if space > quiet {
                    break;
                }
            }
        }
        space = 0;
        for at in to + 1..self.length(line.upright) {
            if bar(at) {
                last = at;
                space = 0;
            } else {
                space += 1;
                if space > quiet {
                    break;
                }
            }
        }
        (first, last, module)
    }

    /// The first and the last line beside `line` that show the same bars between `from` and
    /// `to`: the extent of a barcode across its bars.
    fn same_bars(&self, line: Line, from: usize, to: usize) -> (usize, usize) {
        let threshold = self.threshold(line, from, to);
        let dark = |line: Line, at: usize| self.at(line, at) as u16 <= threshold;
        let length = to - from + 1;
        let same = |other: usize| {
            let other = Line { at: other, ..line };
            let alike = (from..=to).filter(|at| dark(other, *at) == dark(line, *at)).count();
            alike as f64 >= SAME_BARS * length as f64
        };
        let (mut first, mut last) = (line.at, line.at);
        while first > 0 && same(first - 1) {
            first -= 1;
        }
        while last + 1 < self.length(!line.upright) && same(last + 1) {
            last += 1;
        }
        (first, last)
    }
}
