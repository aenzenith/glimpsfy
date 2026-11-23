//! Audio through WASAPI: what the default output device plays (loopback) and what a microphone
//! hears, each on a thread of its own that hands interleaved float buffers to the stream's sink.
//!
//! - The buffers keep the device's own sample rate; the recorder brings them to its own. More
//!   than two channels are mixed down to stereo here (see `downmix_to_stereo`).
//! - A packet's time is WASAPI's QPC position, the clock `host_now` reads, so audio lines up
//!   with the frames of Windows.Graphics.Capture.
//! - A loopback sends nothing while nothing plays; the thread fills that time with silence, so a
//!   take's system track and its meter go on (`SilenceFiller`).
//! - A device that disappears never ends the stream: the thread keeps trying to open it again
//!   and is silent meanwhile, as a macOS stream is when its microphone is unplugged. The default
//!   output and the default microphone are followed when the user switches them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{Error, GUID, PCWSTR};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

use super::sys::{self, api_error, Apartment};
use crate::error::{CaptureError, Result};
use crate::platform::windows_convert::{self as convert, SampleKind, SilenceFiller};
use crate::stream::{AudioBuffer, AudioSource, HostTime, StreamSink};

/// `WAVEFORMATEX.wFormatTag` values, and the sub-formats a `WAVE_FORMAT_EXTENSIBLE` names.
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// The capture buffer WASAPI keeps for the stream, in 100 ns units: half a second, so a thread
/// that is held up for a moment loses nothing.
const BUFFER_DURATION: i64 = 5_000_000;
/// How often the thread empties the buffer. Packets come every 10 ms.
const POLL: Duration = Duration::from_millis(10);
/// How often a missing device is tried again and the default device is compared.
const RETRY: Duration = Duration::from_secs(1);
/// How long opening a microphone may take before the stream's start gives up on it.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How far a packet's time may lie from the clock before it is taken for wrong.
const PLAUSIBLE: u64 = 2_000_000_000;
/// The format of the silence sent before any device was heard.
const DEFAULT_RATE: u32 = 48_000;
/// The longest stretch of silence in one buffer, in nanoseconds.
const SILENCE_CHUNK: u64 = 500_000_000;
/// The most silence sent for one gap, in nanoseconds.
const LONGEST_SILENCE: u64 = 2_000_000_000;

/// What a thread listens to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Endpoint {
    /// Everything the default output device plays.
    Loopback,
    /// A microphone by its MMDevice endpoint id, or the default one.
    Microphone(Option<String>),
}

impl Endpoint {
    fn source(&self) -> AudioSource {
        match self {
            Endpoint::Loopback => AudioSource::System,
            Endpoint::Microphone(_) => AudioSource::Microphone,
        }
    }

    /// Whether the device is "whatever the default is now", which the user can switch.
    fn follows_default(&self) -> bool {
        !matches!(self, Endpoint::Microphone(Some(_)))
    }
}

/// A running audio thread. Dropping it stops the thread and waits for it.
pub(super) struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub(super) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // The thread looks at the flag every `POLL`; a panic on it has nothing to report.
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts listening to `endpoint` and handing its buffers to `sink`.
///
/// A microphone that cannot be opened fails the start, since the user asked for it and would
/// otherwise record without it unknowingly. The loopback never fails: a machine without an
/// output device (many virtual machines) plays nothing, and the thread sends that silence.
pub(super) fn spawn(endpoint: Endpoint, sink: Arc<dyn StreamSink>) -> Result<Worker> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<()>>(1);
    let name = match endpoint {
        Endpoint::Loopback => "glimpsfy-system-audio",
        Endpoint::Microphone(_) => "glimpsfy-microphone",
    };
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || run(endpoint, sink, flag, ready_tx))
        .map_err(|error| CaptureError::Backend(format!("could not start the audio thread: {error}")))?;
    let mut worker = Worker { stop, thread: Some(thread) };
    match ready_rx.recv_timeout(OPEN_TIMEOUT) {
        Ok(Ok(())) => Ok(worker),
        Ok(Err(error)) => {
            worker.stop();
            Err(error)
        }
        Err(_) => {
            // The thread is stuck inside the audio service. It is told to stop and left behind
            // rather than joined, which would hang the caller with it.
            worker.stop.store(true, Ordering::SeqCst);
            worker.thread = None;
            Err(CaptureError::Timeout(OPEN_TIMEOUT.as_millis() as u64))
        }
    }
}

/// Whether `endpoint` names a device that is there now, without opening it. A stream's and a
/// microphone's `open` report a missing microphone this way, before anything starts.
pub(super) fn probe(endpoint: &Endpoint) -> Result<()> {
    let endpoint = endpoint.clone();
    sys::on_worker("audio-probe", OPEN_TIMEOUT, move || {
        let enumerator = enumerator().map_err(|error| api_error("opening the audio devices", &error))?;
        device(&enumerator, &endpoint).map(|_| ()).map_err(|error| open_error(&endpoint, &error))
    })
}

fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    // SAFETY: creating the system's device enumerator in the caller's apartment, which every
    // caller here has entered.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn device(enumerator: &IMMDeviceEnumerator, endpoint: &Endpoint) -> windows::core::Result<IMMDevice> {
    // SAFETY: the calls take constants or a NUL-terminated id and return a new object.
    unsafe {
        match endpoint {
            Endpoint::Loopback => enumerator.GetDefaultAudioEndpoint(eRender, eConsole),
            Endpoint::Microphone(None) => enumerator.GetDefaultAudioEndpoint(eCapture, eConsole),
            Endpoint::Microphone(Some(id)) => {
                let id = sys::wide(id);
                enumerator.GetDevice(PCWSTR(id.as_ptr()))
            }
        }
    }
}

/// The id of a device, which the record crate's device list shows and takes back.
fn device_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: the call returns a NUL-terminated string allocated for the caller, which is
    // copied and then freed with the allocator the documentation names.
    unsafe {
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text
    }
}

/// Why a device could not be opened, in words the app can show.
fn open_error(endpoint: &Endpoint, error: &Error) -> CaptureError {
    match endpoint {
        // Windows' privacy setting for the microphone refuses desktop apps with this code.
        Endpoint::Microphone(_) if error.code() == E_ACCESSDENIED => {
            CaptureError::Backend("the microphone permission is not granted".into())
        }
        Endpoint::Microphone(Some(id)) => {
            CaptureError::InvalidArgument(format!("the microphone {id} is not connected"))
        }
        Endpoint::Microphone(None) => CaptureError::InvalidArgument("no microphone is connected".into()),
        Endpoint::Loopback => api_error("opening the output device", error),
    }
}

/// The sample layout of an open device.
#[derive(Clone, Copy, Debug)]
struct Format {
    rate: u32,
    channels: usize,
    /// Bytes of one frame (one sample of every channel).
    frame_bytes: usize,
    kind: SampleKind,
    channel_mask: u32,
}

/// Reads the format WASAPI's mixer uses for a device.
///
/// # Safety
///
/// `format` must point at a `WAVEFORMATEX` followed by the `cbSize` bytes it declares, as
/// `IAudioClient::GetMixFormat` returns it.
unsafe fn read_format(format: *const WAVEFORMATEX) -> Option<Format> {
    // SAFETY: the caller guarantees a `WAVEFORMATEX`; the struct is packed, so it is copied out
    // without assuming alignment.
    let base = unsafe { std::ptr::read_unaligned(format) };
    let (tag, channels, rate, frame_bytes, bits, extra) =
        (base.wFormatTag, base.nChannels, base.nSamplesPerSec, base.nBlockAlign, base.wBitsPerSample, base.cbSize);
    let (float, channel_mask) = match tag {
        WAVE_FORMAT_PCM => (false, 0),
        WAVE_FORMAT_IEEE_FLOAT => (true, 0),
        // The extensible part is 22 bytes after the base struct.
        WAVE_FORMAT_EXTENSIBLE if extra >= 22 => {
            // SAFETY: the tag and `cbSize` say the allocation holds a `WAVEFORMATEXTENSIBLE`.
            let extensible = unsafe { std::ptr::read_unaligned(format.cast::<WAVEFORMATEXTENSIBLE>()) };
            let (sub_format, mask) = (extensible.SubFormat, extensible.dwChannelMask);
            if sub_format == SUBTYPE_IEEE_FLOAT {
                (true, mask)
            } else if sub_format == SUBTYPE_PCM {
                (false, mask)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let kind = SampleKind::of(float, bits)?;
    let (channels, frame_bytes) = (channels as usize, frame_bytes as usize);
    (channels > 0 && rate > 0 && frame_bytes == channels * kind.bytes()).then_some(Format {
        rate,
        channels,
        frame_bytes,
        kind,
        channel_mask,
    })
}

/// Frees the format `GetMixFormat` allocated.
struct MixFormat(*mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `GetMixFormat`, which allocates with the COM task
        // allocator, and is freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

/// One open, started device.
struct Client {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: Format,
    id: Option<String>,
    /// Where the next packet belongs if its own time cannot be trusted.
    expected: Option<u64>,
}

impl Client {
    fn open(enumerator: &IMMDeviceEnumerator, endpoint: &Endpoint) -> Result<Client> {
        let failed = |error: Error| open_error(endpoint, &error);
        let device = device(enumerator, endpoint).map_err(failed)?;
        let id = device_id(&device);
        // SAFETY: activating the audio client of a valid device without parameters.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(failed)?;
        // SAFETY: the call returns a format it allocated, which `MixFormat` frees.
        let mix = MixFormat(unsafe { client.GetMixFormat() }.map_err(failed)?);
        // SAFETY: `mix.0` is what `GetMixFormat` returned and is alive.
        let format = unsafe { read_format(mix.0) }
            .ok_or_else(|| CaptureError::Backend("the audio device mixes in a format this app cannot read".into()))?;
        let flags = match endpoint {
            Endpoint::Loopback => AUDCLNT_STREAMFLAGS_LOOPBACK,
            Endpoint::Microphone(_) => 0,
        };
        // SAFETY: a shared-mode stream in the mixer's own format, which every device accepts;
        // the format pointer is alive for the call. The stream is polled, so it has no event.
        unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_DURATION, 0, mix.0, None) }
            .map_err(failed)?;
        // SAFETY: asking an initialised client for its capture service, then starting it.
        let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(failed)?;
        unsafe { client.Start() }.map_err(failed)?;
        Ok(Client { client, capture, format, id, expected: None })
    }

    /// Hands every waiting packet to `emit`. An error means the device is gone or was
    /// reconfigured, and the client is to be opened again.
    fn drain(&mut self, source: AudioSource, emit: &mut dyn FnMut(AudioBuffer, u64)) -> windows::core::Result<()> {
        loop {
            // SAFETY: a plain query on the capture service.
            if unsafe { self.capture.GetNextPacketSize() }? == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags, mut position) = (0u32, 0u32, 0u64);
            // SAFETY: the out pointers live through the call. On success the service lends
            // `frames` frames at `data` until `ReleaseBuffer` below.
            unsafe { self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut position)) }?;
            let count = frames as usize;
            let samples = if count == 0 {
                Vec::new()
            } else if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                // The flag says the packet is silence and its bytes mean nothing.
                vec![0.0; count * self.format.channels]
            } else {
                // SAFETY: the service lent `frames` frames of `frame_bytes` bytes each at
                // `data`, valid until the buffer is released; the samples are copied out first.
                let bytes = unsafe { std::slice::from_raw_parts(data, count * self.format.frame_bytes) };
                convert::samples_to_f32(bytes, self.format.kind)
            };
            // SAFETY: gives back exactly the frames `GetBuffer` lent.
            unsafe { self.capture.ReleaseBuffer(frames) }?;
            if count == 0 {
                return Ok(());
            }

            let duration = count as u64 * 1_000_000_000 / self.format.rate as u64;
            let now = sys::host_now().0;
            // The position is the QPC time of the packet's first frame in 100 ns units. A device
            // that cannot say flags it, and a few drivers report nonsense without the flag.
            let stamped = sys::host_time_of(position as i64);
            let trusted = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0
                && position != 0
                && stamped.abs_diff(now) < PLAUSIBLE;
            let timestamp =
                if trusted { stamped } else { self.expected.unwrap_or_else(|| now.saturating_sub(duration)) };
            self.expected = Some(timestamp + duration);

            let (samples, channels) =
                convert::downmix_to_stereo(samples, self.format.channels, self.format.channel_mask);
            emit(
                AudioBuffer {
                    timestamp: HostTime(timestamp),
                    source,
                    sample_rate: self.format.rate,
                    channels,
                    samples,
                },
                duration,
            );
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: stopping a client this struct started; a device that is gone refuses, which
        // changes nothing.
        let _ = unsafe { self.client.Stop() };
    }
}

fn run(endpoint: Endpoint, sink: Arc<dyn StreamSink>, stop: Arc<AtomicBool>, ready: mpsc::SyncSender<Result<()>>) {
    // Declared first, so it is dropped after every COM object of this thread.
    let _apartment = Apartment::enter();
    let source = endpoint.source();
    let enumerator = match enumerator() {
        Ok(enumerator) => Some(enumerator),
        Err(error) if endpoint != Endpoint::Loopback => {
            let _ = ready.send(Err(api_error("opening the audio devices", &error)));
            return;
        }
        // Without the audio service nothing plays either: the system track is silence.
        Err(_) => None,
    };
    let mut client = match enumerator.as_ref().map(|enumerator| Client::open(enumerator, &endpoint)) {
        Some(Ok(client)) => Some(client),
        Some(Err(error)) if endpoint != Endpoint::Loopback => {
            let _ = ready.send(Err(error));
            return;
        }
        _ => None,
    };
    let _ = ready.send(Ok(()));

    let mut filler = (endpoint == Endpoint::Loopback).then(|| SilenceFiller::new(sys::host_now().0));
    let mut silence_format = client.as_ref().map(|client| (client.format.rate, client.format.channels.min(2) as u16));
    let mut checked = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(POLL);
        if let Some(open) = client.as_mut() {
            let drained = open.drain(source, &mut |buffer, duration| {
                if let Some(filler) = filler.as_mut() {
                    filler.heard(buffer.timestamp.0, duration);
                }
                sink.audio(buffer);
            });
            if drained.is_err() {
                // Unplugged, disabled or given a new format: closed here, opened again below.
                client = None;
                checked = Instant::now();
            }
        }
        if checked.elapsed() >= RETRY {
            checked = Instant::now();
            if let Some(enumerator) = enumerator.as_ref() {
                // The user switched the default device: what plays now plays there.
                let moved = endpoint.follows_default()
                    && client.as_ref().is_some_and(|open| {
                        let current = device(enumerator, &endpoint).ok().and_then(|device| device_id(&device));
                        current.is_some() && current != open.id
                    });
                if client.is_none() || moved {
                    client = Client::open(enumerator, &endpoint).ok();
                    if let Some(open) = client.as_ref() {
                        silence_format = Some((open.format.rate, open.format.channels.min(2) as u16));
                    }
                }
            }
        }
        if let Some(filler) = filler.as_mut() {
            if let Some((start, length)) = filler.gap(sys::host_now().0) {
                // After a long stall (the machine slept) only the end of the gap is sent; the
                // recorder fills what lies before a buffer with silence of its own.
                let skipped = length.saturating_sub(LONGEST_SILENCE);
                let (start, length) = (start + skipped, length - skipped);
                let (rate, channels) = silence_format.unwrap_or((DEFAULT_RATE, 2));
                send_silence(&*sink, source, start, length, rate, channels);
            }
        }
    }
}

/// Sends `length` nanoseconds of silence from `start`, in buffers of at most `SILENCE_CHUNK`.
fn send_silence(sink: &dyn StreamSink, source: AudioSource, start: u64, length: u64, rate: u32, channels: u16) {
    let mut sent = 0u64;
    while sent < length {
        let chunk = (length - sent).min(SILENCE_CHUNK);
        let frames = (chunk as u128 * rate as u128 / 1_000_000_000) as usize;
        if frames > 0 {
            sink.audio(AudioBuffer {
                timestamp: HostTime(start + sent),
                source,
                sample_rate: rate,
                channels,
                samples: vec![0.0; frames * channels as usize],
            });
        }
        sent += chunk;
    }
}
