//! Apple's Vision framework: text with `VNRecognizeTextRequest`, codes with
//! `VNDetectBarcodesRequest`, and the dominant language with `NLLanguageRecognizer`.
//!
//! Vision reports boxes normalised to 0…1 with the origin at the bottom-left corner; every box
//! leaves this file in pixels of the image with the origin at the top-left corner. The requests
//! run synchronously on the calling thread inside an autorelease pool, so the worker thread does
//! not collect autoreleased objects between recognitions. A cancelled [`CancelToken`] cancels the
//! running request from Vision's progress handler and from a small watcher thread, whichever
//! comes first.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use glimpsfy_capture::{Frame, PixelFormat};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{msg_send, AnyThread, ClassType};
use objc2_core_foundation::{CFData, CFRetained, CGRect};
use objc2_core_graphics::{
    kCGColorSpaceSRGB, CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSIndexSet, NSObjectProtocol, NSRange, NSString};
use objc2_natural_language::NLLanguageRecognizer;
use objc2_vision::{
    VNBarcodeObservation, VNDetectBarcodesRequest, VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedText,
    VNRectangleObservation, VNRequest, VNRequestProgressProviding, VNRequestTextRecognitionLevel,
};

use crate::{
    codes, Bounds, CancelToken, CodeResult, EngineId, LanguageInfo, LanguageTag, OcrEngine, OcrError, OcrWord, RawLine,
    RawText, ReadOptions, Result, Symbology,
};

/// Reads text with Apple's Vision framework at the accurate level.
#[derive(Clone, Copy, Debug, Default)]
pub struct VisionEngine;

impl OcrEngine for VisionEngine {
    fn id(&self) -> EngineId {
        EngineId::Vision
    }

    fn read(&self, image: &Frame, options: &ReadOptions, cancel: &CancelToken) -> Result<RawText> {
        cancel.check()?;
        autoreleasepool(|_| read_text(image, options, cancel))
    }

    fn supported_languages(&self) -> Result<Vec<LanguageInfo>> {
        autoreleasepool(|_| {
            let tags = supported_tags()?;
            Ok(tags.into_iter().map(|tag| LanguageInfo { tag, available: true, download_bytes: None }).collect())
        })
    }

    fn detect_codes(&self, image: &Frame, cancel: &CancelToken) -> Result<Vec<CodeResult>> {
        cancel.check()?;
        autoreleasepool(|_| read_codes(image, cancel))
    }

    fn dominant_language(&self, text: &str, candidates: &[LanguageTag]) -> Option<LanguageTag> {
        if text.trim().is_empty() {
            return None;
        }
        autoreleasepool(|_| {
            // SAFETY: a class method that only reads the string it is given.
            let language = unsafe { NLLanguageRecognizer::dominantLanguageForString(&NSString::from_str(text)) }?;
            let code = language.to_string();
            if code == "und" {
                return None;
            }
            // Prefer the regional tag the user asked for ("tr" → "tr-TR") when it matches.
            let matched = candidates.iter().find(|c| c.primary() == code.to_ascii_lowercase());
            Some(matched.cloned().unwrap_or(LanguageTag(code)))
        })
    }
}

// --------------------------------------------------------------------------------------- text

fn read_text(image: &Frame, options: &ReadOptions, cancel: &CancelToken) -> Result<RawText> {
    let (width, height) = (image.size.width as f64, image.size.height as f64);
    if width < 1.0 || height < 1.0 {
        return Ok(RawText::default());
    }
    let cg_image = cg_image(image)?;
    let request = text_request(options)?;

    // Vision calls the progress handler from its own queue while the request runs; a cancelled
    // token cancels the request there, and the watcher in `perform` covers the stretches without
    // progress reports.
    let token = cancel.clone();
    let progress = RcBlock::new(move |request: NonNull<VNRequest>, _done: f64, _error: *mut NSError| {
        if token.is_cancelled() {
            // SAFETY: Vision passes the running request, which is alive for the call.
            unsafe { request.as_ref().cancel() };
        }
    });
    // SAFETY: Vision copies the block, so it lives as long as the request needs it.
    unsafe { request.setProgressHandler(RcBlock::as_ptr(&progress)) };
    perform(&cg_image, &request, cancel)?;

    let observations = request.results().map(|r| r.to_vec()).unwrap_or_default();
    let mut lines = Vec::with_capacity(observations.len());
    for observation in observations {
        let Some(top) = observation.topCandidates(1).firstObject() else {
            continue;
        };
        let text = top.string().to_string();
        if text.trim().is_empty() {
            continue;
        }
        // SAFETY: a plain property read on a finished observation.
        let bounds = to_pixels(unsafe { observation.boundingBox() }, width, height);
        let confidence = top.confidence() as f64;
        let words = words_of(&top, &text, width, height, confidence);
        lines.push(RawLine { text, bounds, confidence, words });
    }
    cancel.check()?;
    Ok(RawText { lines, language: None })
}

fn text_request(options: &ReadOptions) -> Result<Retained<VNRecognizeTextRequest>> {
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(options.language_correction);
    // The newest revision the running system has: macOS 13 brought revision 3, which reads more
    // languages and detects the language; later systems may add more.
    if let Some(revision) = newest_revision(VNRecognizeTextRequest::class()) {
        // SAFETY: the revision comes from the class's own list of supported revisions.
        unsafe { request.setRevision(revision) };
    }
    // Text of any height counts; Vision's default ignores text under a fraction of the image.
    request.setMinimumTextHeight(0.0);

    // The list depends on the level and the revision only, which stay the same for the life of
    // the process, so background indexing does not ask Vision for it on every image.
    static SUPPORTED: std::sync::OnceLock<Vec<LanguageTag>> = std::sync::OnceLock::new();
    let supported = match SUPPORTED.get() {
        Some(list) => list.clone(),
        None => {
            // Only a list that was read is kept; a failure is asked again next time.
            let list = supported_tags_of(&request)?;
            SUPPORTED.get_or_init(|| list).clone()
        }
    };
    let languages = resolve_languages(&options.languages, &supported);
    if !languages.is_empty() {
        let names: Vec<Retained<NSString>> = languages.iter().map(|t| NSString::from_str(&t.0)).collect();
        request.setRecognitionLanguages(&NSArray::from_retained_slice(&names));
    }
    // Language detection exists from revision 3; on older systems the explicit list decides.
    if options.detect_language && request.respondsToSelector(objc2::sel!(setAutomaticallyDetectsLanguage:)) {
        request.setAutomaticallyDetectsLanguage(true);
    }
    Ok(request)
}

/// The newest revision a request class supports on this system.
fn newest_revision(class: &objc2::runtime::AnyClass) -> Option<usize> {
    // SAFETY: `supportedRevisions` is a class method of every VNRequest subclass that returns an
    // index set; it is called on the subclass because each one has its own revisions.
    let revisions: Option<Retained<NSIndexSet>> = unsafe { msg_send![class, supportedRevisions] };
    let last = revisions?.lastIndex();
    // NSNotFound is NSIntegerMax.
    (last != isize::MAX as usize).then_some(last)
}

fn supported_tags() -> Result<Vec<LanguageTag>> {
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    if let Some(revision) = newest_revision(VNRecognizeTextRequest::class()) {
        // SAFETY: see `text_request`.
        unsafe { request.setRevision(revision) };
    }
    supported_tags_of(&request)
}

fn supported_tags_of(request: &VNRecognizeTextRequest) -> Result<Vec<LanguageTag>> {
    // SAFETY: reads the request's own configuration; the error is returned, not thrown.
    let languages = unsafe { request.supportedRecognitionLanguagesAndReturnError() }
        .map_err(|error| OcrError::Engine(describe(&error)))?;
    Ok(languages.iter().map(|tag| LanguageTag(tag.to_string())).collect())
}

/// Maps the settings' languages ("tr", "en-GB") to the tags Vision supports ("tr-TR",
/// "en-US"), keeping their order and dropping the ones it cannot read.
fn resolve_languages(wanted: &[LanguageTag], supported: &[LanguageTag]) -> Vec<LanguageTag> {
    let mut out: Vec<LanguageTag> = Vec::new();
    for tag in wanted {
        let exact = supported.iter().find(|s| s.0.eq_ignore_ascii_case(&tag.0));
        let found = exact.or_else(|| supported.iter().find(|s| s.primary() == tag.primary()));
        if let Some(found) = found {
            if !out.contains(found) {
                out.push(found.clone());
            }
        }
    }
    out
}

/// The words of one line with their own boxes. Vision boxes any character range of a candidate;
/// a word whose box it cannot give gets a share of the line's box by its length.
fn words_of(top: &VNRecognizedText, text: &str, width: f64, height: f64, confidence: f64) -> Vec<OcrWord> {
    let mut words = Vec::new();
    let mut utf16 = 0usize;
    let mut start: Option<(usize, usize)> = None; // (utf-16 offset, byte offset)
    let mut push = |from: (usize, usize), to_utf16: usize, to_byte: usize| {
        let word = &text[from.1..to_byte];
        let range = NSRange::new(from.0, to_utf16 - from.0);
        // SAFETY: `boundingBoxForRange:error:` takes a range inside the candidate's string,
        // which `range` is, and returns an observation or an error.
        let boxed: std::result::Result<Retained<VNRectangleObservation>, Retained<NSError>> =
            unsafe { msg_send![top, boundingBoxForRange: range, error: _] };
        if let Ok(rect) = boxed {
            // SAFETY: a plain property read on a finished observation.
            let bounds = to_pixels(unsafe { rect.boundingBox() }, width, height);
            words.push(OcrWord { text: word.to_string(), bounds, confidence });
        }
    };
    for (byte, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(from) = start.take() {
                push(from, utf16, byte);
            }
        } else if start.is_none() {
            start = Some((utf16, byte));
        }
        utf16 += ch.len_utf16();
    }
    if let Some(from) = start {
        push(from, utf16, text.len());
    }
    words
}

// -------------------------------------------------------------------------------------- codes

fn read_codes(image: &Frame, cancel: &CancelToken) -> Result<Vec<CodeResult>> {
    let (width, height) = (image.size.width as f64, image.size.height as f64);
    if width < 1.0 || height < 1.0 {
        return Ok(Vec::new());
    }
    let cg_image = cg_image(image)?;
    // SAFETY: a plain initialiser.
    let request = unsafe { VNDetectBarcodesRequest::new() };
    if let Some(revision) = newest_revision(VNDetectBarcodesRequest::class()) {
        // SAFETY: the revision comes from the class's own list of supported revisions.
        unsafe { request.setRevision(revision) };
    }
    perform(&cg_image, &request, cancel)?;

    // SAFETY: reads the results of a finished request.
    let observations = unsafe { request.results() }.map(|r| r.to_vec()).unwrap_or_default();
    let mut codes = Vec::new();
    for observation in observations {
        let Some(payload) = barcode_payload(&observation) else {
            continue;
        };
        // SAFETY: plain property reads on a finished observation.
        let (symbology, rect) = unsafe { (observation.symbology(), observation.boundingBox()) };
        let url = codes::link_for(&payload);
        codes.push(CodeResult {
            symbology: symbology_of(&symbology),
            payload,
            bounds: to_pixels(rect, width, height),
            url,
        });
    }
    cancel.check()?;
    Ok(codes)
}

fn barcode_payload(observation: &VNBarcodeObservation) -> Option<String> {
    // SAFETY: a plain property read on a finished observation.
    let payload = unsafe { observation.payloadStringValue() }?.to_string();
    (!payload.is_empty()).then_some(payload)
}

fn symbology_of(name: &NSString) -> Symbology {
    // Vision's symbology constants are strings such as "VNBarcodeSymbologyQR".
    let name = name.to_string();
    let short = name.trim_start_matches("VNBarcodeSymbology");
    match short {
        "QR" => Symbology::Qr,
        "MicroQR" => Symbology::MicroQr,
        "Aztec" => Symbology::Aztec,
        "DataMatrix" => Symbology::DataMatrix,
        "PDF417" | "MicroPDF417" => Symbology::Pdf417,
        "EAN8" => Symbology::Ean8,
        "EAN13" => Symbology::Ean13,
        "UPCE" => Symbology::UpcE,
        "Code39" | "Code39Checksum" | "Code39FullASCII" | "Code39FullASCIIChecksum" => Symbology::Code39,
        "Code93" | "Code93i" => Symbology::Code93,
        "Code128" => Symbology::Code128,
        "I2of5" | "I2of5Checksum" | "ITF14" => Symbology::Itf,
        "Codabar" => Symbology::Codabar,
        _ => Symbology::Other,
    }
}

// ----------------------------------------------------------------------------------- plumbing

/// Runs one request over one image, and cancels it when the token is cancelled.
fn perform(image: &CGImage, request: &VNRequest, cancel: &CancelToken) -> Result<()> {
    // SAFETY: allocates a handler for an image that outlives it; the options are empty.
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            image,
            &NSDictionary::<NSString, AnyObject>::new(),
        )
    };

    let finished = Arc::new(AtomicBool::new(false));
    let watcher = Watcher::start(request, cancel.clone(), finished.clone());
    let requests = NSArray::from_slice(&[request]);
    let outcome = handler.performRequests_error(&requests);
    finished.store(true, Ordering::SeqCst);
    if let Some(watcher) = watcher {
        let _ = watcher.join();
    }

    match outcome {
        _ if cancel.is_cancelled() => Err(OcrError::Cancelled),
        Ok(()) => Ok(()),
        Err(error) => Err(OcrError::Engine(describe(&error))),
    }
}

/// A raw request pointer that may cross to the watcher thread. `-[VNRequest cancel]` is meant to
/// be called from another thread while the request runs, and nothing else is done with it.
struct RequestPointer(*const VNRequest);
// SAFETY: the pointer is only used for `cancel`, which Vision documents for exactly this use,
// and only while `perform` keeps the request alive (it joins the watcher before returning).
unsafe impl Send for RequestPointer {}

struct Watcher;

impl Watcher {
    fn start(
        request: &VNRequest,
        cancel: CancelToken,
        finished: Arc<AtomicBool>,
    ) -> Option<std::thread::JoinHandle<()>> {
        let pointer = RequestPointer(request as *const VNRequest);
        std::thread::Builder::new()
            .name("glimpsfy-ocr-cancel".into())
            .spawn(move || {
                let pointer = pointer;
                while !finished.load(Ordering::SeqCst) {
                    if cancel.is_cancelled() {
                        // SAFETY: `perform` joins this thread before the request can be released.
                        unsafe { (*pointer.0).cancel() };
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
            })
            .ok()
    }
}

/// The frame as a CGImage that Vision can read; the pixels are copied once.
fn cg_image(frame: &Frame) -> Result<CFRetained<CGImage>> {
    let (width, height) = (frame.size.width as usize, frame.size.height as usize);
    let needed = frame.bytes_per_row * height.saturating_sub(1) + width * 4;
    if frame.data.len() < needed {
        return Err(OcrError::InvalidImage("the pixel buffer is shorter than its size".into()));
    }
    let data = CFData::from_bytes(&frame.data[..needed]);
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or_else(|| OcrError::Engine("Core Graphics could not wrap the pixels".into()))?;
    // SAFETY: an Apple constant, valid for the life of the process.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| OcrError::Engine("the sRGB colour space is missing".into()))?;
    // Captures of screens are opaque, so the alpha byte is skipped.
    let info = match frame.format {
        PixelFormat::Bgra8 => CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::NoneSkipFirst.0,
        PixelFormat::Rgba8 => CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::NoneSkipLast.0,
    };
    // SAFETY: the provider holds `needed` bytes laid out as `width` × `height` pixels of 32 bits
    // at `bytes_per_row`, which is what the arguments describe; `decode` may be null.
    unsafe {
        CGImage::new(
            width,
            height,
            8,
            32,
            frame.bytes_per_row,
            Some(&space),
            CGBitmapInfo(info),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or_else(|| OcrError::InvalidImage("Core Graphics refused the pixel layout".into()))
}

/// Turns Vision's normalised, bottom-left-origin rectangle into pixels from the top-left corner.
fn to_pixels(rect: CGRect, width: f64, height: f64) -> Bounds {
    let x = (rect.origin.x * width).clamp(0.0, width);
    let right = ((rect.origin.x + rect.size.width) * width).clamp(0.0, width);
    let top = ((1.0 - rect.origin.y - rect.size.height) * height).clamp(0.0, height);
    let bottom = ((1.0 - rect.origin.y) * height).clamp(0.0, height);
    Bounds::new(x, top, (right - x).max(0.0), (bottom - top).max(0.0))
}

fn describe(error: &NSError) -> String {
    error.localizedDescription().to_string()
}
