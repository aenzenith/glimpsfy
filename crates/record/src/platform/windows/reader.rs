//! Reading recordings back on Windows with Media Foundation's source reader: what a file holds,
//! the frame for the trim timeline and the poster, decoded frames for the GIF, the waveform, the
//! sample times and key frames, the MP4 an export writes (passed through or re-encoded), and the
//! decoded pictures and sound an export with cuts (and the speech recognizer) reads.
//! Every call runs on a thread of its own inside the multithreaded apartment; the callbacks the
//! caller passes (the GIF encoder, the progress bar) stay on the caller's thread.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use glimpsfy_capture::{
    ColorInfo, ColorSpace, Frame, HostTime, PhysicalSize, PixelFormat, ScaleFactor, StreamPixelFormat, VideoBuffer,
    VideoFrame,
};
use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFByteStream, IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader,
    MFAudioFormat_Float, MFMediaType_Audio, MFMediaType_Video, MFSampleExtension_CleanPoint, MFVideoFormat_H264,
    MFVideoFormat_HEVC, MFVideoFormat_RGB32, MF_ACCESSMODE_READWRITE, MF_E_INVALIDSTREAMNUMBER,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_OPENMODE_FAIL_IF_EXIST, MF_PD_DURATION,
    MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_MEDIASOURCE,
};

use super::logic::{self, Pixels, HNS_PER_SECOND};
use super::mf::{self, Api, Locked, Locked2D, Platform, Variant};
use super::writer::{self as mp4, VideoSettings};
use crate::export::{Mp4Plan, Reencode};
use crate::session::codec_for;
use crate::{MediaInfo, RecordError, Result, TrackKind, TrimRange, VideoCodec};

/// `ReadSample`'s "any stream", which has the same value as "all streams".
const ANY_STREAM: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;

/// A stream of the file and the format it is stored in.
struct Stream {
    index: u32,
    native: IMFMediaType,
}

/// What one `ReadSample` gave.
enum Read {
    Sample {
        stream: u32,
        at: i64,
        sample: IMFSample,
        /// The decoder changed its output format; the layout has to be read again.
        changed: bool,
        /// The stream ended with this sample.
        last: bool,
    },
    /// No sample this time (a gap in a sparse stream).
    Gap,
    End {
        stream: u32,
    },
}

/// Where a decoded RGB32 picture's pixels are.
#[derive(Clone, Copy, Debug)]
struct Layout {
    width: u32,
    height: u32,
    stride: i32,
    /// The part to show: x, y, width, height.
    visible: (u32, u32, u32, u32),
}

/// An open recording.
struct Source<'a> {
    platform: &'a Platform,
    reader: IMFSourceReader,
    video: Option<Stream>,
    audio: Vec<Stream>,
    /// The file's length in 100 ns units.
    duration: i64,
}

impl<'a> Source<'a> {
    /// Opens `path`; with `decode`, the reader may put in the video processor that turns the
    /// decoder's YUV into RGB32.
    fn open(platform: &'a Platform, path: &Path, decode: bool) -> Result<Source<'a>> {
        let api = platform.api();
        let attributes = api.attributes(1)?;
        if decode {
            mf::set_u32(&attributes, &MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        }
        let reader = api.source_reader(path, &attributes)?;
        let mut video = None;
        let mut audio = Vec::new();
        for index in 0..64u32 {
            // SAFETY: the reader is live; an index past the last stream is an error, not a fault.
            match unsafe { reader.GetNativeMediaType(index, 0) } {
                Ok(native) => {
                    let major = mf::get_guid(&native, &MF_MT_MAJOR_TYPE);
                    if major == Some(MFMediaType_Video) && video.is_none() {
                        video = Some(Stream { index, native });
                    } else if major == Some(MFMediaType_Audio) {
                        audio.push(Stream { index, native });
                    }
                }
                Err(error) if error.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(_) => continue,
            }
        }
        // Each use selects the streams it reads; the reader starts with its own choice.
        // SAFETY: "all streams" is a valid index for this call.
        unsafe { reader.SetStreamSelection(ANY_STREAM, false) }
            .map_err(|e| mf::failed("choosing the streams to read", &e))?;
        // SAFETY: the media source attribute is read into a variant that the guard clears.
        let duration =
            unsafe { reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION) }
                .ok()
                .map(Variant)
                .and_then(|v| v.u64())
                .map_or(0, |d| d.min(i64::MAX as u64) as i64);
        Ok(Source { platform, reader, video, audio, duration })
    }

    fn api(&self) -> &'static Api {
        self.platform.api()
    }

    fn video(&self) -> Result<&Stream> {
        self.video.as_ref().ok_or_else(|| RecordError::Encoder("the recording has no video track".into()))
    }

    fn select(&self, stream: u32, on: bool) -> Result<()> {
        // SAFETY: the index is one the reader listed.
        unsafe { self.reader.SetStreamSelection(stream, on) }.map_err(|e| mf::failed("choosing a stream", &e))
    }

    /// Moves to `at` (100 ns units). The reader lands on the key frame at or before it, and
    /// reading goes on forward from there.
    fn seek(&self, at: i64) -> Result<()> {
        let position = mf::position(at.max(0));
        // SAFETY: GUID_NULL selects 100 ns units, the variant is a valid VT_I8, and no request is
        // pending (every read is synchronous).
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &position) }
            .map_err(|e| mf::failed("seeking in the recording", &e))
    }

    fn read(&self, stream: u32) -> Result<Read> {
        let (mut actual, mut flags, mut at) = (0u32, 0u32, 0i64);
        let mut sample: Option<IMFSample> = None;
        // SAFETY: synchronous mode; every out pointer is a live local the call writes once.
        unsafe {
            self.reader.ReadSample(
                stream,
                0,
                Some(&mut actual as *mut u32),
                Some(&mut flags as *mut u32),
                Some(&mut at as *mut i64),
                Some(&mut sample as *mut Option<IMFSample>),
            )
        }
        .map_err(|e| mf::failed("reading the recording", &e))?;
        if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            return Err(RecordError::Encoder("the recording could not be read to its end".into()));
        }
        let last = flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0;
        Ok(match sample {
            Some(sample) => Read::Sample {
                stream: actual,
                at,
                sample,
                changed: flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0,
                last,
            },
            None if last => Read::End { stream: actual },
            None => Read::Gap,
        })
    }

    /// Makes the reader decode `video` into RGB32 and returns the layout of its pictures.
    fn decode_as_rgb32(&self, video: &Stream) -> Result<Layout> {
        let wanted = self.api().media_type()?;
        mf::set_guid(&wanted, &MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mf::set_guid(&wanted, &MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        // SAFETY: the index is the reader's own, and the type is a live object the call reads.
        if let Err(error) = unsafe { self.reader.SetCurrentMediaType(video.index, None, &wanted) } {
            return Err(if mf::get_guid(&video.native, &MF_MT_SUBTYPE) == Some(MFVideoFormat_HEVC) {
                RecordError::Encoder(format!(
                    "this Windows cannot decode HEVC video; the HEVC Video Extensions add a decoder (0x{:08X})",
                    error.code().0 as u32
                ))
            } else {
                mf::failed("setting up the video decoder", &error)
            });
        }
        self.layout(video)
    }

    /// The layout of the pictures the reader hands out for `video` now.
    fn layout(&self, video: &Stream) -> Result<Layout> {
        // SAFETY: the index is the reader's own.
        let current = unsafe { self.reader.GetCurrentMediaType(video.index) }
            .map_err(|e| mf::failed("reading the decoded format", &e))?;
        let (width, height) =
            frame_size(&current).ok_or_else(|| RecordError::Encoder("the decoded video has no picture size".into()))?;
        let stride = mf::get_u32(&current, &MF_MT_DEFAULT_STRIDE)
            .map(|s| s as i32)
            .filter(|s| s.unsigned_abs() >= width * 4)
            .unwrap_or(width as i32 * 4);
        // A decoder may pad the picture to whole macroblocks; the aperture, or else the size the
        // file declares, says what to keep.
        let shown = aperture(&current).or_else(|| display_size(&video.native).map(|(w, h)| (0, 0, w as i32, h as i32)));
        Ok(Layout { width, height, stride, visible: logic::visible_rect(shown, width, height) })
    }
}

/// The picture size of a video type.
fn frame_size(media: &IMFMediaType) -> Option<(u32, u32)> {
    mf::get_u64(media, &MF_MT_FRAME_SIZE).map(logic::unpack).filter(|&(w, h)| w > 0 && h > 0)
}

/// `MF_MT_MINIMUM_DISPLAY_APERTURE` as x, y, width, height. The blob is an `MFVideoArea`: two
/// `MFOffset`s (a 16-bit fraction, then the 16-bit whole value) and a `SIZE` of two 32-bit ints.
fn aperture(media: &IMFMediaType) -> Option<(i32, i32, i32, i32)> {
    let blob = mf::get_blob(media, &MF_MT_MINIMUM_DISPLAY_APERTURE)?;
    let b = blob.get(..16)?;
    let x = i16::from_le_bytes([b[2], b[3]]) as i32;
    let y = i16::from_le_bytes([b[6], b[7]]) as i32;
    let w = i32::from_le_bytes([b[8], b[9], b[10], b[11]]);
    let h = i32::from_le_bytes([b[12], b[13], b[14], b[15]]);
    Some((x, y, w, h))
}

/// The size a video type is shown at.
fn display_size(media: &IMFMediaType) -> Option<(u32, u32)> {
    let (width, height) = frame_size(media)?;
    let (_, _, w, h) = logic::visible_rect(aperture(media), width, height);
    Some((w, h))
}

/// The audio tracks by their count, as the macOS reader names them when the titles are missing
/// (Media Foundation's MP4 sink writes none): three are the mix, the microphone and the system.
fn audio_kinds(count: usize) -> Vec<TrackKind> {
    match count {
        0 => vec![],
        3 => vec![TrackKind::MixedAudio, TrackKind::Microphone, TrackKind::SystemAudio],
        n => vec![TrackKind::MixedAudio; n],
    }
}

fn codec_name(subtype: Option<GUID>) -> String {
    match subtype {
        Some(s) if s == MFVideoFormat_H264 => "avc1".into(),
        Some(s) if s == MFVideoFormat_HEVC => "hvc1".into(),
        Some(s) => logic::fourcc(s.data1),
        None => String::new(),
    }
}

/// The visible part of a decoded RGB32 sample as tight RGBA rows with opaque alpha.
fn rgba(sample: &IMFSample, layout: &Layout) -> Result<Vec<u8>> {
    // SAFETY: the sample is live; the call returns its one buffer or a copy joining several.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|e| mf::failed("reading a frame", &e))?;
    let (width, height) = (layout.width as usize, layout.height as usize);
    let pixels = if let Ok(picture) = buffer.cast::<IMF2DBuffer>() {
        let locked = Locked2D::lock(&picture)?;
        let (memory, stride) = locked
            .rows(height, width * 4)
            .ok_or_else(|| RecordError::Encoder("a decoded frame had a pitch narrower than its rows".into()))?;
        Pixels { data: memory, stride, width, height, red_first: false }.to_rgba(layout.visible)
    } else {
        let locked = Locked::lock(&buffer)?;
        Pixels { data: locked.bytes(), stride: layout.stride as isize, width, height, red_first: false }
            .to_rgba(layout.visible)
    };
    pixels.ok_or_else(|| RecordError::Encoder("a decoded frame was smaller than its format says".into()))
}

/// Tight RGBA rows as a frame.
fn rgba_frame(data: Vec<u8>, width: u32, height: u32) -> Result<Frame> {
    let size = PhysicalSize { width, height };
    Ok(Frame::new(size, width as usize * 4, PixelFormat::Rgba8, ColorInfo::srgb(), ScaleFactor(1.0), data)?)
}

/// A decoded sample at `size`, as tight RGBA rows with opaque alpha.
fn scaled(sample: &IMFSample, layout: &Layout, size: PhysicalSize) -> Result<Vec<u8>> {
    let data = rgba(sample, layout)?;
    let (_, _, width, height) = layout.visible;
    if (width, height) == (size.width, size.height) {
        return Ok(data);
    }
    Ok(rgba_frame(data, width, height)?.resized(size).to_rgba())
}

pub fn inspect(path: &Path) -> Result<MediaInfo> {
    mf::on_thread("mp4-inspect", |platform| {
        let source = Source::open(platform, path, false)?;
        let video = source.video()?;
        let (width, height) = display_size(&video.native)
            .ok_or_else(|| RecordError::Encoder("the video track has no picture size".into()))?;
        let fps = mf::get_u64(&video.native, &MF_MT_FRAME_RATE)
            .map(logic::unpack)
            .filter(|&(_, d)| d > 0)
            .map_or(0.0, |(n, d)| n as f64 / d as f64);
        Ok(MediaInfo {
            duration_ms: (source.duration.max(0) / 10_000) as u64,
            size: PhysicalSize { width, height },
            fps,
            video_codec: codec_name(mf::get_guid(&video.native, &MF_MT_SUBTYPE)),
            audio: audio_kinds(source.audio.len()),
            file_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        })
    })
}

pub fn frame_at(source: &Path, at_ms: u64, max_height: u32) -> Result<Frame> {
    mf::on_thread("mp4-frame", |platform| {
        let file = Source::open(platform, source, true)?;
        let video = file.video()?;
        let mut layout = file.decode_as_rgb32(video)?;
        file.select(video.index, true)?;
        // Past the end, the last frame is shown.
        let duration_ms = (file.duration.max(0) / 10_000) as u64;
        let at = logic::hns(at_ms.min(duration_ms.saturating_sub(1)).saturating_mul(1_000_000));
        file.seek(at)?;
        let mut shown: Option<(IMFSample, Layout)> = None;
        loop {
            match file.read(video.index)? {
                Read::Sample { at: time, sample, changed, last, .. } => {
                    if changed {
                        layout = file.layout(video)?;
                    }
                    if time > at && shown.is_some() {
                        break;
                    }
                    shown = Some((sample, layout));
                    if time >= at || last {
                        break;
                    }
                }
                Read::Gap => {}
                Read::End { .. } => break,
            }
        }
        let (sample, layout) =
            shown.ok_or_else(|| RecordError::Encoder("the recording has no frame to show".into()))?;
        let (_, _, width, height) = layout.visible;
        let (out_width, out_height) = logic::poster_size(width, height, max_height);
        let frame = rgba_frame(rgba(&sample, &layout)?, width, height)?;
        Ok(if (out_width, out_height) == (width, height) {
            frame
        } else {
            frame.resized(PhysicalSize { width: out_width, height: out_height })
        })
    })
}

pub fn decode_video(
    source: &Path,
    trim: TrimRange,
    size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[u8], u64) -> Result<()>,
) -> Result<()> {
    let (start, end) = logic::trim_window(trim.start_ms, trim.end_ms);
    mf::on_worker(
        "gif-decoder",
        2,
        |platform, frames| {
            let file = Source::open(platform, source, true)?;
            let video = file.video()?;
            let mut layout = file.decode_as_rgb32(video)?;
            file.select(video.index, true)?;
            file.seek(logic::hns(start))?;
            // The picture on screen at the trim's start may be a frame from before it (a still
            // screen sends no new one); it is shown from time 0.
            let mut before: Option<(IMFSample, Layout)> = None;
            let mut sent = false;
            let send = |sample: &IMFSample, layout: &Layout, at: u64| -> Result<()> {
                frames.send((scaled(sample, layout, size)?, at)).map_err(|_| RecordError::Cancelled)
            };
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err(RecordError::Cancelled);
                }
                let (time, sample, last) = match file.read(video.index)? {
                    Read::Sample { at, sample, changed, last, .. } => {
                        if changed {
                            layout = file.layout(video)?;
                        }
                        (logic::ns(at), sample, last)
                    }
                    Read::Gap => continue,
                    Read::End { .. } => break,
                };
                if time < start {
                    before = Some((sample, layout));
                } else if time >= end {
                    break;
                } else {
                    if let Some((held, held_layout)) = before.take() {
                        if time > start {
                            send(&held, &held_layout, 0)?;
                        }
                    }
                    send(&sample, &layout, time - start)?;
                    sent = true;
                }
                if last {
                    break;
                }
            }
            if let (false, Some((held, held_layout))) = (sent, before) {
                send(&held, &held_layout, 0)?;
            }
            Ok(())
        },
        |(rgba, at): (Vec<u8>, u64)| each(&rgba, at),
    )
}

/// The frames of `decode_video` as frames in memory, which the take writer converts to NV12.
pub fn decode_frames(
    source: &Path,
    span: TrimRange,
    size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(VideoFrame, u64) -> Result<()>,
) -> Result<()> {
    decode_video(source, span, size, cancel, &mut |rgba, at| {
        let frame = rgba_frame(rgba.to_vec(), size.width, size.height)?;
        let frame = VideoFrame {
            timestamp: HostTime(at),
            size,
            format: StreamPixelFormat::Bgra,
            color_space: ColorSpace::Srgb,
            buffer: VideoBuffer::Cpu(Arc::new(frame)),
        };
        each(frame, at)
    })
}

/// Decodes audio track `track` within `span` to float, brought to 48 kHz stereo, buffer by
/// buffer with each buffer's position from the span's start.
pub fn decode_audio(
    source: &Path,
    track: usize,
    span: TrimRange,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[f32], u64) -> Result<()>,
) -> Result<()> {
    mf::on_worker(
        "audio-decoder",
        8,
        |platform, buffers| {
            let file = Source::open(platform, source, false)?;
            let stream = file
                .audio
                .get(track)
                .ok_or_else(|| RecordError::Encoder(format!("{} has no audio track {track}", source.display())))?;
            let start = span.start_ms.saturating_mul(1_000_000);
            let end = span.end_ms.saturating_mul(1_000_000).min(logic::ns(file.duration));
            if end <= start {
                return Ok(());
            }
            let wanted = file.api().media_type()?;
            mf::set_guid(&wanted, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            mf::set_guid(&wanted, &MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
            // SAFETY: the index is the reader's own, and the type is a live object the call reads.
            unsafe { file.reader.SetCurrentMediaType(stream.index, None, &wanted) }
                .map_err(|e| mf::failed("setting up the audio decoder", &e))?;
            // SAFETY: as above.
            let current = unsafe { file.reader.GetCurrentMediaType(stream.index) }
                .map_err(|e| mf::failed("reading the decoded audio format", &e))?;
            let channels = mf::get_u32(&current, &MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2).clamp(1, 16) as u16;
            let rate = mf::get_u32(&current, &MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(crate::audio::RATE);
            file.select(stream.index, true)?;
            file.seek(logic::hns(start))?;
            let mut conformer = crate::audio::Conformer::new();
            let mut placed = crate::audio::SpanAudio::new(start, end);
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err(RecordError::Cancelled);
                }
                match file.read(stream.index)? {
                    Read::Sample { at, sample, last, .. } => {
                        // SAFETY: the sample is live; the call returns one buffer holding its data.
                        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
                            .map_err(|e| mf::failed("reading audio", &e))?;
                        let samples: Vec<f32> = {
                            let locked = Locked::lock(&buffer)?;
                            locked
                                .bytes()
                                .chunks_exact(4)
                                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                                .collect()
                        };
                        let stereo = conformer.conform(&samples, channels, rate);
                        if let Some((position, body)) = placed.place(logic::ns(at), &stereo) {
                            buffers.send((body.to_vec(), position)).map_err(|_| RecordError::Cancelled)?;
                        }
                        if last || placed.done() {
                            break;
                        }
                    }
                    Read::Gap => {}
                    Read::End { .. } => break,
                }
            }
            Ok(())
        },
        |(samples, position): (Vec<f32>, u64)| each(&samples, position),
    )
}

pub fn waveform(source: &Path, bins: usize) -> Result<Vec<f32>> {
    let bins = bins.clamp(1, 4096);
    mf::on_thread("waveform", |platform| {
        let file = Source::open(platform, source, false)?;
        let mut peaks = vec![0.0f32; bins];
        let Some(track) = file.audio.first() else { return Ok(peaks) };
        let wanted = file.api().media_type()?;
        mf::set_guid(&wanted, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        mf::set_guid(&wanted, &MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        // SAFETY: the index is the reader's own, and the type is a live object the call reads.
        unsafe { file.reader.SetCurrentMediaType(track.index, None, &wanted) }
            .map_err(|e| mf::failed("setting up the audio decoder", &e))?;
        // SAFETY: as above.
        let current = unsafe { file.reader.GetCurrentMediaType(track.index) }
            .map_err(|e| mf::failed("reading the decoded audio format", &e))?;
        let channels = mf::get_u32(&current, &MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2).max(1) as usize;
        let rate = mf::get_u32(&current, &MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48_000);
        file.select(track.index, true)?;
        loop {
            match file.read(track.index)? {
                Read::Sample { at, sample, last, .. } => {
                    // SAFETY: the sample is live; the call returns one buffer holding its data.
                    let buffer =
                        unsafe { sample.ConvertToContiguousBuffer() }.map_err(|e| mf::failed("reading audio", &e))?;
                    let locked = Locked::lock(&buffer)?;
                    let samples: Vec<f32> =
                        locked.bytes().chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                    logic::add_peaks(&mut peaks, file.duration, at, &samples, channels, rate);
                    if last {
                        break;
                    }
                }
                Read::Gap => {}
                Read::End { .. } => break,
            }
        }
        Ok(peaks)
    })
}

/// The times of the video samples (and whether each is a key frame), read without decoding.
fn video_samples(source: &Path) -> Result<Vec<(u64, Option<bool>)>> {
    mf::on_thread("mp4-index", |platform| {
        let file = Source::open(platform, source, false)?;
        let video = file.video()?;
        file.select(video.index, true)?;
        let mut samples = Vec::new();
        loop {
            match file.read(video.index)? {
                Read::Sample { at, sample, last, .. } => {
                    samples.push((logic::ns(at), clean_point(&sample)));
                    if last {
                        break;
                    }
                }
                Read::Gap => {}
                Read::End { .. } => break,
            }
        }
        Ok(samples)
    })
}

/// Whether a compressed sample is a key frame; `None` when the source did not say.
fn clean_point(sample: &IMFSample) -> Option<bool> {
    mf::get_u32(sample, &MFSampleExtension_CleanPoint).map(|v| v != 0)
}

pub fn video_timestamps(source: &Path) -> Result<Vec<u64>> {
    Ok(video_samples(source)?.into_iter().map(|(at, _)| at).collect())
}

pub fn keyframe_times(source: &Path) -> Result<Vec<u64>> {
    let samples = video_samples(source)?;
    // A file that marks no sample at all is one whose every sample is a key frame.
    if samples.iter().all(|(_, key)| key.is_none()) {
        return Ok(samples.into_iter().map(|(at, _)| at).collect());
    }
    Ok(samples.into_iter().filter(|(_, key)| *key == Some(true)).map(|(at, _)| at).collect())
}

/// The last key frame at or before `start` (100 ns units), looked for in the ten seconds before
/// it first, as the macOS reader does.
fn keyframe_before(file: &Source, video: &Stream, start: i64) -> Result<Option<i64>> {
    if start <= 0 {
        return Ok(Some(0));
    }
    file.select(video.index, true)?;
    let near = (start - 10 * HNS_PER_SECOND).max(0);
    let mut found = keyframe_between(file, video, near, start)?;
    if found.is_none() && near > 0 {
        found = keyframe_between(file, video, 0, start)?;
    }
    Ok(found)
}

fn keyframe_between(file: &Source, video: &Stream, from: i64, to: i64) -> Result<Option<i64>> {
    file.seek(from)?;
    let mut found = None;
    loop {
        match file.read(video.index)? {
            Read::Sample { at, sample, last, .. } => {
                if at > to {
                    break;
                }
                if clean_point(&sample) == Some(true) {
                    found = Some(at);
                }
                if last {
                    break;
                }
            }
            Read::Gap => {}
            Read::End { .. } => break,
        }
    }
    Ok(found)
}

pub fn write_mp4(
    source: &Path,
    dest: &Path,
    plan: &Mp4Plan,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<TrimRange> {
    let wanted = logic::hns(plan.trim.start_ms.saturating_mul(1_000_000));
    let end = logic::hns(plan.trim.end_ms.saturating_mul(1_000_000));
    mf::on_worker(
        "mp4-export",
        4,
        |platform, fractions| {
            let file = Source::open(platform, source, plan.reencode.is_some())?;
            let video = file.video()?;
            let audio = plan.audio_track.and_then(|i| file.audio.get(i));
            // A pass-through starts on the key frame at or before the cut; a re-encode cuts exactly.
            let start = match plan.reencode {
                None => keyframe_before(&file, video, wanted)?.unwrap_or(0),
                Some(_) => wanted,
            };
            let end = end.max(start + 10_000);
            let report = |at: i64| {
                // A full queue only means the progress bar is a step behind.
                let _ = fractions.try_send((at - start) as f64 / (end - start) as f64);
            };
            let cut = Cut { file: &file, video, audio, dest, start, end, cancel, report: &report };
            match plan.reencode {
                None => cut.pass_through()?,
                Some(re) => cut.reencode(re)?,
            }
            Ok(TrimRange { start_ms: (start / 10_000) as u64, end_ms: plan.trim.end_ms })
        },
        |fraction| {
            progress(fraction);
            Ok(())
        },
    )
}

/// The export's file: the byte stream (created only if no file has the name) and its writer.
struct Output {
    bytes: IMFByteStream,
    writer: Option<IMFSinkWriter>,
}

impl Output {
    fn create(api: &Api, dest: &Path, hardware: bool) -> Result<Output> {
        let bytes = api.file(MF_ACCESSMODE_READWRITE, MF_OPENMODE_FAIL_IF_EXIST, dest)?;
        let writer = mp4::mp4_writer(api, dest, &bytes, hardware);
        // On failure, dropping the output closes the stream, so the caller can remove the file.
        let mut output = Output { bytes, writer: None };
        output.writer = Some(writer?);
        Ok(output)
    }

    fn writer(&self) -> Result<&IMFSinkWriter> {
        self.writer.as_ref().ok_or_else(mp4::closed)
    }

    /// Adds a stream that takes the samples of `native` as they are.
    fn copy_stream(&self, native: &IMFMediaType) -> Result<u32> {
        let writer = self.writer()?;
        // SAFETY: the type is a live object both calls read; without encoding, the stream's
        // input and output types are the same.
        unsafe {
            let stream = writer.AddStream(native).map_err(|e| mf::failed("adding a stream to the export", &e))?;
            writer
                .SetInputMediaType(stream, native, None::<&IMFAttributes>)
                .map_err(|e| mf::failed("setting up a stream of the export", &e))?;
            Ok(stream)
        }
    }

    fn begin(&self) -> Result<()> {
        // SAFETY: every stream has its types.
        unsafe { self.writer()?.BeginWriting() }.map_err(|e| mf::failed("starting the export", &e))
    }

    fn write(&self, stream: u32, sample: &IMFSample, at: i64, duration: Option<i64>) -> Result<()> {
        // SAFETY: the sample is live and ours to retime; the index is one `AddStream` gave.
        unsafe {
            sample.SetSampleTime(at).map_err(|e| mf::failed("IMFSample::SetSampleTime", &e))?;
            if let Some(duration) = duration {
                sample
                    .SetSampleDuration(duration.max(1))
                    .map_err(|e| mf::failed("IMFSample::SetSampleDuration", &e))?;
            }
            self.writer()?.WriteSample(stream, sample).map_err(|e| mf::failed("writing the export", &e))
        }
    }

    fn finish(mut self) -> Result<()> {
        let writer = self.writer.take().ok_or_else(mp4::closed)?;
        // SAFETY: the writer is live and finalised once.
        unsafe { writer.Finalize() }.map_err(|e| mf::failed("finishing the export", &e))
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.writer = None;
        // SAFETY: the byte stream is live and closed once, after its writer is released.
        let _ = unsafe { self.bytes.Close() };
    }
}

/// One export: the part of `file` from `start` to `end` (100 ns units) written to `dest`.
struct Cut<'a, 'b> {
    file: &'a Source<'b>,
    video: &'a Stream,
    audio: Option<&'a Stream>,
    dest: &'a Path,
    start: i64,
    end: i64,
    cancel: &'a AtomicBool,
    report: &'a dyn Fn(i64),
}

/// Where the reading stands while samples are copied: which streams still give samples.
struct Lanes {
    video: bool,
    audio: bool,
}

impl Cut<'_, '_> {
    fn api(&self) -> &'static Api {
        self.file.api()
    }

    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(RecordError::Cancelled);
        }
        Ok(())
    }

    /// Selects the streams to copy and moves to the cut's start.
    fn start_reading(&self) -> Result<Lanes> {
        self.file.select(ANY_STREAM, false)?;
        self.file.select(self.video.index, true)?;
        if let Some(audio) = self.audio {
            self.file.select(audio.index, true)?;
        }
        self.file.seek(self.start)?;
        Ok(Lanes { video: true, audio: self.audio.is_some() })
    }

    /// Copies an audio sample inside the cut; past the end the stream is left.
    fn copy_audio(
        &self,
        out: &Output,
        stream: Option<u32>,
        lanes: &mut Lanes,
        at: i64,
        sample: &IMFSample,
    ) -> Result<()> {
        let (Some(stream), Some(audio)) = (stream, self.audio) else { return Ok(()) };
        if at >= self.end {
            lanes.audio = false;
            return self.file.select(audio.index, false);
        }
        if at >= self.start {
            out.write(stream, sample, at - self.start, None)?;
        }
        Ok(())
    }

    /// The compressed samples copied as they are, cut at key frames.
    fn pass_through(&self) -> Result<()> {
        let out = Output::create(self.api(), self.dest, false)?;
        let video_out = out.copy_stream(&self.video.native)?;
        let audio_out = self.audio.map(|a| out.copy_stream(&a.native)).transpose()?;
        out.begin()?;
        let mut lanes = self.start_reading()?;
        // Each video sample is written once the next one says how long it lasts.
        let mut held: Option<(i64, IMFSample)> = None;
        while lanes.video || lanes.audio {
            self.check()?;
            match self.file.read(ANY_STREAM)? {
                Read::Sample { stream, at, sample, last, .. } if stream == self.video.index => {
                    if at >= self.end {
                        lanes.video = false;
                        self.file.select(stream, false)?;
                    } else if at >= self.start {
                        (self.report)(at);
                        if let Some((time, previous)) = held.replace((at, sample)) {
                            out.write(video_out, &previous, time - self.start, Some(at - time))?;
                        }
                    }
                    lanes.video &= !last;
                }
                Read::Sample { at, sample, last, .. } => {
                    self.copy_audio(&out, audio_out, &mut lanes, at, &sample)?;
                    lanes.audio &= !last;
                }
                Read::Gap => {}
                Read::End { stream } if stream == self.video.index => lanes.video = false,
                Read::End { .. } => lanes.audio = false,
            }
        }
        // The last picture stays until the cut's end.
        if let Some((time, last)) = held {
            out.write(video_out, &last, time - self.start, Some(self.end - time))?;
        }
        out.finish()
    }

    /// The video decoded, scaled and encoded again as H.264 (HEVC above its limits); the audio
    /// is copied. A hardware encoder that fails before the first frame is written gives way to
    /// the software one, and the export starts over.
    fn reencode(&self, re: Reencode) -> Result<()> {
        let codec = codec_for(VideoCodec::H264, re.size);
        let nominal = mf::get_u64(&self.video.native, &MF_MT_FRAME_RATE)
            .map(logic::unpack)
            .filter(|&(_, d)| d > 0)
            .map_or(0.0, |(n, d)| n as f64 / d as f64);
        // The nominal rate of a variable-rate recording is low; a key frame every two seconds of
        // 30 frames keeps seeking quick.
        let fps = (nominal.round() as u32).max(30);
        let settings = VideoSettings {
            codec,
            width: re.size.width,
            height: re.size.height,
            fps,
            bits: re.bits_per_second.max(1),
            gop: fps * 2,
        };
        let layout = self.file.decode_as_rgb32(self.video)?;
        match self.reencode_with(settings, layout, true) {
            Err((error, false)) if !matches!(error, RecordError::Cancelled | RecordError::Io(_)) => {
                let _ = std::fs::remove_file(self.dest);
                self.reencode_with(settings, layout, false).map_err(|(again, _)| {
                    RecordError::Encoder(format!(
                        "{}; without hardware encoding: {}",
                        mp4::reason(error),
                        mp4::reason(again)
                    ))
                })
            }
            result => result.map_err(|(error, _)| error),
        }
    }

    /// One re-encoding pass; a failure says whether a frame had been written.
    fn reencode_with(
        &self,
        settings: VideoSettings,
        layout: Layout,
        hardware: bool,
    ) -> std::result::Result<(), (RecordError, bool)> {
        let mut wrote = false;
        let result = self.reencode_pass(settings, layout, hardware, &mut wrote);
        result.map_err(|error| (error, wrote))
    }

    fn reencode_pass(
        &self,
        settings: VideoSettings,
        mut layout: Layout,
        hardware: bool,
        wrote: &mut bool,
    ) -> Result<()> {
        let api = self.api();
        let out = Output::create(api, self.dest, hardware)?;
        let video_out = mp4::add_video(api, out.writer()?, settings)?;
        let audio_out = self.audio.map(|a| out.copy_stream(&a.native)).transpose()?;
        out.begin()?;
        let mut lanes = self.start_reading()?;
        let size = PhysicalSize { width: settings.width, height: settings.height };
        // The picture on screen at the cut's start may come from a frame before it.
        let mut before: Option<(IMFSample, Layout)> = None;
        let mut any = false;
        // Each encoded picture is written once the next one says how long it lasts.
        let mut held: Option<(i64, IMFSample)> = None;
        let mut push = |sample: &IMFSample, layout: &Layout, at: i64| -> Result<()> {
            let data = scaled(sample, layout, size)?;
            let pixels = Pixels {
                data: &data,
                stride: size.width as isize * 4,
                width: size.width as usize,
                height: size.height as usize,
                red_first: true,
            };
            let buffer = mp4::nv12(api, &pixels, size.width, size.height)?;
            let picture = api.sample()?;
            // SAFETY: the sample and the buffer are live; the sample keeps its own reference.
            unsafe { picture.AddBuffer(&buffer) }.map_err(|e| mf::failed("IMFSample::AddBuffer", &e))?;
            if let Some((time, previous)) = held.replace((at, picture)) {
                out.write(video_out, &previous, time, Some(at - time))?;
                *wrote = true;
            }
            Ok(())
        };
        while lanes.video || lanes.audio {
            self.check()?;
            match self.file.read(ANY_STREAM)? {
                Read::Sample { stream, at, sample, changed, last } if stream == self.video.index => {
                    if changed {
                        layout = self.file.layout(self.video)?;
                    }
                    if at < self.start {
                        before = Some((sample, layout));
                    } else if at >= self.end {
                        lanes.video = false;
                        self.file.select(stream, false)?;
                    } else {
                        (self.report)(at);
                        if let Some((first, first_layout)) = before.take() {
                            if at > self.start && !any {
                                push(&first, &first_layout, 0)?;
                            }
                        }
                        push(&sample, &layout, at - self.start)?;
                        any = true;
                    }
                    lanes.video &= !last;
                }
                Read::Sample { at, sample, last, .. } => {
                    self.copy_audio(&out, audio_out, &mut lanes, at, &sample)?;
                    lanes.audio &= !last;
                }
                Read::Gap => {}
                Read::End { stream } if stream == self.video.index => lanes.video = false,
                Read::End { .. } => lanes.audio = false,
            }
        }
        if let (false, Some((first, first_layout))) = (any, before) {
            push(&first, &first_layout, 0)?;
        }
        if let Some((time, last)) = held {
            out.write(video_out, &last, time, Some(self.end - self.start - time))?;
            *wrote = true;
        }
        out.finish()
    }
}
