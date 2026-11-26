//! The whisper.cpp engine: the verified model loaded from memory, language detection, and
//! recognition whose token times (DTW over the model's alignment heads) become word times.
//!
//! The model runs on Metal on a Mac and on the CPU elsewhere. whisper.cpp reads the sound in 30 s
//! windows; after each window its progress callback hands the segments finished so far to the
//! caller, so the trim window shows text while the rest is read. The cancel token is whisper.cpp's
//! abort callback, which the model checks between its steps.

use std::any::Any;
use std::ffi::{c_int, c_void, CStr};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

use whisper_rs::whisper_rs_sys as sys;
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
    WhisperError, WhisperSegment,
};

use crate::engine::{unspaced, Detected, Recognized, Request, SpeechEngine};
use crate::model::{SpeechModelStore, MODEL_ID};
use crate::{CancelToken, Result, SpeechError, Word, SAMPLE_RATE};

/// The most sound language detection reads; whisper.cpp encodes one 30 s window for it.
const DETECT_SAMPLES: usize = 30 * SAMPLE_RATE as usize;
/// Scratch memory for the token alignment of one window (whisper.cpp's default).
const DTW_MEMORY: usize = 128 << 20;

/// Whisper large-v3-turbo through whisper.cpp.
pub struct WhisperEngine {
    context: WhisperContext,
    threads: usize,
    /// Tokens from this id on are special (end of text, language, timestamps), not text.
    end_of_text: i32,
}

impl WhisperEngine {
    /// Loads the speech model from the store, on the GPU where the build has one (Metal on a
    /// Mac). The file's bytes are hashed while they are read and handed to whisper.cpp from
    /// memory, so a file swapped after the check is never loaded. Takes a few seconds and about
    /// twice the model's size in memory while it runs; the bytes are dropped once loaded.
    pub fn load(store: &SpeechModelStore) -> Result<WhisperEngine> {
        Self::load_with(store, true)
    }

    /// [`WhisperEngine::load`], with the GPU left out when `gpu` is false (for measuring the CPU
    /// path that Windows and Linux take).
    pub fn load_with(store: &SpeechModelStore, gpu: bool) -> Result<WhisperEngine> {
        // whisper.cpp writes every step of loading and decoding to stderr; without a log backend
        // the hooks drop it, and the app reports failures through the errors instead.
        static QUIET: Once = Once::new();
        QUIET.call_once(whisper_rs::install_logging_hooks);

        let bytes = store.read()?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(gpu && cfg!(target_os = "macos"));
        // Flash attention turns the alignment heads off, and with them the word times.
        params.flash_attn(false);
        params.dtw_parameters(DtwParameters {
            mode: DtwMode::ModelPreset { model_preset: DtwModelPreset::LargeV3Turbo },
            dtw_mem_size: DTW_MEMORY,
        });
        let context = WhisperContext::new_from_buffer_with_params(&bytes, params).map_err(engine_error)?;
        drop(bytes);
        let end_of_text = context.token_eot();
        Ok(WhisperEngine { context, threads: threads(gpu), end_of_text })
    }

    /// How many CPU threads a recognition uses.
    pub fn threads(&self) -> usize {
        self.threads
    }
}

/// The CPU threads for whisper.cpp: with Metal the GPU does the heavy part and four threads feed
/// it; on the CPU one core is left to the rest of the machine, and more than eight gain nothing.
fn threads(gpu: bool) -> usize {
    let available = std::thread::available_parallelism().map_or(4, |n| n.get());
    if gpu && cfg!(target_os = "macos") {
        available.clamp(1, 4)
    } else {
        available.saturating_sub(1).clamp(1, 8)
    }
}

impl SpeechEngine for WhisperEngine {
    fn model(&self) -> &str {
        MODEL_ID
    }

    fn language(&self, tag: &str) -> Option<String> {
        let primary = tag.split(['-', '_']).next()?.trim().to_ascii_lowercase();
        if primary.is_empty() || primary.contains('\0') || primary == "auto" {
            return None;
        }
        // Whisper keeps a few older codes.
        let code = match primary.as_str() {
            "nb" => "no",
            "iw" => "he",
            "jv" => "jw",
            "fil" => "tl",
            other => other,
        };
        whisper_rs::get_lang_id(code).map(|_| code.to_string())
    }

    fn detect_language(&self, audio: &[f32], cancel: &CancelToken) -> Result<Detected> {
        cancel.check()?;
        let mut state = self.context.create_state().map_err(engine_error)?;
        state.pcm_to_mel(&audio[..audio.len().min(DETECT_SAMPLES)], self.threads).map_err(engine_error)?;
        let (id, probabilities) = state.lang_detect(0, self.threads).map_err(engine_error)?;
        let language = whisper_rs::get_lang_str(id)
            .ok_or_else(|| SpeechError::Engine(format!("the model detected an unknown language ({id})")))?;
        let probability = usize::try_from(id).ok().and_then(|i| probabilities.get(i)).copied().unwrap_or(0.0);
        Ok(Detected { language: language.to_string(), probability })
    }

    fn recognize(
        &self,
        audio: &[f32],
        request: &Request<'_>,
        progress: &mut dyn FnMut(Recognized),
        cancel: &CancelToken,
    ) -> Result<()> {
        cancel.check()?;
        if audio.is_empty() {
            return Ok(());
        }
        let mut state = self.context.create_state().map_err(engine_error)?;

        // Greedy decoding keeps the speaker's words as said; a beam search prefers the fluent
        // sentence and drops hesitations. `best_of` applies to the retries at a higher temperature
        // that whisper.cpp makes when a window comes out repetitive.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 5 });
        params.set_n_threads(self.threads as c_int);
        params.set_language(Some(request.language));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Without the brackets, quotes and music notes Whisper writes for sounds ("[Müzik]", "♪")
        // nothing that was not said turns into a word; hesitations are letters and stay.
        params.set_suppress_nst(true);
        if let Some(prompt) = request.prompt {
            params.set_initial_prompt(prompt);
        }

        let abort = Abort { cancel: cancel.flag(), failed: AtomicBool::new(false) };
        let mut sink = Sink {
            progress,
            abort: &abort,
            end_of_text: self.end_of_text,
            unspaced: unspaced(request.language),
            total_ms: audio.len() as u64 * 1000 / u64::from(SAMPLE_RATE),
            reported: 0,
            panic: None,
        };
        // SAFETY: both pointers name locals that outlive `full`, which is the only time
        // whisper.cpp calls the callbacks, and the callbacks run on this thread.
        unsafe {
            params.set_progress_callback(Some(on_progress));
            params.set_progress_callback_user_data(&mut sink as *mut Sink as *mut c_void);
            params.set_abort_callback(Some(on_abort));
            params.set_abort_callback_user_data(&abort as *const Abort as *mut c_void);
        }
        let outcome = state.full(params, audio);
        if let Some(payload) = sink.panic.take() {
            panic::resume_unwind(payload);
        }
        cancel.check()?;
        outcome.map_err(engine_error)?;

        // The callback runs before each window, so the last window's segments normally arrive
        // there too; any it missed are read here.
        let segments = state.full_n_segments();
        let mut words = Vec::new();
        for i in sink.reported..segments {
            if let Some(segment) = state.get_segment(i) {
                words.extend(segment_words(&read_segment(&segment), sink.end_of_text, sink.unspaced));
            }
        }
        (sink.progress)(Recognized { done_ms: sink.total_ms, words });
        Ok(())
    }
}

fn engine_error(error: WhisperError) -> SpeechError {
    SpeechError::Engine(error.to_string())
}

/// What the abort callback reads: the caller's cancel flag, and whether the progress callback
/// failed and the recognition has to stop.
struct Abort<'a> {
    cancel: &'a AtomicBool,
    failed: AtomicBool,
}

/// What the progress callback writes to.
struct Sink<'a> {
    progress: &'a mut dyn FnMut(Recognized),
    abort: &'a Abort<'a>,
    end_of_text: i32,
    unspaced: bool,
    total_ms: u64,
    /// Segments already turned into words.
    reported: c_int,
    /// A panic of the caller's callback, carried past whisper.cpp and raised again.
    panic: Option<Box<dyn Any + Send>>,
}

unsafe extern "C" fn on_abort(data: *mut c_void) -> bool {
    // SAFETY: `data` is the `Abort` set in `recognize`, alive for the whole of `full`.
    let abort = unsafe { &*(data as *const Abort) };
    abort.cancel.load(Ordering::Relaxed) || abort.failed.load(Ordering::Relaxed)
}

unsafe extern "C" fn on_progress(
    ctx: *mut sys::whisper_context,
    state: *mut sys::whisper_state,
    percent: c_int,
    data: *mut c_void,
) {
    // SAFETY: `data` is the `Sink` set in `recognize`, alive for the whole of `full`, and only
    // this callback touches it while `full` runs.
    let sink = unsafe { &mut *(data as *mut Sink) };
    if sink.panic.is_some() {
        return;
    }
    // A panic must not unwind into C++; it is kept and raised once `full` returns.
    let reported = panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: whisper.cpp passes its live context and state; the segments are only read.
        let segments = unsafe { sys::whisper_full_n_segments_from_state(state) };
        let mut words = Vec::new();
        for i in sink.reported..segments {
            let segment = unsafe { read_raw_segment(ctx, state, i) };
            words.extend(segment_words(&segment, sink.end_of_text, sink.unspaced));
        }
        sink.reported = sink.reported.max(segments);
        let done_ms = sink.total_ms * percent.clamp(0, 100) as u64 / 100;
        (sink.progress)(Recognized { done_ms, words });
    }));
    if let Err(payload) = reported {
        sink.panic = Some(payload);
        sink.abort.failed.store(true, Ordering::Relaxed);
    }
}

/// One token of a segment, as the word joining needs it.
struct Token {
    id: i32,
    probability: f32,
    /// The token's start from the alignment, in 10 ms units; negative when there is none.
    t_dtw: i64,
    /// The token's text as bytes; a token may hold part of a multi-byte letter.
    bytes: Vec<u8>,
}

/// A segment's times in 10 ms units and its tokens.
struct Segment {
    t0: i64,
    t1: i64,
    tokens: Vec<Token>,
}

/// Reads a segment through the raw API, for the progress callback, which has no safe handle.
///
/// # Safety
///
/// `ctx` and `state` must be whisper.cpp's live context and state, and `i` one of its segments.
unsafe fn read_raw_segment(ctx: *mut sys::whisper_context, state: *mut sys::whisper_state, i: c_int) -> Segment {
    unsafe {
        let n = sys::whisper_full_n_tokens_from_state(state, i);
        let tokens = (0..n)
            .map(|t| {
                let data = sys::whisper_full_get_token_data_from_state(state, i, t);
                let text = sys::whisper_full_get_token_text_from_state(ctx, state, i, t);
                let bytes = if text.is_null() { Vec::new() } else { CStr::from_ptr(text).to_bytes().to_vec() };
                Token { id: data.id, probability: data.p, t_dtw: data.t_dtw, bytes }
            })
            .collect();
        Segment {
            t0: sys::whisper_full_get_segment_t0_from_state(state, i),
            t1: sys::whisper_full_get_segment_t1_from_state(state, i),
            tokens,
        }
    }
}

fn read_segment(segment: &WhisperSegment<'_>) -> Segment {
    let tokens = (0..segment.n_tokens())
        .filter_map(|t| segment.get_token(t))
        .map(|token| {
            let data = token.token_data();
            Token {
                id: data.id,
                probability: data.p,
                t_dtw: data.t_dtw,
                bytes: token.to_bytes().map(<[u8]>::to_vec).unwrap_or_default(),
            }
        })
        .collect();
    Segment { t0: segment.start_timestamp(), t1: segment.end_timestamp(), tokens }
}

/// Marks that open the word after them.
const OPENING: &str = "\"'“‘«¿¡(「『（";

/// A word being joined from tokens.
struct Pending {
    bytes: Vec<u8>,
    /// Start in 10 ms units.
    start: i64,
    probability: f32,
    tokens: u32,
}

/// The words of a segment. A token whose text starts with a space starts a word, and the others
/// continue it, except in languages without spaces, where every token that completes its letters
/// is a word of its own. Special tokens are dropped, and punctuation joins the word before it.
fn segment_words(segment: &Segment, end_of_text: i32, unspaced: bool) -> Vec<Word> {
    let text: Vec<(usize, &Token)> =
        segment.tokens.iter().filter(|t| t.id < end_of_text && !t.bytes.is_empty()).enumerate().collect();
    let count = text.len().max(1) as i64;
    // Where the alignment gave no time, the token's share of the segment stands in.
    let time = |index: usize, token: &Token| {
        if token.t_dtw >= 0 {
            token.t_dtw
        } else {
            segment.t0 + (segment.t1 - segment.t0) * index as i64 / count
        }
    };

    let mut pending: Vec<Pending> = Vec::new();
    for (index, token) in text {
        let complete = pending.last().is_none_or(|w| std::str::from_utf8(&w.bytes).is_ok());
        let starts = pending.is_empty() || (complete && (token.bytes[0] == b' ' || unspaced));
        if starts {
            pending.push(Pending { bytes: Vec::new(), start: time(index, token), probability: 0.0, tokens: 0 });
        }
        let word = pending.last_mut().expect("a word was started");
        word.bytes.extend_from_slice(&token.bytes);
        word.probability += token.probability;
        word.tokens += 1;
    }

    let mut words: Vec<Word> = Vec::with_capacity(pending.len());
    let mut prefix = String::new();
    let mut previous_start = 0i64;
    for word in &pending {
        let text = String::from_utf8_lossy(&word.bytes).trim().to_string();
        if !text.chars().any(char::is_alphanumeric) {
            // Punctuation alone belongs to the word before; an opening mark at a segment's start
            // belongs to the next one, and anything else there (a lone dash) is dropped.
            match words.last_mut() {
                Some(last) => last.text.push_str(&text),
                None if text.chars().all(|c| OPENING.contains(c)) => prefix.push_str(&text),
                None => {}
            }
            continue;
        }
        let start = word.start.max(previous_start);
        previous_start = start;
        words.push(Word {
            text: format!("{}{text}", std::mem::take(&mut prefix)),
            start_ms: ms(start),
            end_ms: 0,
            filler: None,
            probability: word.probability / word.tokens.max(1) as f32,
            edited: None,
        });
    }
    // A word lasts until the next one starts; the last one until the segment ends.
    let end = ms(segment.t1);
    for i in 0..words.len() {
        let next = words.get(i + 1).map_or(end, |w| w.start_ms);
        words[i].end_ms = next.max(words[i].start_ms + 10);
    }
    words
}

fn ms(centiseconds: i64) -> u64 {
    centiseconds.max(0) as u64 * 10
}
