//! The take's MP4 writer: `AVAssetWriter` with the hardware H.264 or HEVC encoder, AAC audio
//! tracks, and movie fragments, so a file cut short by a crash or a full disk still plays up to
//! its last fragment.
//!
//! Frames arrive as the stream's own `CVPixelBuffer`s and go to the encoder through a pixel
//! buffer adaptor without a copy. Audio arrives as 48 kHz stereo float placed by the session;
//! each append becomes one `CMSampleBuffer` stamped with its position, so the tracks stay on the
//! session's timeline.
//!
//! An offline writer ([`WriterSpec::offline`], an export with cuts) takes the decoder's pictures
//! and scales them to its size, waits for the encoders instead of dropping sound, writes no movie
//! fragments but moves its index to the front, and cancels itself when it is dropped unfinished.
//!
//! With several audio tracks, they form one alternate group: the first (the mix) is enabled and
//! is the one every player plays; the microphone and system audio tracks are disabled
//! alternates that an editor can pick.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use glimpsfy_capture::{MacPixelBuffer, StreamPixelFormat, VideoBuffer, VideoFrame};
use objc2::rc::Retained;
use objc2::{AnyThread, Message};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputGroup, AVAssetWriterInputPixelBufferAdaptor,
    AVAssetWriterStatus, AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo, AVMetadataCommonIdentifierTitle,
    AVMutableMetadataItem, AVVideoAllowFrameReorderingKey, AVVideoAverageBitRateKey, AVVideoCodecKey,
    AVVideoCodecTypeH264, AVVideoCodecTypeHEVC, AVVideoColorPrimariesKey, AVVideoColorPrimaries_ITU_R_709_2,
    AVVideoColorPropertiesKey, AVVideoCompressionPropertiesKey, AVVideoExpectedSourceFrameRateKey, AVVideoHeightKey,
    AVVideoMaxKeyFrameIntervalDurationKey, AVVideoMaxKeyFrameIntervalKey, AVVideoProfileLevelH264HighAutoLevel,
    AVVideoProfileLevelKey, AVVideoScalingModeKey, AVVideoScalingModeResizeAspect, AVVideoTransferFunctionKey,
    AVVideoTransferFunction_ITU_R_709_2, AVVideoWidthKey, AVVideoYCbCrMatrixKey, AVVideoYCbCrMatrix_ITU_R_709_2,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    kCMBlockBufferAssureMemoryNowFlag, kCMTimeZero, CMAudioSampleBufferCreateReadyWithPacketDescriptions,
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSString};

use super::{any, boolean, constant, describe, dict, file_url, float, int, pcm_format, time};
use crate::session::{TakeWriter, WriterSpec};
use crate::{RecordError, Result, TrackKind, VideoCodec};

/// Video timestamps use the MPEG clock, which fits every frame rate without rounding drift.
const VIDEO_SCALE: i32 = 90_000;
const AAC: i64 = u32::from_be_bytes(*b"aac ") as i64;
/// Audio a busy encoder has not taken yet is kept this long before the oldest is dropped.
const MAX_PENDING_FRAMES: u64 = crate::audio::RATE as u64 * 10;
/// An offline writer (an export) waits once this much audio is queued, and drops none.
const OFFLINE_PENDING_FRAMES: u64 = crate::audio::RATE as u64;
/// How long an offline writer waits for an encoder that takes nothing before it gives up.
const OFFLINE_STALL: Duration = Duration::from_secs(30);

/// Bits per pixel per frame of the recording, for screen content at the quality the design asks
/// for ("crisp text"); HEVC needs about two thirds of H.264's rate for the same picture.
fn bit_rate(spec: &WriterSpec) -> i64 {
    if let Some(bits) = spec.bits_per_second {
        return bits.max(1) as i64;
    }
    let bpp = match spec.codec {
        VideoCodec::H264 => 0.08,
        VideoCodec::Hevc => 0.05,
    };
    let bits = spec.size.width as f64 * spec.size.height as f64 * spec.fps.max(1) as f64 * bpp;
    bits.clamp(2_000_000.0, 60_000_000.0) as i64
}

pub fn create(spec: &WriterSpec) -> Result<Box<dyn TakeWriter>> {
    Ok(Box::new(MacWriter::new(spec)?))
}

pub(crate) struct MacWriter {
    writer: Retained<AVAssetWriter>,
    video: Retained<AVAssetWriterInput>,
    adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
    audio: Vec<Retained<AVAssetWriterInput>>,
    pcm: CFRetained<CMFormatDescription>,
    pending: Vec<VecDeque<(u64, Vec<f32>)>>,
    dropped_audio_frames: u64,
    finished: bool,
    /// The last frame written and its time, appended once more at the end of the take (see
    /// `finish`).
    last: Option<(CFRetained<CVPixelBuffer>, u64)>,
    /// One frame's length at the take's frame rate.
    frame_ns: u64,
    /// An export: audio waits instead of being dropped, and a writer dropped unfinished cancels.
    offline: bool,
    path: std::path::PathBuf,
}

// SAFETY: AVAssetWriter and its inputs may be used from any thread as long as calls are not
// made concurrently; the session holds this writer behind a mutex.
unsafe impl Send for MacWriter {}

/// The title a track carries, which `reader::inspect` reads back to tell the tracks apart.
pub(crate) fn track_title(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "Video",
        TrackKind::MixedAudio => "Mixed audio",
        TrackKind::Microphone => "Microphone",
        TrackKind::SystemAudio => "System audio",
    }
}

fn title_metadata(kind: TrackKind) -> Option<Retained<NSArray<objc2_av_foundation::AVMetadataItem>>> {
    // SAFETY: plain property setters on a new mutable metadata item.
    unsafe {
        let identifier = AVMetadataCommonIdentifierTitle?;
        let item = AVMutableMetadataItem::metadataItem();
        item.setIdentifier(Some(identifier));
        item.setValue(Some(&NSString::from_str(track_title(kind))));
        item.setExtendedLanguageTag(Some(&NSString::from_str("und")));
        let item: Retained<objc2_av_foundation::AVMetadataItem> = Retained::into_super(item);
        Some(NSArray::from_retained_slice(&[item]))
    }
}

impl MacWriter {
    fn new(spec: &WriterSpec) -> Result<MacWriter> {
        let url = file_url(&spec.path)?;
        // SAFETY: the AVFoundation calls below follow the documented order: create the writer,
        // configure it and add inputs, start writing, start the session.
        unsafe {
            let file_type = constant(AVFileTypeMPEG4, "AVFileTypeMPEG4")?;
            let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type)
                .map_err(|e| RecordError::Encoder(describe(&e)))?;
            if spec.offline {
                // An export is finished in one go: its index goes to the front, so a browser
                // starts playing before the whole file has arrived, as the other exports do.
                writer.setShouldOptimizeForNetworkUse(true);
            } else {
                writer.setMovieFragmentInterval(time(spec.fragment_interval_ms * 1_000_000, 1000));
                writer.setShouldOptimizeForNetworkUse(false);
            }

            let codec = match spec.codec {
                VideoCodec::H264 => constant(AVVideoCodecTypeH264, "H.264")?,
                VideoCodec::Hevc => constant(AVVideoCodecTypeHEVC, "HEVC")?,
            };
            let mut compression = vec![
                (constant(AVVideoAverageBitRateKey, "AVVideoAverageBitRateKey")?, int(bit_rate(spec))),
                (
                    constant(AVVideoExpectedSourceFrameRateKey, "AVVideoExpectedSourceFrameRateKey")?,
                    int(spec.fps as i64),
                ),
                // A keyframe every second keeps the export's pass-through trim within a second of
                // the wanted start.
                (
                    constant(AVVideoMaxKeyFrameIntervalKey, "AVVideoMaxKeyFrameIntervalKey")?,
                    int(spec.fps.max(1) as i64),
                ),
                // The same in time, for a still screen that sends few frames.
                (constant(AVVideoMaxKeyFrameIntervalDurationKey, "AVVideoMaxKeyFrameIntervalDurationKey")?, float(1.0)),
                // No B-frames: a recording is cut and appended in presentation order, and a
                // pass-through trim can end on any frame.
                (constant(AVVideoAllowFrameReorderingKey, "AVVideoAllowFrameReorderingKey")?, boolean(false)),
            ];
            if spec.codec == VideoCodec::H264 {
                compression.push((
                    constant(AVVideoProfileLevelKey, "AVVideoProfileLevelKey")?,
                    any(constant(AVVideoProfileLevelH264HighAutoLevel, "H.264 High")?.retain()),
                ));
            }
            // Screen pixels are sRGB, which shares its primaries with BT.709; tagging BT.709 is
            // what QuickTime and browsers read for HD video.
            let color = dict(&[
                (
                    constant(AVVideoColorPrimariesKey, "AVVideoColorPrimariesKey")?,
                    any(constant(AVVideoColorPrimaries_ITU_R_709_2, "BT.709 primaries")?.retain()),
                ),
                (
                    constant(AVVideoTransferFunctionKey, "AVVideoTransferFunctionKey")?,
                    any(constant(AVVideoTransferFunction_ITU_R_709_2, "BT.709 transfer")?.retain()),
                ),
                (
                    constant(AVVideoYCbCrMatrixKey, "AVVideoYCbCrMatrixKey")?,
                    any(constant(AVVideoYCbCrMatrix_ITU_R_709_2, "BT.709 matrix")?.retain()),
                ),
            ]);
            let mut settings = vec![
                (constant(AVVideoCodecKey, "AVVideoCodecKey")?, any(codec.retain())),
                (constant(AVVideoWidthKey, "AVVideoWidthKey")?, int(spec.size.width as i64)),
                (constant(AVVideoHeightKey, "AVVideoHeightKey")?, int(spec.size.height as i64)),
                (constant(AVVideoColorPropertiesKey, "AVVideoColorPropertiesKey")?, any(color)),
                (
                    constant(AVVideoCompressionPropertiesKey, "AVVideoCompressionPropertiesKey")?,
                    any(dict(&compression)),
                ),
            ];
            if spec.offline {
                // An export hands over the decoder's pictures at the recording's size; a smaller
                // export is scaled here, by the encoder's own scaler.
                settings.push((
                    constant(AVVideoScalingModeKey, "AVVideoScalingModeKey")?,
                    any(constant(AVVideoScalingModeResizeAspect, "resize aspect")?.retain()),
                ));
            }
            let settings = dict(&settings);
            let video_type = constant(AVMediaTypeVideo, "AVMediaTypeVideo")?;
            if !writer.canApplyOutputSettings_forMediaType(Some(&settings), video_type) {
                return Err(RecordError::Encoder(format!(
                    "the encoder cannot write {}×{} video",
                    spec.size.width, spec.size.height
                )));
            }
            let video = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(video_type, Some(&settings));
            video.setExpectsMediaDataInRealTime(true);
            video.setMediaTimeScale(VIDEO_SCALE);
            let adaptor =
                AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(
                    &video, None,
                );
            if !writer.canAddInput(&video) {
                return Err(RecordError::Encoder("the video track could not be added".into()));
            }
            writer.addInput(&video);

            let pcm = pcm_format()?;
            let audio_type = constant(AVMediaTypeAudio, "AVMediaTypeAudio")?;
            let format_key = NSString::from_str("AVFormatIDKey");
            let rate_key = NSString::from_str("AVSampleRateKey");
            let channels_key = NSString::from_str("AVNumberOfChannelsKey");
            let bit_rate_key = NSString::from_str("AVEncoderBitRateKey");
            let mut audio = Vec::new();
            for &kind in &spec.audio {
                let bits = if kind == TrackKind::MixedAudio { 192_000 } else { 160_000 };
                let settings = dict(&[
                    (&format_key, int(AAC)),
                    (&rate_key, float(crate::audio::RATE as f64)),
                    (&channels_key, int(2)),
                    (&bit_rate_key, int(bits)),
                ]);
                let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(
                    audio_type,
                    Some(&settings),
                    Some(&pcm),
                );
                input.setExpectsMediaDataInRealTime(true);
                if let Some(metadata) = title_metadata(kind) {
                    input.setMetadata(&metadata);
                }
                if !writer.canAddInput(&input) {
                    return Err(RecordError::Encoder("an audio track could not be added".into()));
                }
                writer.addInput(&input);
                audio.push(input);
            }
            if audio.len() > 1 {
                let inputs = NSArray::from_retained_slice(&audio);
                let group = AVAssetWriterInputGroup::initWithInputs_defaultInput(
                    AVAssetWriterInputGroup::alloc(),
                    &inputs,
                    Some(&audio[0]),
                );
                if writer.canAddInputGroup(&group) {
                    writer.addInputGroup(&group);
                } else {
                    // Without the group every track would play at once; keep the mix alone on.
                    for input in &audio[1..] {
                        input.setMarksOutputTrackAsEnabled(false);
                    }
                }
            }

            if !writer.startWriting() {
                let reason = writer.error().map(|e| describe(&e)).unwrap_or_else(|| "unknown".into());
                return Err(RecordError::Encoder(format!("the file could not be started: {reason}")));
            }
            writer.startSessionAtSourceTime(kCMTimeZero);
            let pending = spec.audio.iter().map(|_| VecDeque::new()).collect();
            Ok(MacWriter {
                writer,
                video,
                adaptor,
                audio,
                pcm,
                pending,
                dropped_audio_frames: 0,
                finished: false,
                last: None,
                frame_ns: 1_000_000_000 / spec.fps.max(1) as u64,
                offline: spec.offline,
                path: spec.path.clone(),
            })
        }
    }

    fn failed(&self) -> Option<String> {
        // SAFETY: reading the writer's status and error.
        unsafe {
            (self.writer.status() == AVAssetWriterStatus::Failed)
                .then(|| self.writer.error().map(|e| describe(&e)).unwrap_or_else(|| "the writer failed".into()))
        }
    }

    /// A pixel buffer of a frame that came as CPU pixels, copied into one of the adaptor's.
    fn copy_into_pool(&self, frame: &glimpsfy_capture::Frame) -> Result<CFRetained<CVPixelBuffer>> {
        // SAFETY: the pool belongs to the adaptor; the buffer is locked while written, rows are
        // bounded by both strides.
        unsafe {
            let pool: Retained<CVPixelBufferPool> =
                self.adaptor.pixelBufferPool().ok_or_else(|| RecordError::Encoder("no pixel buffer pool".into()))?;
            let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
            let status = CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::from(&mut out));
            let buffer = NonNull::new(out)
                .filter(|_| status == 0)
                .ok_or_else(|| RecordError::Encoder(format!("no pixel buffer ({status})")))?;
            let buffer = CFRetained::from_raw(buffer);
            CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
            let base = CVPixelBufferGetBaseAddress(&buffer) as *mut u8;
            let stride = CVPixelBufferGetBytesPerRow(&buffer);
            let row = (frame.size.width as usize * 4).min(stride).min(frame.bytes_per_row);
            if !base.is_null() {
                for y in 0..frame.size.height as usize {
                    let src = &frame.data[y * frame.bytes_per_row..y * frame.bytes_per_row + row];
                    std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(y * stride), row);
                }
            }
            CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
            Ok(buffer)
        }
    }

    fn audio_sample(&self, position: u64, samples: &[f32]) -> Result<Retained<CMSampleBuffer>> {
        let bytes = std::mem::size_of_val(samples);
        let frames = (samples.len() / 2) as isize;
        // SAFETY: the block is allocated by Core Media at once (`AssureMemoryNow`) with `bytes`
        // bytes and filled from `samples`, which holds exactly that many; the sample buffer
        // takes its own references to the block and the format.
        unsafe {
            let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
            let status = CMBlockBuffer::create_with_memory_block(
                None,
                std::ptr::null_mut(),
                bytes,
                None,
                std::ptr::null(),
                0,
                bytes,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            );
            let block = NonNull::new(block)
                .filter(|_| status == 0)
                .ok_or_else(|| RecordError::Encoder(format!("no audio block ({status})")))?;
            let block = CFRetained::from_raw(block);
            let source = NonNull::new(samples.as_ptr() as *mut c_void)
                .ok_or_else(|| RecordError::Encoder("empty audio".into()))?;
            let status = CMBlockBuffer::replace_data_bytes(source, &block, 0, bytes);
            if status != 0 {
                return Err(RecordError::Encoder(format!("the audio could not be copied ({status})")));
            }
            let mut sample: *mut CMSampleBuffer = std::ptr::null_mut();
            let status = CMAudioSampleBufferCreateReadyWithPacketDescriptions(
                None,
                &block,
                &self.pcm,
                frames,
                time(crate::audio::ns_at(position), crate::audio::RATE as i32),
                std::ptr::null(),
                NonNull::from(&mut sample),
            );
            let sample = NonNull::new(sample)
                .filter(|_| status == 0)
                .ok_or_else(|| RecordError::Encoder(format!("no audio sample ({status})")))?;
            Retained::from_raw(sample.as_ptr()).ok_or_else(|| RecordError::Encoder("no audio sample".into()))
        }
    }

    /// Hands queued audio of `track` to its input while the input takes more.
    fn drain(&mut self, track: usize) -> Result<()> {
        loop {
            let Some(input) = self.audio.get(track) else { return Ok(()) };
            // SAFETY: reading the input's readiness.
            if !unsafe { input.isReadyForMoreMediaData() } {
                return Ok(());
            }
            let Some((position, samples)) = self.pending[track].pop_front() else { return Ok(()) };
            let sample = self.audio_sample(position, &samples)?;
            // SAFETY: the sample buffer is complete and matches the input's source format hint.
            if !unsafe { self.audio[track].appendSampleBuffer(&sample) } {
                return Err(RecordError::Encoder(self.failed().unwrap_or_else(|| "audio was refused".into())));
            }
        }
    }
}

impl TakeWriter for MacWriter {
    fn append_video(&mut self, frame: &VideoFrame, media_ns: u64) -> Result<bool> {
        if let Some(reason) = self.failed() {
            return Err(RecordError::Encoder(reason));
        }
        // SAFETY: reading the input's readiness.
        if !unsafe { self.video.isReadyForMoreMediaData() } {
            return Ok(false);
        }
        let buffer: CFRetained<CVPixelBuffer> = match &frame.buffer {
            VideoBuffer::Native(native) => match native.0.downcast_ref::<MacPixelBuffer>() {
                Some(MacPixelBuffer(buffer)) => buffer.clone(),
                None => return Err(RecordError::Encoder("the frame is not a macOS pixel buffer".into())),
            },
            VideoBuffer::Cpu(cpu) => {
                if frame.format != StreamPixelFormat::Bgra {
                    return Err(RecordError::Encoder("CPU frames must be BGRA".into()));
                }
                self.copy_into_pool(cpu)?
            }
        };
        // SAFETY: the pixel buffer is retained for the call and the time is valid.
        let ok = unsafe { self.adaptor.appendPixelBuffer_withPresentationTime(&buffer, time(media_ns, VIDEO_SCALE)) };
        if !ok {
            return Err(RecordError::Encoder(self.failed().unwrap_or_else(|| "a frame was refused".into())));
        }
        self.last = Some((buffer, media_ns));
        for track in 0..self.audio.len() {
            self.drain(track)?;
        }
        Ok(true)
    }

    fn append_audio(&mut self, track: usize, position: u64, samples: &[f32]) -> Result<()> {
        if track >= self.pending.len() || samples.len() < 2 {
            return Ok(());
        }
        if let Some(reason) = self.failed() {
            return Err(RecordError::Encoder(reason));
        }
        self.pending[track].push_back((position, samples.to_vec()));
        let queued = |pending: &VecDeque<(u64, Vec<f32>)>| pending.iter().map(|(_, s)| s.len() as u64 / 2).sum::<u64>();
        if self.offline {
            // An export loses no sound: it waits for the encoder instead.
            let deadline = Instant::now() + OFFLINE_STALL;
            self.drain(track)?;
            while queued(&self.pending[track]) > OFFLINE_PENDING_FRAMES {
                if Instant::now() >= deadline {
                    return Err(RecordError::Encoder("the audio encoder stopped taking sound".into()));
                }
                std::thread::sleep(Duration::from_millis(1));
                self.drain(track)?;
            }
            return Ok(());
        }
        if queued(&self.pending[track]) > MAX_PENDING_FRAMES {
            if let Some((_, old)) = self.pending[track].pop_front() {
                self.dropped_audio_frames += old.len() as u64 / 2;
            }
        }
        self.drain(track)
    }

    fn failure(&self) -> Option<String> {
        self.failed()
    }

    fn finish(&mut self, end_ns: u64) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        // Let the encoders take what is still queued, for up to two seconds; an export loses no
        // sound, so it waits as long as the encoder keeps taking it.
        let deadline = Instant::now() + if self.offline { OFFLINE_STALL } else { Duration::from_secs(2) };
        while self.pending.iter().any(|q| !q.is_empty()) && Instant::now() < deadline {
            for track in 0..self.audio.len() {
                self.drain(track)?;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(reason) = self.failed() {
            return Err(RecordError::Encoder(reason));
        }
        if self.offline && self.pending.iter().any(|q| !q.is_empty()) {
            return Err(RecordError::Encoder("the audio encoder stopped taking sound".into()));
        }
        // A still screen sends no frames, so the video track would end with the last change
        // while the sound and the take go on: `endSessionAtSourceTime` does not stretch the last
        // frame. The last frame goes in once more, one frame before the end, so the picture
        // lasts as long as the take.
        if let Some((buffer, at)) = self.last.take() {
            let again = end_ns.saturating_sub(self.frame_ns);
            if again > at {
                let deadline = Instant::now() + if self.offline { OFFLINE_STALL } else { Duration::from_secs(1) };
                // SAFETY: reading the input's readiness.
                while !unsafe { self.video.isReadyForMoreMediaData() } && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
                // SAFETY: the pixel buffer is retained and the time is after the last frame's.
                unsafe { self.adaptor.appendPixelBuffer_withPresentationTime(&buffer, time(again, VIDEO_SCALE)) };
            }
        }
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let handler = RcBlock::new(move || {
            let _ = tx.try_send(());
        });
        // SAFETY: inputs are marked finished once, the session ends at a valid time after the
        // last sample, and the completion handler outlives the call (it is reference counted).
        unsafe {
            self.video.markAsFinished();
            for input in &self.audio {
                input.markAsFinished();
            }
            if end_ns > 0 {
                self.writer.endSessionAtSourceTime(time(end_ns, VIDEO_SCALE));
            }
            self.writer.finishWritingWithCompletionHandler(&handler);
        }
        if rx.recv_timeout(Duration::from_secs(60)).is_err() {
            return Err(RecordError::Encoder("the file did not finish within a minute".into()));
        }
        // SAFETY: reading the writer's final status.
        match unsafe { self.writer.status() } {
            AVAssetWriterStatus::Completed => Ok(()),
            _ => Err(RecordError::Encoder(self.failed().unwrap_or_else(|| "the file could not be finished".into()))),
        }
    }
}

impl Drop for MacWriter {
    fn drop(&mut self) {
        if !self.offline {
            return;
        }
        // SAFETY: reading the status, and cancelling a writer that is still writing; the call
        // blocks until it stops and removes what it wrote.
        unsafe {
            if !self.finished && self.writer.status() == AVAssetWriterStatus::Writing {
                self.writer.cancelWriting();
            }
        }
        // Moving the index to the front writes the file again as `<name>.sb-…` next to it; a
        // failed or cancelled move can leave that behind.
        let (Some(dir), Some(name)) = (self.path.parent(), self.path.file_name()) else { return };
        let prefix = format!("{}.sb-", name.to_string_lossy());
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
