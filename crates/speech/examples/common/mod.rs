//! What the examples share: the model folder the app uses, the model download on request, and
//! reading a WAV file as 16 kHz mono.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use glimpsfy_speech::audio::Downsampler;
use glimpsfy_speech::model::SpeechModelStore;
use glimpsfy_speech::CancelToken;

/// The app's model folder (`<app data>/models`), or `GLIMPSFY_MODELS` when it is set.
pub fn default_models() -> PathBuf {
    if let Some(dir) = std::env::var_os("GLIMPSFY_MODELS") {
        return dir.into();
    }
    let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let data = if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default())
    } else {
        std::env::var_os("XDG_DATA_HOME").map_or_else(|| home().join(".local/share"), PathBuf::from)
    };
    data.join("com.aenzenith.glimpsfy").join("models")
}

/// Downloads the model into the store unless it is there already. This is the only thing in the
/// examples that touches the network, and only when `--download` asks for it.
pub fn ensure_model(store: &SpeechModelStore, download: bool) {
    if store.installed() {
        return;
    }
    if !download {
        eprintln!("the speech model is not in {}; pass --download to fetch it (574 MB)", store.dir().display());
        std::process::exit(2);
    }
    let started = Instant::now();
    let mut last = 0;
    let result = store.download(
        &mut |received, total| {
            let percent = received * 100 / total.max(1);
            if percent >= last + 5 {
                last = percent;
                eprintln!("downloading the speech model: {percent} %");
            }
        },
        &CancelToken::new(),
        &|| true,
    );
    if let Err(error) = result {
        eprintln!("the download failed: {error}");
        std::process::exit(1);
    }
    eprintln!("downloaded and verified in {:.0} s", started.elapsed().as_secs_f64());
}

/// The samples of a WAV file of any rate and channel count, as 16 kHz mono.
pub fn read_wav(path: &Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).unwrap_or_else(|e| {
        eprintln!("{} cannot be read: {e}", path.display());
        std::process::exit(2)
    });
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    let mut downsampler = Downsampler::with_input(spec.sample_rate, spec.channels);
    let mut out = Vec::with_capacity(samples.len());
    // In buffers, as the app feeds the recorder's track.
    for chunk in samples.chunks(4096 * usize::from(spec.channels)) {
        downsampler.push(chunk, &mut out);
    }
    downsampler.finish(&mut out);
    out
}

/// Seconds with two decimals, for printing times.
pub fn seconds(ms: u64) -> String {
    format!("{:.2}", ms as f64 / 1000.0)
}
