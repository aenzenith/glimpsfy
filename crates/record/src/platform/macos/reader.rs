//! Reading recordings back with `AVAssetReader`: what a file holds, its frames for the GIF
//! export and the trim timeline, its waveform, the MP4 export (pass-through or re-encoded), and
//! the decoded pictures and sound an export with cuts (and the speech recognizer) reads.
//!
//! The synchronous `AVAsset` accessors are deprecated in favour of async loaders, but they still
//! load on first use; every caller here already runs on a worker thread, so blocking is fine.
#![allow(deprecated)]

use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use glimpsfy_capture::{
    ColorInfo, ColorSpace, Frame, HostTime, MacPixelBuffer, NativeBuffer, PhysicalSize, PixelFormat, ScaleFactor,
    StreamPixelFormat, VideoBuffer, VideoFrame,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::Message;
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVAssetWriter, AVAssetWriterInput,
    AVAssetWriterStatus, AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo, AVURLAsset, AVVideoAverageBitRateKey,
    AVVideoCodecKey, AVVideoCodecTypeH264, AVVideoCompressionPropertiesKey, AVVideoHeightKey,
    AVVideoMaxKeyFrameIntervalKey, AVVideoProfileLevelH264HighAutoLevel, AVVideoProfileLevelKey, AVVideoScalingModeKey,
    AVVideoScalingModeResizeAspect, AVVideoWidthKey,
};
use objc2_core_foundation::CFString;
use objc2_core_media::{kCMSampleAttachmentKey_NotSync, CMFormatDescription, CMSampleBuffer, CMTimeRange};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use super::writer::track_title;
use super::{any, boolean, constant, describe, dict, file_url, int, nanos, time, FORMAT_LPCM};
use crate::export::Mp4Plan;
use crate::{MediaInfo, RecordError, Result, TrackKind, TrimRange};

const SCALE: i32 = 90_000;

struct Asset {
    asset: Retained<AVURLAsset>,
    video: Retained<AVAssetTrack>,
    audio: Vec<Retained<AVAssetTrack>>,
}

fn open(path: &Path) -> Result<Asset> {
    if !path.is_file() {
        return Err(RecordError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} does not exist", path.display()),
        )));
    }
    let url = file_url(path)?;
    // SAFETY: creating an asset and listing its tracks; both block until loaded, off the main
    // thread.
    unsafe {
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        let video_type = constant(AVMediaTypeVideo, "AVMediaTypeVideo")?;
        let audio_type = constant(AVMediaTypeAudio, "AVMediaTypeAudio")?;
        let video = asset
            .tracksWithMediaType(video_type)
            .firstObject()
            .ok_or_else(|| RecordError::Encoder(format!("{} has no video track", path.display())))?;
        let audio = asset.tracksWithMediaType(audio_type).to_vec();
        Ok(Asset { asset, video, audio })
    }
}

fn four_cc(code: u32) -> String {
    code.to_be_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

fn format_of(track: &AVAssetTrack) -> Option<Retained<CMFormatDescription>> {
    // SAFETY: the array holds `CMFormatDescription`s, which are Core Foundation objects that
    // Objective-C can retain.
    unsafe {
        let formats = track.formatDescriptions();
        let first: Retained<AnyObject> = formats.firstObject()?;
        Some(Retained::cast_unchecked(first))
    }
}

fn title_of(track: &AVAssetTrack) -> Option<String> {
    // SAFETY: reading a track's metadata items and their string values.
    unsafe { track.metadata().iter().find_map(|item| item.stringValue().map(|s| s.to_string())) }
}

/// The kinds of the audio tracks, from the titles the writer gave them, or by their count for a
/// file whose titles were lost (the writer's order: the mix first, then microphone, then system).
fn audio_kinds(tracks: &[Retained<AVAssetTrack>]) -> Vec<TrackKind> {
    let by_title: Vec<Option<TrackKind>> = tracks
        .iter()
        .map(|t| {
            let title = title_of(t)?;
            [TrackKind::MixedAudio, TrackKind::Microphone, TrackKind::SystemAudio]
                .into_iter()
                .find(|&k| track_title(k) == title)
        })
        .collect();
    if by_title.iter().all(Option::is_some) {
        return by_title.into_iter().flatten().collect();
    }
    match tracks.len() {
        0 => vec![],
        3 => vec![TrackKind::MixedAudio, TrackKind::Microphone, TrackKind::SystemAudio],
        n => vec![TrackKind::MixedAudio; n],
    }
}

pub fn inspect(path: &Path) -> Result<MediaInfo> {
    let asset = open(path)?;
    // SAFETY: plain reads of loaded asset and track properties.
    unsafe {
        let duration_ms = nanos(asset.asset.duration()) / 1_000_000;
        let natural = asset.video.naturalSize();
        let codec = format_of(&asset.video).map(|f| four_cc(f.media_sub_type())).unwrap_or_default();
        Ok(MediaInfo {
            duration_ms,
            size: PhysicalSize { width: natural.width.round() as u32, height: natural.height.round() as u32 },
            fps: asset.video.nominalFrameRate() as f64,
            video_codec: codec,
            audio: audio_kinds(&asset.audio),
            file_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        })
    }
}

/// The presentation times of every video sample, in file order, read without decoding. Tests
/// check a recording's timing with it.
pub fn video_timestamps(path: &Path) -> Result<Vec<u64>> {
    let asset = open(path)?;
    // SAFETY: a pass-through reader over the video track; every sample buffer is only read.
    unsafe {
        let reader =
            AVAssetReader::assetReaderWithAsset_error(&asset.asset).map_err(|e| RecordError::Encoder(describe(&e)))?;
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&asset.video, None);
        output.setAlwaysCopiesSampleData(false);
        reader.addOutput(&output);
        if !reader.startReading() {
            return Err(reader_error(&reader));
        }
        let mut times = Vec::new();
        while let Some(sample) = output.copyNextSampleBuffer() {
            if sample.num_samples() > 0 {
                times.push(nanos(sample.presentation_time_stamp()));
            }
        }
        if reader.status() == AVAssetReaderStatus::Failed {
            return Err(reader_error(&reader));
        }
        Ok(times)
    }
}

fn reader_error(reader: &AVAssetReader) -> RecordError {
    // SAFETY: reading the reader's error.
    let reason = unsafe { reader.error() }.map(|e| describe(&e)).unwrap_or_else(|| "the file could not be read".into());
    RecordError::Encoder(reason)
}

fn range(start_ns: u64, end_ns: u64) -> CMTimeRange {
    CMTimeRange { start: time(start_ns, SCALE), duration: time(end_ns.saturating_sub(start_ns), SCALE) }
}

fn pixel_settings(format: u32) -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: a CFString key is toll-free bridged to NSString.
    let key: &NSString = unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const CFString as *const NSString) };
    dict(&[(key, int(format as i64))])
}

/// Reads the BGRA frame of a decoded sample into a `Frame`.
fn frame_of(sample: &CMSampleBuffer) -> Option<Frame> {
    // SAFETY: the image buffer is locked read-only while its rows are copied, and every row read
    // stays within `bytes_per_row × height`.
    unsafe {
        let image = sample.image_buffer()?;
        let buffer: &CVPixelBuffer = &image;
        CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        let width = CVPixelBufferGetWidth(buffer);
        let height = CVPixelBufferGetHeight(buffer);
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer) as *const u8;
        let data = (!base.is_null()).then(|| std::slice::from_raw_parts(base, stride * height).to_vec());
        CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        Frame::new(
            PhysicalSize { width: width as u32, height: height as u32 },
            stride,
            PixelFormat::Bgra8,
            ColorInfo::srgb(),
            ScaleFactor(1.0),
            data?,
        )
        .ok()
    }
}

/// Packed RGBA of `frame` at `size`.
fn rgba(frame: &Frame, size: PhysicalSize) -> Vec<u8> {
    let scaled;
    let frame = if frame.size == size {
        frame
    } else {
        scaled = frame.resized(size);
        &scaled
    };
    let (w, h) = (frame.size.width as usize, frame.size.height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &frame.data[y * frame.bytes_per_row..y * frame.bytes_per_row + w * 4];
        for p in row.chunks_exact(4) {
            match frame.format {
                PixelFormat::Bgra8 => out.extend_from_slice(&[p[2], p[1], p[0], 255]),
                PixelFormat::Rgba8 => out.extend_from_slice(&[p[0], p[1], p[2], 255]),
            }
        }
    }
    out
}

/// Decodes the frames shown within `trim`, scaled to `size`. A recording has a variable frame
/// rate (a still screen sends no frames), so the frame on screen at the trim's start may come
/// from before it: decoding starts at the keyframe before the start, and the last frame before
/// the start is handed out at time 0.
pub fn decode_video(
    source: &Path,
    trim: TrimRange,
    size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[u8], u64) -> Result<()>,
) -> Result<()> {
    let asset = open(source)?;
    let start = trim.start_ms * 1_000_000;
    let end = (trim.end_ms * 1_000_000).max(start + 1_000_000);
    let from = keyframe_before(&asset, start)?.unwrap_or(0);
    // SAFETY: a decoding reader over the video track from the keyframe; buffers are only read.
    unsafe {
        let reader =
            AVAssetReader::assetReaderWithAsset_error(&asset.asset).map_err(|e| RecordError::Encoder(describe(&e)))?;
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &asset.video,
            Some(&pixel_settings(kCVPixelFormatType_32BGRA)),
        );
        output.setAlwaysCopiesSampleData(false);
        reader.addOutput(&output);
        reader.setTimeRange(range(from, end));
        if !reader.startReading() {
            return Err(reader_error(&reader));
        }
        let mut held: Option<Frame> = None;
        let mut emit = |frame: &Frame, at: u64| -> Result<()> { each(&rgba(frame, size), at) };
        while let Some(sample) = output.copyNextSampleBuffer() {
            if cancel.load(Ordering::SeqCst) {
                reader.cancelReading();
                return Err(RecordError::Cancelled);
            }
            let at = nanos(sample.presentation_time_stamp());
            if at >= end {
                break;
            }
            let Some(frame) = frame_of(&sample) else { continue };
            if at < start {
                held = Some(frame);
                continue;
            }
            if let Some(first) = held.take() {
                if at > start {
                    if let Err(e) = emit(&first, 0) {
                        reader.cancelReading();
                        return Err(e);
                    }
                }
            }
            if let Err(e) = emit(&frame, at - start) {
                reader.cancelReading();
                return Err(e);
            }
        }
        if reader.status() == AVAssetReaderStatus::Failed {
            return Err(reader_error(&reader));
        }
        if let Some(first) = held {
            emit(&first, 0)?;
        }
    }
    Ok(())
}

pub fn frame_at(source: &Path, at_ms: u64, max_height: u32) -> Result<Frame> {
    let info = inspect(source)?;
    let at_ms = at_ms.min(info.duration_ms.saturating_sub(1));
    let height = max_height.clamp(16, info.size.height.max(16));
    let width = ((info.size.width as f64 * height as f64 / info.size.height.max(1) as f64).round() as u32).max(1);
    let size = PhysicalSize { width, height };
    let mut picture = None;
    let cancel = AtomicBool::new(false);
    let trim = TrimRange { start_ms: at_ms, end_ms: at_ms + 1 };
    let result = decode_video(source, trim, size, &cancel, &mut |rgba, _| {
        if picture.is_none() {
            picture = Some(rgba.to_vec());
            cancel.store(true, Ordering::SeqCst);
        }
        Ok(())
    });
    match (picture, result) {
        (Some(data), _) => {
            Frame::new(size, width as usize * 4, PixelFormat::Rgba8, ColorInfo::srgb(), ScaleFactor(1.0), data)
                .map_err(RecordError::Capture)
        }
        (None, Err(e)) => Err(e),
        (None, Ok(())) => Err(RecordError::Encoder(format!("{} has no frame at {at_ms} ms", source.display()))),
    }
}

fn pcm_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    dict(&[
        (&NSString::from_str("AVFormatIDKey"), int(FORMAT_LPCM as i64)),
        (&NSString::from_str("AVLinearPCMBitDepthKey"), int(32)),
        (&NSString::from_str("AVLinearPCMIsFloatKey"), boolean(true)),
        (&NSString::from_str("AVLinearPCMIsNonInterleaved"), boolean(false)),
        (&NSString::from_str("AVLinearPCMIsBigEndianKey"), boolean(false)),
        (&NSString::from_str("AVNumberOfChannelsKey"), int(2)),
        (&NSString::from_str("AVSampleRateKey"), int(crate::audio::RATE as i64)),
    ])
}

fn samples_of(sample: &CMSampleBuffer) -> Vec<f32> {
    // SAFETY: the block buffer holds `data_length` bytes of 32-bit float PCM, copied out.
    unsafe {
        let Some(block) = sample.data_buffer() else { return vec![] };
        let length = block.data_length();
        let mut bytes = vec![0u8; length];
        let Some(dest) = NonNull::new(bytes.as_mut_ptr().cast()) else { return vec![] };
        if length == 0 || block.copy_data_bytes(0, length, dest) != 0 {
            return vec![];
        }
        bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
    }
}

pub fn waveform(source: &Path, bins: usize) -> Result<Vec<f32>> {
    let bins = bins.clamp(1, 4096);
    let asset = open(source)?;
    let Some(track) = asset.audio.first() else { return Ok(vec![0.0; bins]) };
    // SAFETY: a decoding reader over the first audio track; buffers are only read.
    unsafe {
        let duration = nanos(asset.asset.duration()).max(1);
        let reader =
            AVAssetReader::assetReaderWithAsset_error(&asset.asset).map_err(|e| RecordError::Encoder(describe(&e)))?;
        let output =
            AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(track, Some(&pcm_settings()));
        reader.addOutput(&output);
        if !reader.startReading() {
            return Err(reader_error(&reader));
        }
        let mut peaks = vec![0f32; bins];
        while let Some(sample) = output.copyNextSampleBuffer() {
            let start = nanos(sample.presentation_time_stamp());
            for (i, frame) in samples_of(&sample).chunks_exact(2).enumerate() {
                let at = start + crate::audio::ns_at(i as u64);
                let bin = ((at as u128 * bins as u128 / duration as u128) as usize).min(bins - 1);
                peaks[bin] = peaks[bin].max(frame[0].abs().max(frame[1].abs()));
            }
        }
        Ok(peaks.into_iter().map(|p| p.min(1.0)).collect())
    }
}

/// Decodes audio track `track` within `span` to 48 kHz stereo float (the reader converts the
/// rate and the channels), buffer by buffer, each with its position from the span's start.
pub fn decode_audio(
    source: &Path,
    track: usize,
    span: TrimRange,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[f32], u64) -> Result<()>,
) -> Result<()> {
    let asset = open(source)?;
    let Some(track) = asset.audio.get(track) else {
        return Err(RecordError::Encoder(format!("{} has no audio track {track}", source.display())));
    };
    // SAFETY: reading the loaded duration.
    let duration = unsafe { nanos(asset.asset.duration()) };
    let start = span.start_ms.saturating_mul(1_000_000);
    let end = span.end_ms.saturating_mul(1_000_000).min(duration);
    if end <= start {
        return Ok(());
    }
    let mut placed = crate::audio::SpanAudio::new(start, end);
    // SAFETY: a decoding reader over one audio track within the span; buffers are only read.
    unsafe {
        let reader =
            AVAssetReader::assetReaderWithAsset_error(&asset.asset).map_err(|e| RecordError::Encoder(describe(&e)))?;
        let output =
            AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(track, Some(&pcm_settings()));
        reader.addOutput(&output);
        reader.setTimeRange(range(start, end));
        if !reader.startReading() {
            return Err(reader_error(&reader));
        }
        while let Some(sample) = output.copyNextSampleBuffer() {
            if cancel.load(Ordering::SeqCst) {
                reader.cancelReading();
                return Err(RecordError::Cancelled);
            }
            let samples = samples_of(&sample);
            if let Some((position, body)) = placed.place(nanos(sample.presentation_time_stamp()), &samples) {
                if let Err(e) = each(body, position) {
                    reader.cancelReading();
                    return Err(e);
                }
            }
            if placed.done() {
                reader.cancelReading();
                return Ok(());
            }
        }
        if reader.status() == AVAssetReaderStatus::Failed {
            return Err(reader_error(&reader));
        }
    }
    Ok(())
}

/// The decoded picture of a sample as a frame the take writer takes without a copy.
fn native_frame(sample: &CMSampleBuffer, at: u64) -> Option<VideoFrame> {
    // SAFETY: reading the image buffer of a decoded sample and its size; the buffer is retained
    // by the frame and only read from here on.
    unsafe {
        let image = sample.image_buffer()?;
        let size =
            PhysicalSize { width: CVPixelBufferGetWidth(&image) as u32, height: CVPixelBufferGetHeight(&image) as u32 };
        Some(VideoFrame {
            timestamp: HostTime(at),
            size,
            format: StreamPixelFormat::Nv12,
            color_space: ColorSpace::Srgb,
            buffer: VideoBuffer::Native(NativeBuffer(Box::new(MacPixelBuffer(image)))),
        })
    }
}

/// Decodes the frames shown within `span` as the decoder's own NV12 pixel buffers at the file's
/// size, which the take writer scales to its own size; `_size` is the size the other platforms
/// scale to. The frame on screen at the span's start goes out at time 0 when it comes from
/// before the span, as in `decode_video`.
pub fn decode_frames(
    source: &Path,
    span: TrimRange,
    _size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(VideoFrame, u64) -> Result<()>,
) -> Result<()> {
    let asset = open(source)?;
    let start = span.start_ms * 1_000_000;
    let end = (span.end_ms * 1_000_000).max(start + 1_000_000);
    let from = keyframe_before(&asset, start)?.unwrap_or(0);
    // SAFETY: a decoding reader over the video track from the keyframe; buffers are only read.
    unsafe {
        let reader =
            AVAssetReader::assetReaderWithAsset_error(&asset.asset).map_err(|e| RecordError::Encoder(describe(&e)))?;
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &asset.video,
            Some(&pixel_settings(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)),
        );
        output.setAlwaysCopiesSampleData(false);
        reader.addOutput(&output);
        reader.setTimeRange(range(from, end));
        if !reader.startReading() {
            return Err(reader_error(&reader));
        }
        // The sample before the start is held, not its frame: most of them are passed over.
        let mut held: Option<Retained<CMSampleBuffer>> = None;
        let mut emit = |sample: &CMSampleBuffer, at: u64| -> Result<()> {
            match native_frame(sample, at) {
                Some(frame) => each(frame, at),
                None => Ok(()),
            }
        };
        while let Some(sample) = output.copyNextSampleBuffer() {
            if cancel.load(Ordering::SeqCst) {
                reader.cancelReading();
                return Err(RecordError::Cancelled);
            }
            if sample.num_samples() == 0 || sample.image_buffer().is_none() {
                continue;
            }
            let at = nanos(sample.presentation_time_stamp());
            if at >= end {
                break;
            }
            if at < start {
                held = Some(sample);
                continue;
            }
            if let Some(first) = held.take() {
                if at > start {
                    if let Err(e) = emit(&first, 0) {
                        reader.cancelReading();
                        return Err(e);
                    }
                }
            }
            if let Err(e) = emit(&sample, at - start) {
                reader.cancelReading();
                return Err(e);
            }
        }
        if reader.status() == AVAssetReaderStatus::Failed {
            return Err(reader_error(&reader));
        }
        if let Some(first) = held {
            emit(&first, 0)?;
        }
    }
    Ok(())
}

fn is_keyframe(sample: &CMSampleBuffer) -> bool {
    // SAFETY: the attachments array holds one CFDictionary per sample, toll-free bridged to
    // NSDictionary; the NotSync key is a CFString bridged to NSString.
    unsafe {
        let Some(attachments) = sample.sample_attachments_array(false) else { return true };
        let list: &NSArray<NSDictionary<NSString, AnyObject>> = &*(&*attachments
            as *const objc2_core_foundation::CFArray
            as *const NSArray<NSDictionary<NSString, AnyObject>>);
        let Some(first) = list.firstObject() else { return true };
        let key: &NSString = &*(kCMSampleAttachmentKey_NotSync as *const CFString as *const NSString);
        match first.objectForKey(key) {
            None => true,
            Some(value) => !value.downcast::<NSNumber>().map(|n| n.boolValue()).unwrap_or(false),
        }
    }
}

/// The time of every keyframe of the video within `[from_ns, to_ns]`, read without decoding.
fn keyframes(asset: &Asset, from_ns: u64, to_ns: u64) -> Result<Vec<u64>> {
    // SAFETY: a pass-through reader whose samples are only inspected.
    unsafe {
        let reader =
            AVAssetReader::assetReaderWithAsset_error(&asset.asset).map_err(|e| RecordError::Encoder(describe(&e)))?;
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&asset.video, None);
        output.setAlwaysCopiesSampleData(false);
        reader.addOutput(&output);
        reader.setTimeRange(range(from_ns, to_ns + 1_000_000));
        if !reader.startReading() {
            return Err(reader_error(&reader));
        }
        let mut times = Vec::new();
        while let Some(sample) = output.copyNextSampleBuffer() {
            let at = nanos(sample.presentation_time_stamp());
            if at > to_ns {
                reader.cancelReading();
                break;
            }
            if sample.num_samples() > 0 && is_keyframe(&sample) {
                times.push(at);
            }
        }
        Ok(times)
    }
}

/// The keyframes of `path` in nanoseconds; the tests check the pass-through trim with it.
pub fn keyframe_times(path: &Path) -> Result<Vec<u64>> {
    let asset = open(path)?;
    // SAFETY: reading the loaded duration.
    let duration = unsafe { nanos(asset.asset.duration()) };
    keyframes(&asset, 0, duration)
}

/// The keyframe at or before `start_ns`, where a pass-through cut or a decode has to start,
/// since frames after a keyframe only decode from it. The recorder writes one at least every
/// second while frames come, so ten seconds back holds one unless the screen was still; then
/// the search goes back to the start of the file. `None` when the file has no frame before.
fn keyframe_before(asset: &Asset, start_ns: u64) -> Result<Option<u64>> {
    if start_ns == 0 {
        return Ok(Some(0));
    }
    let near = start_ns.saturating_sub(10_000_000_000);
    let mut found = keyframes(asset, near, start_ns)?.into_iter().filter(|&t| t <= start_ns).max();
    if found.is_none() && near > 0 {
        found = keyframes(asset, 0, start_ns)?.into_iter().filter(|&t| t <= start_ns).max();
    }
    Ok(found)
}

/// Removes what the writer leaves next to `dest` when it moves the index to the front
/// (`<name>.sb-…`, the file before the move), on success, failure and cancel alike.
struct Leftovers<'a>(&'a Path);

impl Drop for Leftovers<'_> {
    fn drop(&mut self) {
        let (Some(dir), Some(name)) = (self.0.parent(), self.0.file_name()) else { return };
        let prefix = format!("{}.sb-", name.to_string_lossy());
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

struct Lane {
    output: Retained<AVAssetReaderTrackOutput>,
    input: Retained<AVAssetWriterInput>,
    done: bool,
    video: bool,
    /// The video sample read last, appended when the next one comes or, stretched to the cut's
    /// end, when the track ends (`stretched`).
    held: Option<Retained<CMSampleBuffer>>,
}

/// The last video sample of a cut, lasting until the cut's end. A still screen sends no frames,
/// so the last one before the end can be seconds early, and the track would stop there while the
/// sound goes on; `endSessionAtSourceTime` does not stretch it. `None` when it already lasts.
fn stretched(sample: &CMSampleBuffer, end_ns: u64) -> Option<Retained<CMSampleBuffer>> {
    // SAFETY: reads the timing of a valid sample and copies it with one new timing entry.
    unsafe {
        let mut timing = std::mem::zeroed::<objc2_core_media::CMSampleTimingInfo>();
        if sample.sample_timing_info(0, NonNull::from(&mut timing)) != 0 {
            return None;
        }
        let at = nanos(timing.presentationTimeStamp);
        let lasts = at + nanos(timing.duration);
        if end_ns <= lasts || end_ns <= at {
            return None;
        }
        timing.duration = time(end_ns - at, SCALE);
        let mut copy: *mut CMSampleBuffer = std::ptr::null_mut();
        let status = CMSampleBuffer::create_copy_with_new_timing(None, sample, 1, &timing, NonNull::from(&mut copy));
        if status != 0 {
            return None;
        }
        Retained::from_raw(copy)
    }
}

pub fn write_mp4(
    source: &Path,
    dest: &Path,
    plan: &Mp4Plan,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<TrimRange> {
    let asset = open(source)?;
    let wanted_start = plan.trim.start_ms * 1_000_000;
    let end = plan.trim.end_ms * 1_000_000;
    // A re-encode cuts exactly, even on a still screen that sent no frame at the start: a decoding
    // reader hands out the picture on screen at its range's start, stamped with that start.
    let start =
        if plan.reencode.is_none() { keyframe_before(&asset, wanted_start)?.unwrap_or(0) } else { wanted_start };
    let url = file_url(dest)?;
    let _leftovers = Leftovers(dest);
    // SAFETY: the AVFoundation calls follow the documented order (configure reader and writer,
    // start both, append while ready, finish), and every sample buffer is appended once.
    unsafe {
        let reader =
            AVAssetReader::assetReaderWithAsset_error(&asset.asset).map_err(|e| RecordError::Encoder(describe(&e)))?;
        reader.setTimeRange(range(start, end));
        let writer =
            AVAssetWriter::assetWriterWithURL_fileType_error(&url, constant(AVFileTypeMPEG4, "AVFileTypeMPEG4")?)
                .map_err(|e| RecordError::Encoder(describe(&e)))?;
        // The moov box goes to the front, so a browser starts playing before the whole file
        // has arrived.
        writer.setShouldOptimizeForNetworkUse(true);

        let video_type = constant(AVMediaTypeVideo, "AVMediaTypeVideo")?;
        let audio_type = constant(AVMediaTypeAudio, "AVMediaTypeAudio")?;
        let mut lanes = Vec::new();
        let (video_output, video_input) = match plan.reencode {
            None => {
                let output =
                    AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&asset.video, None);
                let hint = format_of(&asset.video);
                let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(
                    video_type,
                    None,
                    hint.as_deref(),
                );
                (output, input)
            }
            Some(re) => {
                let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                    &asset.video,
                    Some(&pixel_settings(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)),
                );
                // The nominal rate of a variable-rate recording is low; a keyframe every two
                // seconds of 30 frames keeps seeking quick.
                let fps = (asset.video.nominalFrameRate().round() as i64).max(30);
                let compression = dict(&[
                    (constant(AVVideoAverageBitRateKey, "AVVideoAverageBitRateKey")?, int(re.bits_per_second as i64)),
                    (constant(AVVideoMaxKeyFrameIntervalKey, "AVVideoMaxKeyFrameIntervalKey")?, int(fps * 2)),
                    (
                        constant(AVVideoProfileLevelKey, "AVVideoProfileLevelKey")?,
                        any(constant(AVVideoProfileLevelH264HighAutoLevel, "H.264 High")?.retain()),
                    ),
                ]);
                let settings = dict(&[
                    (
                        constant(AVVideoCodecKey, "AVVideoCodecKey")?,
                        any(constant(AVVideoCodecTypeH264, "H.264")?.retain()),
                    ),
                    (constant(AVVideoWidthKey, "AVVideoWidthKey")?, int(re.size.width as i64)),
                    (constant(AVVideoHeightKey, "AVVideoHeightKey")?, int(re.size.height as i64)),
                    (
                        constant(AVVideoScalingModeKey, "AVVideoScalingModeKey")?,
                        any(constant(AVVideoScalingModeResizeAspect, "resize aspect")?.retain()),
                    ),
                    (constant(AVVideoCompressionPropertiesKey, "AVVideoCompressionPropertiesKey")?, any(compression)),
                ]);
                let input =
                    AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(video_type, Some(&settings));
                (output, input)
            }
        };
        video_output.setAlwaysCopiesSampleData(false);
        if !reader.canAddOutput(&video_output) || !writer.canAddInput(&video_input) {
            return Err(RecordError::Encoder("the video track could not be exported".into()));
        }
        reader.addOutput(&video_output);
        writer.addInput(&video_input);
        lanes.push(Lane { output: video_output, input: video_input, done: false, video: true, held: None });

        if let Some(track) = plan.audio_track.and_then(|i| asset.audio.get(i)) {
            let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(track, None);
            output.setAlwaysCopiesSampleData(false);
            let hint = format_of(track);
            let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(
                audio_type,
                None,
                hint.as_deref(),
            );
            if reader.canAddOutput(&output) && writer.canAddInput(&input) {
                reader.addOutput(&output);
                writer.addInput(&input);
                lanes.push(Lane { output, input, done: false, video: false, held: None });
            }
        }

        if !reader.startReading() {
            return Err(reader_error(&reader));
        }
        if !writer.startWriting() {
            reader.cancelReading();
            let reason = writer.error().map(|e| describe(&e)).unwrap_or_else(|| "unknown".into());
            return Err(RecordError::Encoder(format!("the export could not start: {reason}")));
        }
        writer.startSessionAtSourceTime(time(start, SCALE));
        let length = end.saturating_sub(start).max(1);
        while lanes.iter().any(|l| !l.done) {
            if cancel.load(Ordering::SeqCst) {
                reader.cancelReading();
                writer.cancelWriting();
                return Err(RecordError::Cancelled);
            }
            let mut moved = false;
            for lane in lanes.iter_mut().filter(|l| !l.done) {
                while lane.input.isReadyForMoreMediaData() {
                    match lane.output.copyNextSampleBuffer() {
                        Some(sample) => {
                            if lane.video {
                                let at = nanos(sample.presentation_time_stamp());
                                // A pass-through reader hands out whole groups of pictures; the
                                // frames outside the cut are not needed (the recording has no
                                // B-frames, and the cut starts on a keyframe).
                                if at < start || at >= end {
                                    continue;
                                }
                                progress(at.saturating_sub(start) as f64 / length as f64);
                            }
                            let sample = if lane.video {
                                match lane.held.replace(sample) {
                                    Some(previous) => previous,
                                    None => continue,
                                }
                            } else {
                                sample
                            };
                            if !lane.input.appendSampleBuffer(&sample) {
                                reader.cancelReading();
                                let reason = writer
                                    .error()
                                    .map(|e| describe(&e))
                                    .unwrap_or_else(|| "a sample was refused".into());
                                writer.cancelWriting();
                                return Err(RecordError::Encoder(reason));
                            }
                            moved = true;
                        }
                        None => {
                            if let Some(last) = lane.held.take() {
                                let last = stretched(&last, end).unwrap_or(last);
                                if !lane.input.appendSampleBuffer(&last) {
                                    reader.cancelReading();
                                    let reason = writer
                                        .error()
                                        .map(|e| describe(&e))
                                        .unwrap_or_else(|| "a sample was refused".into());
                                    writer.cancelWriting();
                                    return Err(RecordError::Encoder(reason));
                                }
                            }
                            lane.input.markAsFinished();
                            lane.done = true;
                            break;
                        }
                    }
                }
            }
            if !moved {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if reader.status() == AVAssetReaderStatus::Failed {
            writer.cancelWriting();
            return Err(reader_error(&reader));
        }
        writer.endSessionAtSourceTime(time(end, SCALE));
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let handler = RcBlock::new(move || {
            let _ = tx.try_send(());
        });
        writer.finishWritingWithCompletionHandler(&handler);
        if rx.recv_timeout(Duration::from_secs(120)).is_err() {
            return Err(RecordError::Encoder("the export did not finish within two minutes".into()));
        }
        if writer.status() != AVAssetWriterStatus::Completed {
            let reason = writer.error().map(|e| describe(&e)).unwrap_or_else(|| "the export failed".into());
            return Err(RecordError::Encoder(reason));
        }
    }
    Ok(TrimRange { start_ms: start / 1_000_000, end_ms: plan.trim.end_ms })
}
