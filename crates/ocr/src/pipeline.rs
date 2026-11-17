//! The recognition pipeline every engine shares: prepare the image, let the engine read, map the
//! boxes back to the original image, put the text into reading order and name its language.

use std::borrow::Cow;

use glimpsfy_capture::Frame;

use crate::layout::{self, Layout, LayoutOptions};
use crate::{
    prepare, BlockKind, CancelToken, CodeResult, OcrEngine, OcrRequest, OcrResult, RawText, ReadOptions, Result,
};

/// Code makes up at least this share of the characters before the image is read again without
/// language correction.
const CODE_SHARE: f64 = 0.4;

/// Reads the text of the request's image with `engine`. Blocks; see the crate's notes on threads.
pub fn recognize(engine: &dyn OcrEngine, request: &OcrRequest) -> Result<OcrResult> {
    let cancel = &request.cancel;
    let image: &Frame = &request.image;
    let preparation = &request.prepare;
    let mut options = ReadOptions {
        languages: request.languages.clone(),
        detect_language: request.detect_language,
        language_correction: preparation.language_correction,
    };
    let layout_options = LayoutOptions { keep_line_breaks: request.keep_line_breaks };

    let dark = preparation.invert_dark && prepare::light_on_dark(image);
    let base: Cow<'_, Frame> = if dark { Cow::Owned(prepare::inverted(image)) } else { Cow::Borrowed(image) };
    cancel.check()?;
    let mut raw = engine.read(&base, &options, cancel)?;

    // Small text is read again from an enlarged copy, and the better reading wins.
    let mut enlarged: Option<(Frame, u32)> = None;
    if preparation.upscale_small_text {
        if let Some(factor) = prepare::upscale_factor(&raw, &base, preparation) {
            cancel.check()?;
            let large = prepare::upscaled(&base, factor);
            let again = read_scaled(engine, &large, factor, &options, cancel)?;
            if prepare::better(&again, &raw) {
                raw = again;
                enlarged = Some((large, factor));
            }
        }
    }
    cancel.check()?;
    let mut layout = layout::arrange(&raw, &layout_options);

    // Code reads better without the language model, so mostly-code images are read once more.
    if options.language_correction && preparation.code_without_correction && code_share(&layout) >= CODE_SHARE {
        options.language_correction = false;
        let again = match &enlarged {
            Some((large, factor)) => read_scaled(engine, large, *factor, &options, cancel)?,
            None => engine.read(&base, &options, cancel)?,
        };
        cancel.check()?;
        layout = layout::arrange(&again, &layout_options);
        raw = again;
    }

    let language = raw.language.clone().or_else(|| engine.dominant_language(&layout.text, &request.languages));
    Ok(OcrResult { engine: engine.id(), text: layout.text, lines: layout.lines, language, image_size: image.size })
}

/// Finds QR codes and barcodes in `image` with `engine`. Blocks.
pub fn detect_codes(engine: &dyn OcrEngine, image: &Frame, cancel: &CancelToken) -> Result<Vec<CodeResult>> {
    engine.detect_codes(image, cancel)
}

/// Reads an image enlarged by `factor` and maps its boxes back to the original's pixels.
fn read_scaled(
    engine: &dyn OcrEngine,
    image: &Frame,
    factor: u32,
    options: &ReadOptions,
    cancel: &CancelToken,
) -> Result<RawText> {
    let mut raw = engine.read(image, options, cancel)?;
    let factor = factor as f64;
    for line in &mut raw.lines {
        line.bounds = line.bounds.scaled_down(factor);
        for word in &mut line.words {
            word.bounds = word.bounds.scaled_down(factor);
        }
    }
    Ok(raw)
}

/// The share of the recognised characters that sit in code blocks.
fn code_share(layout: &Layout) -> f64 {
    let (code, all) = layout.lines.iter().fold((0usize, 0usize), |(code, all), line| {
        let n = line.text.chars().filter(|c| !c.is_whitespace()).count();
        (code + if line.kind == BlockKind::Code { n } else { 0 }, all + n)
    });
    if all == 0 {
        0.0
    } else {
        code as f64 / all as f64
    }
}
