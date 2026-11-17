//! The microphones and cameras a recording can use, and the live level meter of the setup's
//! microphone list.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use glimpsfy_capture::{AudioBuffer, CaptureError, CaptureStream, MicrophoneSource, StreamSink, VideoFrame};

use crate::RecordingDevices;

/// The microphones and cameras connected now, with stable ids and the permissions' state.
pub fn list() -> RecordingDevices {
    crate::platform::devices()
}

/// Opens each microphone of a list and keeps its current level, for the meters next to the
/// microphones in the setup. Dropping it closes them.
pub struct Meter {
    streams: Vec<Box<dyn CaptureStream>>,
    levels: Arc<Mutex<HashMap<String, Arc<AtomicU32>>>>,
}

struct LevelSink(Arc<AtomicU32>);

impl StreamSink for LevelSink {
    fn video(&self, _: VideoFrame) {}

    fn audio(&self, buffer: AudioBuffer) {
        let level = crate::audio::level(&buffer.samples);
        // The meter falls back slowly, as a level meter does, so a short peak stays readable.
        let old = f32::from_bits(self.0.load(Ordering::Relaxed));
        let shown = if level >= old { level } else { old * 0.85 + level * 0.15 };
        self.0.store(shown.to_bits(), Ordering::Relaxed);
    }

    fn stopped(&self, _: Option<CaptureError>) {
        self.0.store(0f32.to_bits(), Ordering::Relaxed);
    }
}

impl Meter {
    /// Opens the microphones with these ids; one that fails to open reads 0 and the rest go on.
    pub fn open(ids: &[String]) -> Meter {
        let levels = Arc::new(Mutex::new(HashMap::new()));
        let mut streams = Vec::new();
        for id in ids {
            let cell = Arc::new(AtomicU32::new(0));
            let source = MicrophoneSource { device_id: Some(id.clone()) };
            if let Ok(mut stream) = glimpsfy_capture::open_microphone(source, Box::new(LevelSink(cell.clone()))) {
                if stream.start().is_ok() {
                    streams.push(stream);
                }
            }
            levels.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), cell);
        }
        Meter { streams, levels }
    }

    /// The level of each microphone, 0 to 1.
    pub fn levels(&self) -> HashMap<String, f32> {
        self.levels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, cell)| (id.clone(), f32::from_bits(cell.load(Ordering::Relaxed))))
            .collect()
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        for stream in &mut self.streams {
            let _ = stream.stop();
        }
    }
}
