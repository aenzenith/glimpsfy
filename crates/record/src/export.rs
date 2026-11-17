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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use glimpsfy_capture::PhysicalSize;

use crate::files;
use crate::gif::{GifEncoder, Picture};
use crate::{
    ExportAudio, ExportFormat, ExportOptions, ExportResult, MediaInfo, Mp4Quality, RecordError, RecordingId,
    Resolution, Result, TrackKind, TrimRange,
};

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

/// The MP4 plan for `options` on a file described by `info`.
pub fn mp4_plan(info: &MediaInfo, options: &ExportOptions, resolution: Resolution, quality: Mp4Quality) -> Mp4Plan {
    let trim = clamp_trim(options.trim, info.duration_ms);
    let audio_track = pick_audio(&info.audio, options.audio);
    let reencode = if resolution == Resolution::Native && quality == Mp4Quality::High {
        None
    } else {
        let size = match resolution {
            Resolution::Native => PhysicalSize { width: even(info.size.width), height: even(info.size.height) },
            Resolution::Half => PhysicalSize { width: half(info.size.width), height: half(info.size.height) },
        };
        // A screen recording has a variable frame rate (a still screen sends no frames), so its
        // nominal rate says little; the bit rate is set for at least 30 frames per second.
        let fps = info.fps.round().max(30.0);
        let bits = size.width as f64 * size.height as f64 * fps * bits_per_pixel(quality);
        Some(Reencode { size, bits_per_second: bits.clamp(250_000.0, 80_000_000.0) as u32 })
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
    let (trim, reencoded, size) = match outcome {
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
    let path = match files::place_without_replacing(&part, wanted) {
        Ok(path) => path,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e.into());
        }
    };
    report(1.0);
    let file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(ExportResult {
        recording_id: id.clone(),
        path,
        duration_ms: trim.end_ms - trim.start_ms,
        size,
        file_bytes,
        reencoded,
        trim,
    })
}

fn write(
    info: &MediaInfo,
    source: &Path,
    options: &ExportOptions,
    part: &Path,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f64),
) -> Result<(TrimRange, bool, PhysicalSize)> {
    match &options.format {
        ExportFormat::Mp4 { resolution, quality } => {
            let plan = mp4_plan(info, options, *resolution, *quality);
            let size = plan.reencode.map(|r| r.size).unwrap_or(info.size);
            let trim = crate::platform::write_mp4(source, part, &plan, cancel, report)?;
            Ok((trim, plan.reencode.is_some(), size))
        }
        ExportFormat::Gif { fps, width, loop_forever } => {
            let trim = clamp_trim(options.trim, info.duration_ms);
            let size = gif_size(info.size, *width);
            write_gif(source, part, trim, (*fps).clamp(1, 50), size, *loop_forever, cancel, report)?;
            Ok((trim, true, size))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_gif(
    source: &Path,
    part: &Path,
    trim: TrimRange,
    fps: u32,
    size: PhysicalSize,
    loop_forever: bool,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(f64),
) -> Result<()> {
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(part)?;
    let mut gif = GifEncoder::new(std::io::BufWriter::new(file), size.width, size.height, loop_forever)?;
    let length_ns = (trim.end_ms - trim.start_ms) * 1_000_000;
    let step = 1_000_000_000 / fps as u64;
    let mut next = 0u64;
    crate::platform::decode_video(source, trim, size, cancel, &mut |rgba: &[u8], at_ns: u64| {
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
