//! The system's side of recording: the writer, reading recorded files back, the devices and the
//! Trash. macOS uses AVFoundation, Windows Media Foundation and Linux GStreamer. Each platform's
//! module offers the same functions (`writer::create`, `reader::*`, `devices::list`, `trash`),
//! and this file hands every call to the one the app was built for.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use glimpsfy_capture::{Frame, HostTime, PhysicalSize, VideoFrame};

use crate::export::Mp4Plan;
use crate::session::{TakeWriter, WriterSpec};
use crate::{MediaInfo, RecordingDevices, Result, TrimRange};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as sys;
#[cfg(target_os = "macos")]
use macos as sys;
#[cfg(target_os = "windows")]
use windows as sys;

pub fn host_now() -> HostTime {
    glimpsfy_capture::host_now()
}

/// The writer of one take.
pub fn create_writer(spec: &WriterSpec) -> Result<Box<dyn TakeWriter>> {
    sys::writer::create(spec)
}

/// Moves `path` to the Trash.
pub fn trash(path: &Path) -> std::io::Result<()> {
    sys::trash(path)
}

/// What a recorded file holds.
pub fn inspect(path: &Path) -> Result<MediaInfo> {
    sys::reader::inspect(path)
}

/// Writes the MP4 of `plan` from `source` into `dest` and returns the part really written.
pub fn write_mp4(
    source: &Path,
    dest: &Path,
    plan: &Mp4Plan,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<TrimRange> {
    sys::reader::write_mp4(source, dest, plan, cancel, progress)
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
    sys::reader::decode_video(source, trim, size, cancel, each)
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
    sys::reader::frame_at(source, at_ms, max_height)
}

/// The peak level of the first audio track in `bins` equal parts, 0 to 1.
pub fn waveform(source: &Path, bins: usize) -> Result<Vec<f32>> {
    sys::reader::waveform(source, bins)
}

/// The presentation time of every video sample of `source`, in file order, in nanoseconds; the
/// tests read a recording's timing with it.
pub fn video_timestamps(source: &Path) -> Result<Vec<u64>> {
    sys::reader::video_timestamps(source)
}

/// The time of every keyframe of `source`, in nanoseconds.
pub fn keyframe_times(source: &Path) -> Result<Vec<u64>> {
    sys::reader::keyframe_times(source)
}

pub fn devices() -> RecordingDevices {
    sys::devices::list()
}
