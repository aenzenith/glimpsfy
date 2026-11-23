//! The system's side of recording: the writer, reading recorded files back, the devices and the
//! Trash. macOS uses AVFoundation; other platforms answer `NotImplemented` until their
//! milestone.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use glimpsfy_capture::{Frame, HostTime, PhysicalSize, VideoFrame};

use crate::export::Mp4Plan;
use crate::session::{TakeWriter, WriterSpec};
use crate::{MediaInfo, RecordingDevices, Result, TrimRange};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "macos"))]
use crate::RecordError;

pub fn host_now() -> HostTime {
    glimpsfy_capture::host_now()
}

/// The writer of one take.
pub fn create_writer(spec: &WriterSpec) -> Result<Box<dyn TakeWriter>> {
    #[cfg(target_os = "macos")]
    return macos::writer::create(spec);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = spec;
        Err(RecordError::NotImplemented("the recording writer on this platform"))
    }
}

/// Moves `path` to the Trash.
pub fn trash(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::trash(path);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "the Trash is not available on this platform"))
    }
}

/// What a recorded file holds.
pub fn inspect(path: &Path) -> Result<MediaInfo> {
    #[cfg(target_os = "macos")]
    return macos::reader::inspect(path);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(RecordError::NotImplemented("reading recordings on this platform"))
    }
}

/// Writes the MP4 of `plan` from `source` into `dest` and returns the part really written.
pub fn write_mp4(
    source: &Path,
    dest: &Path,
    plan: &Mp4Plan,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<TrimRange> {
    #[cfg(target_os = "macos")]
    return macos::reader::write_mp4(source, dest, plan, cancel, progress);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (source, dest, plan, cancel, progress);
        Err(RecordError::NotImplemented("MP4 export on this platform"))
    }
}

/// Decodes the video of `source` within `trim`, scaled to `size`, and hands each frame to
/// `each` as packed RGBA with its time from the trim's start.
pub fn decode_video(
    source: &Path,
    trim: TrimRange,
    size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[u8], u64) -> Result<()>,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    return macos::reader::decode_video(source, trim, size, cancel, each);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (source, trim, size, cancel, each);
        Err(RecordError::NotImplemented("decoding recordings on this platform"))
    }
}

/// Decodes the video of `source` within `span` into frames a writer of `size` takes as they are,
/// each with its time from the span's start: native `CVPixelBuffer`s at the file's size on macOS
/// (the writer scales them), RGBA frames at `size` elsewhere. As in [`decode_video`], the frame on
/// screen at the span's start goes out at time 0 when it comes from before the span.
pub fn decode_frames(
    source: &Path,
    span: TrimRange,
    size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(VideoFrame, u64) -> Result<()>,
) -> Result<()> {
    sys::reader::decode_frames(source, span, size, cancel, each)
}

/// Decodes audio track `track` of `source` (an index among its audio tracks, as in
/// [`MediaInfo::audio`]) within `span` to interleaved 48 kHz stereo float, and hands it to `each`
/// buffer by buffer with each buffer's frame position from the span's start. It streams, so a
/// whole recording can be read (a span reaching past the end reads to the end) without holding
/// it in memory; buffers that follow each other are contiguous.
pub fn decode_audio(
    source: &Path,
    track: usize,
    span: TrimRange,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[f32], u64) -> Result<()>,
) -> Result<()> {
    sys::reader::decode_audio(source, track, span, cancel, each)
}

/// The frame shown at `at_ms`, at most `max_height` pixels high, for the trim timeline and the
/// history's poster.
pub fn frame_at(source: &Path, at_ms: u64, max_height: u32) -> Result<Frame> {
    #[cfg(target_os = "macos")]
    return macos::reader::frame_at(source, at_ms, max_height);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (source, at_ms, max_height);
        Err(RecordError::NotImplemented("decoding recordings on this platform"))
    }
}

/// The peak level of the first audio track in `bins` equal parts, 0 to 1.
pub fn waveform(source: &Path, bins: usize) -> Result<Vec<f32>> {
    #[cfg(target_os = "macos")]
    return macos::reader::waveform(source, bins);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (source, bins);
        Err(RecordError::NotImplemented("decoding recordings on this platform"))
    }
}

/// The presentation time of every video sample of `source`, in file order, in nanoseconds; the
/// tests read a recording's timing with it.
#[cfg(target_os = "macos")]
pub fn video_timestamps(source: &Path) -> Result<Vec<u64>> {
    macos::reader::video_timestamps(source)
}

/// The time of every keyframe of `source`, in nanoseconds.
#[cfg(target_os = "macos")]
pub fn keyframe_times(source: &Path) -> Result<Vec<u64>> {
    macos::reader::keyframe_times(source)
}

pub fn devices() -> RecordingDevices {
    #[cfg(target_os = "macos")]
    return macos::devices::list();
    #[cfg(not(target_os = "macos"))]
    RecordingDevices {
        microphones: vec![],
        cameras: vec![],
        system_audio: false,
        microphone_permission: crate::PermissionState::NotRequired,
        camera_permission: crate::PermissionState::NotRequired,
    }
}
