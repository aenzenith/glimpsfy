//! The recording stream on macOS: an `SCStream` that delivers `CVPixelBuffer`s and audio, and the
//! microphone through `AVCaptureSession` where ScreenCaptureKit cannot capture it (macOS 14).
//!
//! - Video frames reach the sink as retained `CVPixelBuffer`s ([`MacPixelBuffer`]), so the pixels
//!   go from ScreenCaptureKit to the encoder without a copy.
//! - Audio is converted to interleaved 32-bit float once, here, whatever layout the system
//!   delivers (float or 16/32-bit integer, interleaved or one buffer per channel).
//! - Every buffer keeps its presentation time from the host clock, which ScreenCaptureKit and
//!   `AVCaptureSession` both stamp with, so video, system audio and the microphone line up.
//! - The filter comes from [`FilterPlan`]: a display or area recording leaves out every window
//!   of this app except the shown ones (the webcam bubble, the click ring), and a window
//!   recording crops the display to the window and shows only it and those windows.
//! - Samples arrive on one serial dispatch queue per stream, so the sink sees them in order.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, AllocAnyThread, ClassType, DefinedClass};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureAudioDataOutput, AVCaptureAudioDataOutputSampleBufferDelegate, AVCaptureConnection,
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession, AVMediaTypeAudio,
};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM,
    AudioBuffer as CoreAudioBuffer, AudioBufferList, AudioStreamBasicDescription,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceDisplayP3, kCGColorSpaceSRGB, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode,
};
use objc2_core_media::{
    kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, CMAudioFormatDescriptionGetStreamBasicDescription,
    CMBlockBuffer, CMClock, CMSampleBuffer, CMTime,
};
use objc2_core_video::{
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSObject, NSString};
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamDelegate, SCStreamErrorCode, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType, SCWindow,
};

use crate::display::{DisplayId, WindowId};
use crate::error::{CaptureError, Result};
use crate::frame::{ColorInfo, ColorSpace, Frame, PixelFormat};
use crate::geometry::{LogicalRect, PhysicalSize, ScaleFactor};
use crate::stream::{
    AudioBuffer, AudioSource, CaptureStream, FilterPlan, HostTime, MacPixelBuffer, MicrophoneSource, NativeBuffer,
    StreamConfig, StreamPixelFormat, StreamSink, StreamTarget, VideoBuffer, VideoFrame,
};

/// How long starting or stopping the stream may take before it counts as failed.
const TIMEOUT: Duration = Duration::from_secs(5);
/// How long the list of displays and windows may take.
const CONTENT_TIMEOUT: Duration = Duration::from_secs(3);

/// Now on the host clock (`CMClockGetHostTimeClock`), in nanoseconds.
pub fn host_now() -> HostTime {
    // SAFETY: the host time clock is a process-wide singleton that is always valid.
    let time: CMTime = unsafe { CMClock::host_time_clock().time() };
    HostTime(nanos(time))
}

/// A `CMTime` in nanoseconds; invalid or negative times are 0.
pub(crate) fn nanos(time: CMTime) -> u64 {
    if time.timescale <= 0 || time.value < 0 {
        return 0;
    }
    (time.value as i128 * 1_000_000_000 / time.timescale as i128) as u64
}

/// ScreenCaptureKit and AVFoundation objects that objc2 does not mark `Send`.
struct Shared<T>(T);
// SAFETY: the wrapped objects (SCStream, SCShareableContent, AVCaptureSession, the output
// objects) are documented as usable from any thread for the calls made here; each one is used
// by one thread at a time, behind `&mut self` of the stream that owns it.
unsafe impl<T> Send for Shared<T> {}

/// What the output objects hand buffers to.
struct OutputIvars {
    sink: Arc<dyn StreamSink>,
    format: StreamPixelFormat,
    color_space: ColorSpace,
    /// Set by whichever comes first, the stop or a failure, so the sink hears of the end once.
    ended: Arc<AtomicBool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `StreamOutput` has no `Drop`. The
    // ivars are `Send + Sync` (an `Arc` to a `Send + Sync` sink, plain values and an atomic), as
    // ScreenCaptureKit calls this object on its own queue.
    #[unsafe(super(NSObject))]
    #[name = "GlimpsfyRecordingStreamOutput"]
    #[ivars = OutputIvars]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            let ivars = self.ivars();
            if kind == SCStreamOutputType::Screen {
                deliver_video(ivars, sample);
            } else if kind == SCStreamOutputType::Audio {
                deliver_audio(&*ivars.sink, sample, AudioSource::System);
            } else if kind == SCStreamOutputType::Microphone {
                deliver_audio(&*ivars.sink, sample, AudioSource::Microphone);
            }
        }
    }

    unsafe impl SCStreamDelegate for StreamOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            let ivars = self.ivars();
            if !ivars.ended.swap(true, Ordering::SeqCst) {
                ivars.sink.stopped(Some(sc_error(Some(error))));
            }
        }
    }
);

impl StreamOutput {
    fn new(ivars: OutputIvars) -> Retained<StreamOutput> {
        let this = StreamOutput::alloc().set_ivars(ivars);
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// The receiver of the microphone's buffers from `AVCaptureSession`.
struct MicrophoneIvars {
    sink: Arc<dyn StreamSink>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `MicrophoneOutput` has no `Drop`.
    // The ivar is an `Arc` to a `Send + Sync` sink, as AVFoundation calls this object on the
    // queue given to it.
    #[unsafe(super(NSObject))]
    #[name = "GlimpsfyRecordingMicrophoneOutput"]
    #[ivars = MicrophoneIvars]
    struct MicrophoneOutput;

    unsafe impl NSObjectProtocol for MicrophoneOutput {}

    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for MicrophoneOutput {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(&self, _output: &AVCaptureOutput, sample: &CMSampleBuffer, _connection: &AVCaptureConnection) {
            deliver_audio(&*self.ivars().sink, sample, AudioSource::Microphone);
        }
    }
);

impl MicrophoneOutput {
    fn new(sink: Arc<dyn StreamSink>) -> Retained<MicrophoneOutput> {
        let this = MicrophoneOutput::alloc().set_ivars(MicrophoneIvars { sink });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// Hands a complete frame to the sink as its retained pixel buffer. Idle and blank frames carry
/// no new picture (ScreenCaptureKit sends them when nothing on screen changed) and are skipped.
fn deliver_video(ivars: &OutputIvars, sample: &CMSampleBuffer) {
    if frame_status(sample).is_some_and(|status| status != SCFrameStatus::Complete) {
        return;
    }
    // SAFETY: `sample` is valid for the duration of the callback; the image buffer is retained.
    let Some(image) = (unsafe { sample.image_buffer() }) else { return };
    let size = PhysicalSize::new(CVPixelBufferGetWidth(&image) as u32, CVPixelBufferGetHeight(&image) as u32);
    // SAFETY: as above.
    let timestamp = HostTime(nanos(unsafe { sample.presentation_time_stamp() }));
    ivars.sink.video(VideoFrame {
        timestamp,
        size,
        format: ivars.format,
        color_space: ivars.color_space,
        buffer: VideoBuffer::Native(NativeBuffer(Box::new(MacPixelBuffer(image)))),
    });
}

/// The pixels of a delivered frame of a `Bgra` stream, copied out of its pixel buffer.
pub(crate) fn cpu_frame(frame: &VideoFrame) -> Result<Frame> {
    let VideoBuffer::Native(native) = &frame.buffer else {
        return Err(CaptureError::InvalidArgument("the frame is not a pixel buffer".into()));
    };
    let Some(MacPixelBuffer(buffer)) = native.0.downcast_ref::<MacPixelBuffer>() else {
        return Err(CaptureError::InvalidArgument("the frame is not a pixel buffer".into()));
    };
    if CVPixelBufferGetPixelFormatType(buffer) != kCVPixelFormatType_32BGRA {
        return Err(CaptureError::InvalidArgument("the stream delivers NV12, not BGRA".into()));
    }
    // SAFETY: the buffer is retained by the frame; a read-only lock is released below.
    let status = unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    if status != 0 {
        return Err(CaptureError::Backend(format!("the pixel buffer could not be locked ({status})")));
    }
    let (width, height) = (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer));
    let stride = CVPixelBufferGetBytesPerRow(buffer);
    let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>().cast_const();
    let row = width * 4;
    let mut data = Vec::with_capacity(row * height);
    if !base.is_null() && stride >= row {
        for y in 0..height {
            // SAFETY: the locked buffer holds `height` rows of `stride` bytes from `base`, and a
            // BGRA row is `width × 4` of them.
            data.extend_from_slice(unsafe { std::slice::from_raw_parts(base.add(y * stride), row) });
        }
    }
    // SAFETY: the lock taken above, with the same flags.
    unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    let color = ColorInfo { space: frame.color_space, icc_profile: None };
    Frame::new(PhysicalSize::new(width as u32, height as u32), row, PixelFormat::Bgra8, color, ScaleFactor::ONE, data)
}

/// The frame's `SCStreamFrameInfoStatus`, or `None` when the buffer carries no status.
fn frame_status(sample: &CMSampleBuffer) -> Option<SCFrameStatus> {
    // SAFETY: `sample` is valid for the call; the array is retained.
    let array = unsafe { sample.sample_attachments_array(false) }?;
    // SAFETY: CFArray is toll-free bridged with NSArray, and the array lives while `array` does.
    let array: &NSArray<NSObject> = unsafe { &*(CFRetained::as_ptr(&array).as_ptr() as *const NSArray<NSObject>) };
    let first = array.firstObject()?;
    if !first.isKindOfClass(NSDictionary::<AnyObject, AnyObject>::class()) {
        return None;
    }
    // SAFETY: checked above to be a dictionary; its keys are strings (the attachment keys).
    let dict: &NSDictionary<NSString, AnyObject> =
        unsafe { &*(Retained::as_ptr(&first) as *const NSDictionary<NSString, AnyObject>) };
    // SAFETY: a constant string of ScreenCaptureKit that lives for the whole process.
    let key = unsafe { SCStreamFrameInfoStatus };
    let status = dict.objectForKey(key)?.downcast::<NSNumber>().ok()?;
    Some(SCFrameStatus(status.integerValue()))
}

/// Converts an audio sample buffer to interleaved 32-bit float and hands it to the sink.
fn deliver_audio(sink: &dyn StreamSink, sample: &CMSampleBuffer, source: AudioSource) {
    if let Some(buffer) = audio_buffer(sample, source) {
        sink.audio(buffer);
    }
}

fn audio_buffer(sample: &CMSampleBuffer, source: AudioSource) -> Option<AudioBuffer> {
    // SAFETY: `sample` is valid for the duration of the callback.
    let description = unsafe { sample.format_description() }?;
    // SAFETY: the description is an audio one (or the call returns null), valid while held.
    let asbd: AudioStreamBasicDescription =
        unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&description).as_ref() }.copied()?;
    if asbd.mFormatID != kAudioFormatLinearPCM || asbd.mChannelsPerFrame == 0 {
        return None;
    }
    let mut needed = 0usize;
    // SAFETY: asking only for the size of the buffer list; the other out pointers are null.
    let status = unsafe {
        sample.audio_buffer_list_with_retained_block_buffer(
            &mut needed,
            std::ptr::null_mut(),
            0,
            None,
            None,
            0,
            std::ptr::null_mut(),
        )
    };
    if status != 0 || needed < std::mem::size_of::<AudioBufferList>() {
        return None;
    }
    // A u64 vector keeps the list aligned for its pointers.
    let mut storage = vec![0u64; needed.div_ceil(8)];
    let list = storage.as_mut_ptr().cast::<AudioBufferList>();
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: `list` points at `needed` writable, aligned bytes, and `block` receives a retained
    // block buffer that owns the data the list points into.
    let status = unsafe {
        sample.audio_buffer_list_with_retained_block_buffer(
            std::ptr::null_mut(),
            list,
            needed,
            None,
            None,
            kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
            &mut block,
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: the call above returned the block buffer retained; this releases it when dropped,
    // after the samples were read.
    let _block = NonNull::new(block).map(|b| unsafe { CFRetained::from_raw(b) });
    // SAFETY: the list was filled by the call above: `mNumberBuffers` entries follow its header,
    // within the `needed` bytes it asked for.
    let buffers: &[CoreAudioBuffer] = unsafe {
        let count = (*list).mNumberBuffers as usize;
        let max = (needed - std::mem::offset_of!(AudioBufferList, mBuffers)) / std::mem::size_of::<CoreAudioBuffer>();
        std::slice::from_raw_parts(std::ptr::addr_of!((*list).mBuffers).cast::<CoreAudioBuffer>(), count.min(max))
    };
    let samples = interleave(&asbd, buffers)?;
    // SAFETY: as above.
    let timestamp = HostTime(nanos(unsafe { sample.presentation_time_stamp() }));
    Some(AudioBuffer {
        timestamp,
        source,
        sample_rate: asbd.mSampleRate.round() as u32,
        channels: asbd.mChannelsPerFrame.min(u16::MAX as u32) as u16,
        samples,
    })
}

/// The bytes of one Core Audio buffer.
fn bytes_of(buffer: &CoreAudioBuffer) -> &[u8] {
    if buffer.mData.is_null() {
        return &[];
    }
    // SAFETY: Core Audio filled `mDataByteSize` bytes at `mData`, owned by the retained block
    // buffer that outlives this borrow.
    unsafe { std::slice::from_raw_parts(buffer.mData.cast::<u8>(), buffer.mDataByteSize as usize) }
}

/// Reads linear PCM of any common layout into interleaved f32; `None` for a layout this does
/// not read (24-bit packed integers, which no Mac input delivers by default).
fn interleave(asbd: &AudioStreamBasicDescription, buffers: &[CoreAudioBuffer]) -> Option<Vec<f32>> {
    let float = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0;
    let signed = asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0;
    let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
    let bits = asbd.mBitsPerChannel;
    let width = (bits / 8) as usize;
    let read: fn(&[u8]) -> f32 = match (float, signed, bits) {
        (true, _, 32) => |b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
        (true, _, 64) => |b| f64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
        (false, true, 16) => |b| i16::from_ne_bytes([b[0], b[1]]) as f32 / 32_768.0,
        (false, true, 32) => |b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        _ => return None,
    };
    let channels = asbd.mChannelsPerFrame as usize;
    if planar {
        let planes: Vec<&[u8]> = buffers.iter().map(bytes_of).take(channels).collect();
        if planes.is_empty() {
            return None;
        }
        let frames = planes.iter().map(|p| p.len() / width).min().unwrap_or(0);
        let mut out = Vec::with_capacity(frames * planes.len());
        for i in 0..frames {
            for plane in &planes {
                out.push(read(&plane[i * width..(i + 1) * width]));
            }
        }
        // A list with fewer buffers than channels is read as that many channels; pad the rest
        // with silence so the channel count stays true.
        if planes.len() < channels {
            let have = planes.len();
            let mut padded = Vec::with_capacity(frames * channels);
            for frame in out.chunks_exact(have) {
                padded.extend_from_slice(frame);
                padded.extend(std::iter::repeat_n(0.0, channels - have));
            }
            return Some(padded);
        }
        Some(out)
    } else {
        let mut out = Vec::new();
        for buffer in buffers {
            let bytes = bytes_of(buffer);
            let whole = bytes.len() / (width * channels) * (width * channels);
            out.extend(bytes[..whole].chunks_exact(width).map(read));
        }
        Some(out)
    }
}

/// ScreenCaptureKit's error, or a generic one when it gave none.
fn sc_error(error: Option<&NSError>) -> CaptureError {
    let Some(error) = error else {
        return CaptureError::Backend("ScreenCaptureKit returned nothing".into());
    };
    if error.code() == SCStreamErrorCode::UserDeclined.0 {
        return CaptureError::PermissionDenied;
    }
    CaptureError::Backend(error.localizedDescription().to_string())
}

/// Waits for a ScreenCaptureKit completion handler that takes an optional error.
fn wait_for(call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>), what: &str) -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Result<()>>(1);
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the error, when there is one, is valid for the duration of the handler.
        let result = match unsafe { error.as_ref() } {
            None => Ok(()),
            Some(error) => Err(sc_error(Some(error))),
        };
        let _ = tx.send(result);
    });
    call(&handler);
    rx.recv_timeout(TIMEOUT)
        .map_err(|_| CaptureError::Backend(format!("{what} did not answer within {} s", TIMEOUT.as_secs())))?
}

/// Everything ScreenCaptureKit lists, displays and windows of every app included.
fn fetch_content() -> Result<Retained<SCShareableContent>> {
    let (tx, rx) = mpsc::sync_channel::<Result<Shared<Retained<SCShareableContent>>>>(1);
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: the content and the error, when there are any, are valid for the call; the
        // content is retained here.
        let result = match unsafe { Retained::retain(content) } {
            Some(content) => Ok(Shared(content)),
            None => Err(sc_error(unsafe { error.as_ref() })),
        };
        let _ = tx.send(result);
    });
    // SAFETY: the handler is retained by ScreenCaptureKit until it runs. Windows that are off
    // screen are listed too, so this app is found even when only its hidden windows exist.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, false, &handler,
        );
    }
    let Shared(content) =
        rx.recv_timeout(CONTENT_TIMEOUT).map_err(|_| CaptureError::Timeout(CONTENT_TIMEOUT.as_millis() as u64))??;
    Ok(content)
}

fn display_by_id(content: &SCShareableContent, id: DisplayId) -> Result<Retained<SCDisplay>> {
    // SAFETY: reading the immutable lists of fetched content.
    unsafe { content.displays() }
        .iter()
        .find(|d| unsafe { d.displayID() } == id.0)
        .ok_or(CaptureError::DisplayNotFound(id))
}

fn windows_by_id(content: &SCShareableContent, ids: &[WindowId]) -> Vec<Retained<SCWindow>> {
    if ids.is_empty() {
        return Vec::new();
    }
    // SAFETY: reading the immutable lists of fetched content.
    unsafe { content.windows() }.iter().filter(|w| ids.contains(&WindowId(unsafe { w.windowID() }))).collect()
}

fn intersection(a: CGRect, b: CGRect) -> f64 {
    let w = (a.origin.x + a.size.width).min(b.origin.x + b.size.width) - a.origin.x.max(b.origin.x);
    let h = (a.origin.y + a.size.height).min(b.origin.y + b.size.height) - a.origin.y.max(b.origin.y);
    if w > 0.0 && h > 0.0 {
        w * h
    } else {
        0.0
    }
}

/// The display that shows most of `frame` (global points), and `frame` relative to it, clipped
/// to the display.
fn display_of(content: &SCShareableContent, frame: CGRect) -> Option<(Retained<SCDisplay>, LogicalRect)> {
    // SAFETY: reading the immutable lists of fetched content.
    let displays = unsafe { content.displays() };
    let display = displays.iter().max_by(|a, b| {
        let a = intersection(unsafe { a.frame() }, frame);
        let b = intersection(unsafe { b.frame() }, frame);
        a.total_cmp(&b)
    })?;
    // SAFETY: as above.
    let bounds = unsafe { display.frame() };
    let x0 = frame.origin.x.max(bounds.origin.x);
    let y0 = frame.origin.y.max(bounds.origin.y);
    let x1 = (frame.origin.x + frame.size.width).min(bounds.origin.x + bounds.size.width);
    let y1 = (frame.origin.y + frame.size.height).min(bounds.origin.y + bounds.size.height);
    if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
        return None;
    }
    let rect = LogicalRect { x: x0 - bounds.origin.x, y: y0 - bounds.origin.y, width: x1 - x0, height: y1 - y0 };
    Some((display, rect))
}

/// Pixels per point of a display, from Core Graphics, for when the filter does not say.
fn display_scale(id: u32) -> f64 {
    let bounds = CGDisplayBounds(id);
    let pixels = CGDisplayCopyDisplayMode(id).map(|mode| CGDisplayMode::pixel_width(Some(&mode)) as f64);
    match pixels {
        Some(pixels) if bounds.size.width > 0.0 && pixels > 0.0 => pixels / bounds.size.width,
        _ => 1.0,
    }
}

/// An even pixel count of at least 2, which video encoders need.
fn even(value: f64) -> usize {
    let v = value.max(2.0) as usize;
    (v & !1).max(2)
}

/// What the stream records, worked out once from the content: the filter and the size of the
/// region in points and its pixel scale.
struct Source {
    filter: Retained<SCContentFilter>,
    /// The recorded region, display-local, in points; `None` records the whole display.
    source_rect: Option<LogicalRect>,
    points: CGSize,
    scale: f64,
}

fn resolve(config: &StreamConfig) -> Result<Source> {
    let content = fetch_content()?;
    let (display, window_display) = match &config.target {
        StreamTarget::Display { display } | StreamTarget::Area { display, .. } => {
            (display_by_id(&content, *display)?, None)
        }
        StreamTarget::Window { window } => {
            let found =
                windows_by_id(&content, &[*window]).into_iter().next().ok_or(CaptureError::WindowNotFound(*window))?;
            // SAFETY: reading a property of a listed window.
            let frame = unsafe { found.frame() };
            let (display, rect) = display_of(&content, frame)
                .ok_or_else(|| CaptureError::InvalidArgument("the window is not on any display".into()))?;
            // SAFETY: reading a property of a listed display.
            let id = DisplayId(unsafe { display.displayID() });
            (display, Some((id, rect)))
        }
    };
    let plan = FilterPlan::new(config, window_display);
    let own_pid = std::process::id() as i32;
    // SAFETY: the filter initialisers take lists of objects from the same content.
    let filter = unsafe {
        if plan.window.is_some() {
            let mut ids = Vec::with_capacity(plan.shown_windows.len() + 1);
            ids.extend(plan.window);
            ids.extend(plan.shown_windows.iter().copied());
            let included = NSArray::from_retained_slice(&windows_by_id(&content, &ids));
            SCContentFilter::initWithDisplay_includingWindows(SCContentFilter::alloc(), &display, &included)
        } else {
            let own_app = content.applications().iter().find(|app| app.processID() == own_pid);
            match own_app.filter(|_| plan.exclude_own_app) {
                Some(app) => {
                    let apps = NSArray::from_retained_slice(&[app]);
                    let except = NSArray::from_retained_slice(&windows_by_id(&content, &plan.shown_windows));
                    SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                        SCContentFilter::alloc(),
                        &display,
                        &apps,
                        &except,
                    )
                }
                None => {
                    let excluded = NSArray::from_retained_slice(&windows_by_id(&content, &plan.excluded_windows));
                    SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &excluded)
                }
            }
        }
    };
    // SAFETY: reading properties of a listed display and of the filter just made.
    let (bounds, id, filter_scale) = unsafe { (display.frame(), display.displayID(), filter.pointPixelScale()) };
    let scale = if filter_scale > 0.0 { filter_scale as f64 } else { display_scale(id) };
    let points = match plan.source_rect {
        Some(rect) => CGSize::new(rect.width, rect.height),
        None => bounds.size,
    };
    Ok(Source { filter, source_rect: plan.source_rect, points, scale })
}

fn configuration(config: &StreamConfig, source: &Source) -> Retained<SCStreamConfiguration> {
    let (width, height) = match config.output_size {
        Some(size) => (even(size.width as f64), even(size.height as f64)),
        None => {
            let divisor = config.size_divisor.max(1) as f64;
            (even(source.points.width * source.scale / divisor), even(source.points.height * source.scale / divisor))
        }
    };
    // SAFETY: plain property setters on a configuration this function owns, with constant
    // strings that live for the whole process.
    unsafe {
        let sc = SCStreamConfiguration::new();
        sc.setWidth(width);
        sc.setHeight(height);
        if let Some(rect) = source.source_rect {
            sc.setSourceRect(CGRect::new(CGPoint::new(rect.x, rect.y), CGSize::new(rect.width, rect.height)));
        }
        sc.setScalesToFit(true);
        sc.setPreservesAspectRatio(true);
        sc.setCaptureResolution(SCCaptureResolutionType::Best);
        sc.setMinimumFrameInterval(CMTime::new(1, config.fps.clamp(1, 120) as i32));
        sc.setQueueDepth(config.queue_depth.clamp(3, 8) as isize);
        sc.setShowsCursor(config.shows_cursor);
        match config.pixel_format {
            StreamPixelFormat::Nv12 => {
                sc.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
                sc.setColorMatrix(kCVImageBufferYCbCrMatrix_ITU_R_709_2);
            }
            StreamPixelFormat::Bgra => sc.setPixelFormat(kCVPixelFormatType_32BGRA),
        }
        sc.setColorSpaceName(match config.color_space {
            ColorSpace::Srgb => kCGColorSpaceSRGB,
            ColorSpace::DisplayP3 => kCGColorSpaceDisplayP3,
        });
        sc.setCapturesAudio(config.capture_system_audio);
        sc.setExcludesCurrentProcessAudio(true);
        sc.setSampleRate(48_000);
        sc.setChannelCount(2);
        if let Some(microphone) = &config.microphone {
            if native_microphone(&sc) {
                sc.setCaptureMicrophone(true);
                if let Some(id) = &microphone.device_id {
                    sc.setMicrophoneCaptureDeviceID(Some(&NSString::from_str(id)));
                }
            }
        }
        sc
    }
}

/// Whether ScreenCaptureKit can capture the microphone itself (macOS 15 and later).
fn native_microphone(sc: &SCStreamConfiguration) -> bool {
    sc.respondsToSelector(sel!(setCaptureMicrophone:))
}

/// Hands the microphone's buffers of an `AVCaptureSession` to the stream's sink, so a recording
/// on macOS 14 has one sink as on later systems.
struct Forward {
    sink: Arc<dyn StreamSink>,
    ended: Arc<AtomicBool>,
}

impl StreamSink for Forward {
    fn video(&self, _frame: VideoFrame) {}

    fn audio(&self, buffer: AudioBuffer) {
        self.sink.audio(buffer);
    }

    fn stopped(&self, error: Option<CaptureError>) {
        // The stream reports its own stop; only a failure of the microphone ends it early.
        if let Some(error) = error {
            if !self.ended.swap(true, Ordering::SeqCst) {
                self.sink.stopped(Some(error));
            }
        }
    }
}

struct MacStream {
    stream: Shared<Retained<SCStream>>,
    /// Kept alive here: ScreenCaptureKit holds its delegate weakly.
    _output: Shared<Retained<StreamOutput>>,
    _queue: DispatchRetained<DispatchQueue>,
    sink: Arc<dyn StreamSink>,
    ended: Arc<AtomicBool>,
    source: Shared<Source>,
    microphone: Option<Box<dyn CaptureStream>>,
    running: bool,
}

/// Opens a stream for `config`, stopped; `start` begins delivering to `sink`.
pub fn open(config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
    if config.fps == 0 {
        return Err(CaptureError::InvalidArgument("the frame rate is 0".into()));
    }
    let sink: Arc<dyn StreamSink> = Arc::from(sink);
    let ended = Arc::new(AtomicBool::new(false));
    let source = resolve(&config)?;
    let sc = configuration(&config, &source);
    let native_mic = native_microphone(&sc);
    let output = StreamOutput::new(OutputIvars {
        sink: sink.clone(),
        format: config.pixel_format,
        color_space: config.color_space,
        ended: ended.clone(),
    });
    let delegate: &ProtocolObject<dyn SCStreamDelegate> = ProtocolObject::from_ref(&*output);
    // SAFETY: the filter and the configuration are valid; the delegate is kept alive by the
    // returned stream object for as long as the SCStream.
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &source.filter, &sc, Some(delegate))
    };
    let queue = DispatchQueue::new("app.glimpsfy.recording.stream", None);
    let mut kinds = vec![SCStreamOutputType::Screen];
    if config.capture_system_audio {
        kinds.push(SCStreamOutputType::Audio);
    }
    if config.microphone.is_some() && native_mic {
        kinds.push(SCStreamOutputType::Microphone);
    }
    let receiver: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
    for kind in kinds {
        let queue_ref: &DispatchQueue = &queue;
        // SAFETY: `addStreamOutput:type:sampleHandlerQueue:error:` takes an output object, an
        // NSInteger type and a serial dispatch queue (an Objective-C object), and returns a BOOL
        // with an NSError out parameter. It is called through `msg_send!` because the typed
        // binding needs a crate feature this crate does not enable.
        let added: std::result::Result<(), Retained<NSError>> = unsafe {
            msg_send![&*stream, addStreamOutput: receiver, type: kind, sampleHandlerQueue: queue_ref, error: _]
        };
        if let Err(error) = added {
            return Err(sc_error(Some(&error)));
        }
    }
    let microphone = match &config.microphone {
        Some(source) if !native_mic => {
            Some(open_microphone(source.clone(), Box::new(Forward { sink: sink.clone(), ended: ended.clone() }))?)
        }
        _ => None,
    };
    Ok(Box::new(MacStream {
        stream: Shared(stream),
        _output: Shared(output),
        _queue: queue,
        sink,
        ended,
        source: Shared(source),
        microphone,
        running: false,
    }))
}

impl CaptureStream for MacStream {
    fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        let stream = &self.stream.0;
        // SAFETY: the handler is retained by ScreenCaptureKit until it runs.
        wait_for(|handler| unsafe { stream.startCaptureWithCompletionHandler(Some(handler)) }, "starting the stream")?;
        self.running = true;
        if let Some(microphone) = self.microphone.as_mut() {
            if let Err(error) = microphone.start() {
                let _ = self.stop();
                return Err(error);
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Whether the stream had already stopped on its own (a failure the sink has heard of):
        // ScreenCaptureKit then answers the stop with an error, and the stop has happened anyway.
        let ended_on_its_own = self.ended.load(Ordering::SeqCst);
        let mut result = Ok(());
        if self.running {
            self.running = false;
            let stream = &self.stream.0;
            // SAFETY: the handler is retained by ScreenCaptureKit until it runs.
            result = wait_for(
                |handler| unsafe { stream.stopCaptureWithCompletionHandler(Some(handler)) },
                "stopping the stream",
            );
        }
        if let Some(microphone) = self.microphone.as_mut() {
            let _ = microphone.stop();
        }
        if !self.ended.swap(true, Ordering::SeqCst) {
            self.sink.stopped(None);
        }
        match result {
            Err(_) if ended_on_its_own => Ok(()),
            other => other,
        }
    }

    fn update(&mut self, config: StreamConfig) -> Result<()> {
        let sc = configuration(&config, &self.source.0);
        let stream = &self.stream.0;
        // SAFETY: the configuration is valid, and the handler is retained by ScreenCaptureKit
        // until it runs.
        wait_for(
            |handler| unsafe { stream.updateConfiguration_completionHandler(&sc, Some(handler)) },
            "updating the stream",
        )
    }
}

impl Drop for MacStream {
    fn drop(&mut self) {
        if self.running {
            let _ = self.stop();
        }
    }
}

struct MicrophoneStream {
    session: Shared<Retained<AVCaptureSession>>,
    /// Kept alive here: the data output holds its delegate weakly.
    _output: Shared<(Retained<AVCaptureAudioDataOutput>, Retained<MicrophoneOutput>)>,
    _queue: DispatchRetained<DispatchQueue>,
    sink: Arc<dyn StreamSink>,
    running: bool,
    ended: bool,
}

/// Opens the microphone `source` through `AVCaptureSession`, stopped.
pub fn open_microphone(source: MicrophoneSource, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
    // SAFETY: a constant string of AVFoundation that lives for the whole process.
    let media = unsafe { AVMediaTypeAudio }
        .ok_or_else(|| CaptureError::Backend("AVFoundation does not name the audio media type".into()))?;
    // SAFETY: a class method that only reads the app's privacy state.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status != AVAuthorizationStatus::Authorized {
        return Err(CaptureError::Backend("the microphone permission is not granted".into()));
    }
    // SAFETY: class methods that look a device up; both return `None` when there is none.
    let device = unsafe {
        match &source.device_id {
            Some(id) => AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id)),
            None => AVCaptureDevice::defaultDeviceWithMediaType(media),
        }
    }
    .ok_or_else(|| {
        CaptureError::InvalidArgument(match &source.device_id {
            Some(id) => format!("the microphone {id} is not connected"),
            None => "no microphone is connected".into(),
        })
    })?;
    // SAFETY: the device is valid; failure comes back as an NSError.
    let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
        .map_err(|error| CaptureError::Backend(error.localizedDescription().to_string()))?;
    let sink: Arc<dyn StreamSink> = Arc::from(sink);
    // SAFETY: a new session, configured before it runs. Without audio settings the output
    // delivers the device's own format, which `interleave` reads; setting a format the device
    // cannot give would raise an Objective-C exception instead of an error.
    unsafe {
        let session = AVCaptureSession::new();
        if !session.canAddInput(&input) {
            return Err(CaptureError::Backend("the microphone cannot be added to a capture session".into()));
        }
        session.addInput(&input);
        let output = AVCaptureAudioDataOutput::new();
        let delegate = MicrophoneOutput::new(sink.clone());
        let queue = DispatchQueue::new("app.glimpsfy.recording.microphone", None);
        output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&queue));
        if !session.canAddOutput(&output) {
            return Err(CaptureError::Backend("the microphone's audio cannot be read".into()));
        }
        session.addOutput(&output);
        Ok(Box::new(MicrophoneStream {
            session: Shared(session),
            _output: Shared((output, delegate)),
            _queue: queue,
            sink,
            running: false,
            ended: false,
        }))
    }
}

impl CaptureStream for MicrophoneStream {
    fn start(&mut self) -> Result<()> {
        if !self.running {
            // SAFETY: starting a configured session; it blocks until the device runs, which is
            // why callers start streams on a worker thread.
            unsafe { self.session.0.startRunning() };
            self.running = true;
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if self.running {
            // SAFETY: stopping a session this stream started.
            unsafe { self.session.0.stopRunning() };
            self.running = false;
        }
        if !self.ended {
            self.ended = true;
            self.sink.stopped(None);
        }
        Ok(())
    }

    fn update(&mut self, _config: StreamConfig) -> Result<()> {
        // A microphone has nothing to change while it runs; muting is the recorder's.
        Ok(())
    }
}

impl Drop for MicrophoneStream {
    fn drop(&mut self) {
        if self.running {
            let _ = self.stop();
        }
    }
}
