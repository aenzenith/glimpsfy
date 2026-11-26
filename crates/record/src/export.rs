//! MP4 and GIF export of a finished recording, written next to its final name under a hidden
//! working name, and moved to a free name only when complete.
//!
//! An existing file is never replaced: the final move refuses a taken name and picks the next
//! free one. A cancelled or failed export removes its working file, so nothing half-written is
//! ever left under a name a person would open.
//!
//! MP4: with the original resolution and the "high" quality the video passes through untouched
//! (no generation loss, seconds instead of minutes), and a trim then starts at the keyframe at
//! or before the asked start; [`ExportResult::trim`] says where it really starts. Any other
//! resolution or quality re-encodes the video to H.264 and cuts exactly. Audio always passes
//! through: the export keeps the one track that matches the audio switches.
//!
//! With cuts ([`ExportOptions::cuts`]) the file holds the parts between them end to end
//! ([`crate::cuts`]), and the MP4 is always encoded again: each kept part is decoded (its frames
//! and the kept audio track, on two threads) and handed in time order to the take writer in its
//! offline mode, which drops nothing, with its sound faded in and out at every join. A GIF with
//! cuts leaves the cut parts out the same way.
//!
//! Subtitles ([`ExportOptions::captions`], [`Subtitles`]) are made once the kept parts are known,
//! so they follow a pass-through trim to its keyframe and every cut. A track goes into the
//! working file before it takes its name; an SRT or VTT file is written under a working name
//! first, so a full disk fails the whole export, and takes the MP4's name with its own extension.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use glimpsfy_capture::{ColorSpace, PhysicalSize, VideoFrame};

use crate::audio::{self, CHANNELS};
use crate::captions::{self, Caption};
use crate::cuts::{self, Span};
use crate::files;
use crate::gif::{GifEncoder, Picture};
use crate::session::{self, TakeWriter, WriterSpec};
use crate::{
    ExportAudio, ExportCaptions, ExportFormat, ExportOptions, ExportPurpose, ExportResult, MediaInfo, Mp4Quality,
    RecordError, RecordingId, Resolution, Result, TrackKind, TrimRange, VideoCodec,
};

/// How long an export waits for an encoder that takes nothing before it gives up.
const STALL: Duration = Duration::from_secs(30);

/// How an MP4 export writes its video and which audio track it keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct Mp4Plan {
    pub trim: TrimRange,
    /// `None` passes the video through; otherwise it is re-encoded at this size and bit rate.
    pub reencode: Option<Reencode>,
    /// The audio track to keep, as an index among the file's audio tracks.
    pub audio_track: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reencode {
    pub size: PhysicalSize,
    pub bits_per_second: u32,
}

/// The export's progress: the share done and an estimate of the time left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub fraction: f64,
    pub remaining_ms: Option<u64>,
}

/// Bits per pixel per frame of a re-encoded export, by quality, measured on screen recordings
/// (text and flat colour) so the "high" setting keeps small text sharp.
pub fn bits_per_pixel(quality: Mp4Quality) -> f64 {
    match quality {
        Mp4Quality::High => 0.10,
        Mp4Quality::Medium => 0.06,
        Mp4Quality::Low => 0.035,
    }
}

/// The trim clamped to the recording, at least a tenth of a second long.
pub fn clamp_trim(trim: Option<TrimRange>, duration_ms: u64) -> TrimRange {
    let duration_ms = duration_ms.max(1);
    let Some(t) = trim else { return TrimRange { start_ms: 0, end_ms: duration_ms } };
    let end = t.end_ms.min(duration_ms);
    let start = t.start_ms.min(end.saturating_sub(100));
    TrimRange { start_ms: start, end_ms: end.max(start + 1) }
}

/// The audio track an export keeps for the switches, among the file's audio tracks.
pub fn pick_audio(tracks: &[TrackKind], wanted: ExportAudio) -> Option<usize> {
    let find = |kind| tracks.iter().position(|&k| k == kind);
    match (wanted.microphone, wanted.system) {
        (false, false) => None,
        (true, true) => {
            find(TrackKind::MixedAudio).or_else(|| find(TrackKind::Microphone)).or_else(|| find(TrackKind::SystemAudio))
        }
        (true, false) => find(TrackKind::Microphone).or_else(|| {
            // The sources share one track: the switch for either keeps it.
            find(TrackKind::MixedAudio).filter(|_| !tracks.contains(&TrackKind::SystemAudio))
        }),
        (false, true) => find(TrackKind::SystemAudio)
            .or_else(|| find(TrackKind::MixedAudio).filter(|_| !tracks.contains(&TrackKind::Microphone))),
    }
}

fn even(n: u32) -> u32 {
    (n & !1).max(2)
}

/// Half of `n`, rounded to an even number; the trim window's estimate (`halfSize` in
/// `src/recorder/estimate.ts`) rounds the same way.
fn half(n: u32) -> u32 {
    (2 * ((n as f64 / 4.0).round() as u32)).max(2)
}

/// The size and bit rate a re-encode of a file described by `info` gets.
fn reencode_for(info: &MediaInfo, resolution: Resolution, quality: Mp4Quality) -> Reencode {
    let size = match resolution {
        Resolution::Native => PhysicalSize { width: even(info.size.width), height: even(info.size.height) },
        Resolution::Half => PhysicalSize { width: half(info.size.width), height: half(info.size.height) },
    };
    // A screen recording has a variable frame rate (a still screen sends no frames), so its
    // nominal rate says little; the bit rate is set for at least 30 frames per second.
    let fps = info.fps.round().max(30.0);
    let bits = size.width as f64 * size.height as f64 * fps * bits_per_pixel(quality);
    Reencode { size, bits_per_second: bits.clamp(250_000.0, 80_000_000.0) as u32 }
}

/// The MP4 plan for `options` on a file described by `info`.
pub fn mp4_plan(info: &MediaInfo, options: &ExportOptions, resolution: Resolution, quality: Mp4Quality) -> Mp4Plan {
    let trim = clamp_trim(options.trim, info.duration_ms);
    let audio_track = pick_audio(&info.audio, options.audio);
    let reencode = if resolution == Resolution::Native && quality == Mp4Quality::High {
        None
    } else {
        Some(reencode_for(info, resolution, quality))
    };
    Mp4Plan { trim, reencode, audio_track }
}

/// The GIF's size for a wanted width, keeping the aspect ratio with an even height; a width at
/// or above the recording's keeps its size (as `gifSize` in `src/recorder/estimate.ts`).
pub fn gif_size(source: PhysicalSize, width: Option<u32>) -> PhysicalSize {
    match width {
        Some(w) if w < source.width => {
            let w = w.max(16);
            let h = 2 * ((w as f64 * source.height as f64 / source.width.max(1) as f64 / 2.0).round() as u32);
            PhysicalSize { width: w, height: h.max(2) }
        }
        _ => source,
    }
}

/// Where the subtitles of an export come from: the speech of the recording, which only the app
/// holds.
pub struct Subtitles<'a> {
    /// The language of the speech, as a transcript names it ("tr", "en").
    pub language: &'a str,
    /// The captions of a file that holds these parts of the recording, in the file's time.
    pub captions: &'a dyn Fn(&[TrimRange]) -> Vec<Caption>,
}

/// Exports `source` (the recording `id`) as `options` asks into `wanted`, or the first free name
/// next to it, and returns what was written. `progress` is called as the export goes on;
/// setting `cancel` stops it and leaves no file behind.
pub fn export(
    id: &RecordingId,
    source: &Path,
    options: &ExportOptions,
    wanted: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<ExportResult> {
    export_with(id, source, options, wanted, None, cancel, progress)
}

/// [`export`] with the subtitles `options.captions` asks for; without `subtitles` the file gets
/// none.
pub fn export_with(
    id: &RecordingId,
    source: &Path,
    options: &ExportOptions,
    wanted: &Path,
    subtitles: Option<&Subtitles<'_>>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<ExportResult> {
    let info = crate::platform::inspect(source)?;
    let folder = wanted.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let part = files::part_path(wanted, &id.0);
    let started = Instant::now();
    let mut report = |fraction: f64| {
        let fraction = fraction.clamp(0.0, 1.0);
        let spent = started.elapsed().as_millis() as f64;
        let remaining_ms = (fraction > 0.02).then(|| (spent / fraction * (1.0 - fraction)) as u64);
        progress(Progress { fraction, remaining_ms });
    };
    report(0.0);
    let outcome = write(&info, source, options, &part, cancel, &mut report);
    let Written { trim, reencoded, size, kept } = match outcome {
        Ok(done) if !cancel.load(Ordering::SeqCst) => done,
        Ok(_) => {
            let _ = std::fs::remove_file(&part);
            return Err(RecordError::Cancelled);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(if cancel.load(Ordering::SeqCst) { RecordError::Cancelled } else { e });
        }
    };
    let sidecar = match add_captions(id, options, wanted, &part, &kept, subtitles) {
        Ok(sidecar) => sidecar,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(if cancel.load(Ordering::SeqCst) { RecordError::Cancelled } else { e });
        }
    };
    if cancel.load(Ordering::SeqCst) {
        let _ = std::fs::remove_file(&part);
        if let Some((sidecar, _)) = &sidecar {
            let _ = std::fs::remove_file(sidecar);
        }
        return Err(RecordError::Cancelled);
    }
    let path = match files::place_without_replacing(&part, wanted) {
        Ok(path) => path,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            if let Some((sidecar, _)) = sidecar {
                let _ = std::fs::remove_file(sidecar);
            }
            return Err(e.into());
        }
    };
    // The text file takes the name the MP4 got, so a player finds it; a name that is taken
    // makes it the next free one instead of replacing a file.
    let captions_path = sidecar.and_then(|(sidecar, ext)| {
        let placed = files::place_without_replacing(&sidecar, &path.with_extension(ext));
        if placed.is_err() {
            let _ = std::fs::remove_file(&sidecar);
        }
        placed.ok()
    });
    report(1.0);
    let file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(ExportResult {
        recording_id: id.clone(),
        path,
        duration_ms: kept.iter().map(|k| k.end_ms - k.start_ms).sum(),
        size,
        file_bytes,
        reencoded,
        trim,
        kept,
        captions_path,
    })
}

/// Puts the subtitles `options` asks for into the working MP4 `part`, or writes them under a
/// working name next to `wanted`, which it returns with the extension the file takes.
fn add_captions(
    id: &RecordingId,
    options: &ExportOptions,
    wanted: &Path,
    part: &Path,
    kept: &[TrimRange],
    subtitles: Option<&Subtitles<'_>>,
) -> Result<Option<(PathBuf, &'static str)>> {
    let Some(subtitles) = subtitles else { return Ok(None) };
    if options.captions == ExportCaptions::None || !matches!(options.format, ExportFormat::Mp4 { .. }) {
        return Ok(None);
    }
    let list = (subtitles.captions)(kept);
    if list.is_empty() {
        return Ok(None);
    }
    let (text, ext) = match options.captions {
        ExportCaptions::None => return Ok(None),
        ExportCaptions::Track => {
            captions::add_track(part, &list, subtitles.language)?;
            return Ok(None);
        }
        // A copy or an upload hands on one file.
        _ if options.purpose != ExportPurpose::Save => return Ok(None),
        ExportCaptions::Srt => (captions::srt(&list), "srt"),
        ExportCaptions::Vtt => (captions::vtt(&list), "vtt"),
    };
    let sidecar = files::part_path(&wanted.with_extension(ext), &id.0);
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&sidecar).and_then(|mut file| {
        use std::io::Write;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&sidecar);
        return Err(e.into());
    }
    Ok(Some((sidecar, ext)))
}

/// What `write` wrote: the trim really exported, whether the video was encoded again, the
/// picture's size, and the parts of the recording the file holds.
struct Written {
    trim: TrimRange,
    reencoded: bool,
    size: PhysicalSize,
    kept: Vec<TrimRange>,
}

fn write(
    info: &MediaInfo,
    source: &Path,
    options: &ExportOptions,
    part: &Path,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f64),
) -> Result<Written> {
    let trim = clamp_trim(options.trim, info.duration_ms);
    let spans = cuts::kept_spans(trim, &options.cuts);
    if spans.is_empty() {
        return Err(RecordError::InvalidConfig("the cuts leave nothing of the recording to export".into()));
    }
    let has_cuts = spans.len() != 1 || spans[0].source != trim;
    let kept = |spans: &[Span]| -> Vec<TrimRange> { spans.iter().map(|s| s.source).collect() };
    match &options.format {
        ExportFormat::Mp4 { resolution, quality } => {
            let plan = mp4_plan(info, options, *resolution, *quality);
            if has_cuts {
                // A pass-through cannot join parts that start between keyframes, so an export
                // with cuts is always encoded again, at the plan's rate or else at "high".
                let rate = plan.reencode.unwrap_or_else(|| reencode_for(info, Resolution::Native, Mp4Quality::High));
                let spans = match plan.audio_track {
                    Some(track) => cuts::snapped(trim, &spans, &mut |edge, lo, hi| {
                        cuts::quietest(source, track, edge, lo, hi, cancel)
                    })?,
                    None => spans,
                };
                write_cut_mp4(info, source, part, &spans, rate, plan.audio_track, cancel, report)?;
                return Ok(Written { trim, reencoded: true, size: rate.size, kept: kept(&spans) });
            }
            let size = plan.reencode.map(|r| r.size).unwrap_or(info.size);
            let trim = crate::platform::write_mp4(source, part, &plan, cancel, report)?;
            Ok(Written { trim, reencoded: plan.reencode.is_some(), size, kept: vec![trim] })
        }
        ExportFormat::Gif { fps, width, loop_forever } => {
            let size = gif_size(info.size, *width);
            write_gif(source, part, &spans, (*fps).clamp(1, 50), size, *loop_forever, cancel, report)?;
            Ok(Written { trim, reencoded: true, size, kept: kept(&spans) })
        }
    }
}

/// The MP4 of the kept parts `spans`, encoded again at `rate` through the take writer in its
/// offline mode, with audio track `audio_track` (decoded and encoded again) as its one track.
#[allow(clippy::too_many_arguments)]
fn write_cut_mp4(
    info: &MediaInfo,
    source: &Path,
    part: &Path,
    spans: &[Span],
    rate: Reencode,
    audio_track: Option<usize>,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f64),
) -> Result<()> {
    let total_ms: u64 = spans.iter().map(Span::len_ms).sum();
    let kind = audio_track.and_then(|i| info.audio.get(i).copied());
    let spec = WriterSpec {
        path: part.to_path_buf(),
        size: rate.size,
        // The nominal rate of a variable-rate recording is low; the writer keys and paces for
        // at least 30 frames per second, as the bit rate does.
        fps: (info.fps.round() as u32).max(30),
        codec: session::codec_for(VideoCodec::H264, rate.size),
        color_space: ColorSpace::Srgb,
        audio: kind.into_iter().collect(),
        fragment_interval_ms: session::FRAGMENT_INTERVAL_MS,
        bits_per_second: Some(rate.bits_per_second),
        offline: true,
    };
    // On a failure or a cancel the writer is dropped here, which closes its file, and `export`
    // removes it.
    let mut writer = crate::platform::create_writer(&spec)?;
    let track = kind.and(audio_track);
    for (i, span) in spans.iter().enumerate() {
        let joins = (i > 0, i + 1 < spans.len());
        let mut progress = |done_ms: u64| report(done_ms as f64 / total_ms.max(1) as f64);
        feed(&mut *writer, source, span, track, joins, rate.size, cancel, &mut progress)?;
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(RecordError::Cancelled);
    }
    // The last picture lasts until the end; within the file, each part's last picture lasts
    // until the next part's first.
    writer.finish(total_ms * 1_000_000)
}

/// Writes one kept part: its frames and its sound are decoded on two threads and handed to the
/// writer in time order, a sound buffer before a frame only when it ends by the frame's time. So
/// no writer is ever more than a buffer of sound ahead of its pictures, and a writer that repeats
/// the last picture on a still screen (Windows, Linux) repeats it before the next frame comes.
/// `fades` says whether a join comes before the part and after it.
#[allow(clippy::too_many_arguments)]
fn feed(
    writer: &mut dyn TakeWriter,
    source: &Path,
    span: &Span,
    track: Option<usize>,
    fades: (bool, bool),
    size: PhysicalSize,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<()> {
    std::thread::scope(|scope| {
        let (video_tx, video_rx) = mpsc::sync_channel::<(VideoFrame, u64)>(4);
        let video = scope.spawn(move || {
            crate::platform::decode_frames(source, span.source, size, cancel, &mut |frame, at| {
                video_tx.send((frame, at)).map_err(|_| RecordError::Cancelled)
            })
        });
        let (sound_tx, sound_rx) = mpsc::sync_channel::<(Vec<f32>, u64)>(16);
        let sound = track.map(|track| {
            scope.spawn(move || {
                crate::platform::decode_audio(source, track, span.source, cancel, &mut |samples, at| {
                    sound_tx.send((samples.to_vec(), at)).map_err(|_| RecordError::Cancelled)
                })
            })
        });
        let written = interleave(writer, span, track.is_some(), fades, &video_rx, &sound_rx, cancel, progress);
        // A decoder still running stops at its next send.
        drop(video_rx);
        drop(sound_rx);
        let failed = |what: &str| RecordError::Encoder(format!("the {what} decoder stopped"));
        let video = video.join().unwrap_or_else(|_| Err(failed("video")));
        let sound = sound.map_or(Ok(()), |h| h.join().unwrap_or_else(|_| Err(failed("audio"))));
        written?;
        video?;
        sound
    })
}

/// The merge of `feed`: frames and sound buffers of `span` from the decoders, into the writer.
#[allow(clippy::too_many_arguments)]
fn interleave(
    writer: &mut dyn TakeWriter,
    span: &Span,
    with_sound: bool,
    (fade_in, fade_out): (bool, bool),
    video: &mpsc::Receiver<(VideoFrame, u64)>,
    sound: &mpsc::Receiver<(Vec<f32>, u64)>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<()> {
    let base_ns = span.output_start_ms * 1_000_000;
    let mut track = SpanSound {
        base: audio::frames_at(base_ns),
        length: audio::frames_at(span.len_ms() * 1_000_000),
        ramp: 0,
        fade_in,
        fade_out,
        written: 0,
    };
    track.ramp = audio::frames_at(cuts::RAMP_MS * 1_000_000).min(track.length / 2);
    let mut next_frame = video.recv().ok();
    let mut next_sound = sound.recv().ok();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(RecordError::Cancelled);
        }
        let frame_first = match (&next_frame, &next_sound) {
            (None, None) => break,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some((_, at)), Some((samples, position))) => {
                *at < audio::ns_at(position + (samples.len() / CHANNELS) as u64)
            }
        };
        if frame_first {
            if let Some((frame, at)) = next_frame.take() {
                append_frame(writer, &frame, base_ns + at, cancel)?;
                progress(span.output_start_ms + at / 1_000_000);
            }
            next_frame = video.recv().ok();
        } else {
            if let Some((mut samples, position)) = next_sound.take() {
                track.write(writer, position, &mut samples)?;
            }
            next_sound = sound.recv().ok();
        }
    }
    if with_sound {
        // The part's sound lasts as long as the part, with silence where the track has none.
        track.write(writer, track.length, &mut [])?;
    }
    Ok(())
}

/// Hands `frame` to the writer at `at_ns`, waiting while its encoder is busy (the writer says
/// so by refusing the frame): an export drops no frame.
fn append_frame(writer: &mut dyn TakeWriter, frame: &VideoFrame, at_ns: u64, cancel: &AtomicBool) -> Result<()> {
    let deadline = Instant::now() + STALL;
    while !writer.append_video(frame, at_ns)? {
        if cancel.load(Ordering::SeqCst) {
            return Err(RecordError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(RecordError::Encoder("the encoder stopped taking frames".into()));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

/// The sound of one kept part on its way to the writer's one audio track, contiguous from the
/// part's start in the file (`base`) for `length` frames, faded over `ramp` frames at a join.
struct SpanSound {
    base: u64,
    length: u64,
    ramp: u64,
    fade_in: bool,
    fade_out: bool,
    /// The frames of the part written so far.
    written: u64,
}

impl SpanSound {
    /// Writes `samples` (stereo) at `position` from the part's start, after silence for any gap
    /// before it and without what overlaps the sound already written or passes the part's end.
    fn write(&mut self, writer: &mut dyn TakeWriter, position: u64, samples: &mut [f32]) -> Result<()> {
        let position = position.min(self.length);
        while self.written < position {
            let gap = (position - self.written).min(audio::RATE as u64 / 10);
            self.put(writer, &mut vec![0.0; gap as usize * CHANNELS])?;
        }
        let frames = (samples.len() / CHANNELS) as u64;
        let skip = (self.written - position).min(frames);
        let take = (frames - skip).min(self.length - self.written);
        let body = &mut samples[skip as usize * CHANNELS..(skip + take) as usize * CHANNELS];
        self.put(writer, body)
    }

    fn put(&mut self, writer: &mut dyn TakeWriter, samples: &mut [f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        for (i, frame) in samples.chunks_exact_mut(CHANNELS).enumerate() {
            let gain = cuts::ramp_gain(self.written + i as u64, self.length, self.ramp, self.fade_in, self.fade_out);
            if gain < 1.0 {
                frame.iter_mut().for_each(|s| *s *= gain);
            }
        }
        writer.append_audio(0, self.base + self.written, samples)?;
        self.written += (samples.len() / CHANNELS) as u64;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn write_gif(
    source: &Path,
    part: &Path,
    spans: &[Span],
    fps: u32,
    size: PhysicalSize,
    loop_forever: bool,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f64),
) -> Result<()> {
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(part)?;
    let mut gif = GifEncoder::new(std::io::BufWriter::new(file), size.width, size.height, loop_forever)?;
    let length_ns = spans.iter().map(Span::len_ms).sum::<u64>() * 1_000_000;
    let step = 1_000_000_000 / fps as u64;
    let mut next = 0u64;
    // Each kept part is decoded on its own and placed after the ones before it.
    for span in spans {
        let base = span.output_start_ms * 1_000_000;
        crate::platform::decode_video(source, span.source, size, cancel, &mut |rgba: &[u8], at: u64| {
            let at_ns = base + at;
            // A frame a little early still counts for its tick: decoded times wobble by a few
            // microseconds around the frame grid.
            const SLACK: u64 = 2_000_000;
            if at_ns + SLACK < next {
                return Ok(());
            }
            gif.push(Picture { rgba }, at_ns)?;
            while next <= at_ns + SLACK {
                next += step;
            }
            report(at_ns as f64 / length_ns.max(1) as f64);
            Ok(())
        })?;
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(RecordError::Cancelled);
    }
    let out = gif.finish(length_ns)?;
    let file = out.into_inner().map_err(|e| RecordError::Io(e.into_error()))?;
    file.sync_all()?;
    Ok(())
}

/// The file name an export of `recording` takes in `folder` for `format`, before a free name
/// is picked.
pub fn wanted_path(folder: &Path, recording: &Path, format: &ExportFormat, trimmed: bool) -> PathBuf {
    let stem = recording.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Recording".into());
    let stem = if trimmed { format!("{stem} (trimmed)") } else { stem };
    let ext = match format {
        ExportFormat::Mp4 { .. } => "mp4",
        ExportFormat::Gif { .. } => "gif",
    };
    folder.join(format!("{stem}.{ext}"))
}
