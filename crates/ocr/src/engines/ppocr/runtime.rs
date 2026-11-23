//! The inference runtime of PP-OCR: RTen, written in Rust, loads a model's ONNX file and runs a
//! tensor through it. Everything that names `rten` lives here.
//!
//! RTen is compiled from source like any other crate, so nothing is downloaded at build time, no
//! native library is linked or loaded, and nothing opens a connection. It picks the processor's
//! vector instructions (AVX2, AVX-512, NEON) at run time and falls back to plain code, so no
//! processor is turned away.
//!
//! The engine first ran on ONNX Runtime 1.31 through `ort`. That runtime's static library for
//! Linux needs glibc 2.38 and the C++ library of GCC 14 (`__isoc23_strtol`,
//! `__cxa_call_terminate`), so it did not link on Ubuntu 22.04, the oldest Linux the app is built
//! for. RTen reads every sample with the same characters and a dense screen somewhat slower.

use std::sync::Arc;

use rten::{RunOptions, ThreadPool};
use rten_tensor::prelude::*;
use rten_tensor::Tensor;

use crate::{OcrError, Result};

/// The most threads a reading computes with. More would take the cores of everything else on a
/// workstation for a gain that the measurements no longer show.
const MAX_THREADS: usize = 8;

/// One model file, loaded and ready to run. The model itself is shared and runs from several
/// threads at once; each lane is a pool of threads that one run computes with, so runs on
/// separate lanes go side by side.
pub struct Model {
    model: rten::Model,
    lanes: Vec<Arc<ThreadPool>>,
}

/// How a model's work is spread over the processor.
#[derive(Clone, Copy, Debug)]
pub struct Threads {
    /// The runs that may go side by side.
    pub lanes: usize,
    /// The threads each run computes with.
    pub each: usize,
}

impl Threads {
    /// One lane with every thread, for the detector: it runs one large image, which the runtime
    /// splits well over its threads.
    pub fn one_lane() -> Threads {
        Threads { lanes: 1, each: budget() }
    }

    /// A lane per thread, for the recogniser: it runs many small lines, and the runtime splits
    /// one line over threads poorly (eight threads on one lane read a dense screen in twice the
    /// time that eight lanes of one thread take).
    pub fn lane_per_thread() -> Threads {
        Threads { lanes: budget(), each: 1 }
    }
}

impl Model {
    /// Loads a model from the bytes of an ONNX file whose hash the caller has checked.
    pub fn load(bytes: Vec<u8>, threads: Threads) -> Result<Model> {
        let model = rten::Model::load(bytes).map_err(failed)?;
        let lanes =
            (0..threads.lanes.max(1)).map(|_| Arc::new(ThreadPool::with_num_threads(threads.each.max(1)))).collect();
        Ok(Model { model, lanes })
    }

    /// How many runs may go side by side.
    pub fn lanes(&self) -> usize {
        self.lanes.len()
    }

    /// Runs one `[batch, channels, height, width]` tensor through the model on lane `lane` and
    /// hands the first output's shape and values to `read`.
    pub fn run<T>(
        &self,
        lane: usize,
        shape: [usize; 4],
        data: Vec<f32>,
        read: impl FnOnce(&[usize], &[f32]) -> Result<T>,
    ) -> Result<T> {
        // `Tensor::from_data` panics on a mismatch, so the size is checked here first.
        if shape.iter().product::<usize>() != data.len() {
            return Err(OcrError::Engine("a model input does not have the size of its shape".into()));
        }
        let pool = self.lanes.get(lane).ok_or_else(|| OcrError::Engine("the model has no such lane".into()))?;
        let input = Tensor::from_data(&shape[..], data);
        let options = RunOptions::default().with_thread_pool(Some(Arc::clone(pool)));
        let output = self.model.run_one(input.into(), Some(options)).map_err(failed)?;
        let output: Tensor<f32> =
            output.try_into().map_err(|_| OcrError::Engine("a model output does not hold numbers".into()))?;
        let values = output.data().ok_or_else(|| OcrError::Engine("a model output is not contiguous".into()))?;
        read(output.shape(), values)
    }
}

/// The threads a reading may compute with: the machine's, up to [`MAX_THREADS`]. A reading is a
/// short burst that the user waits for, so it takes the cores it can use.
fn budget() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, MAX_THREADS)
}

fn failed(error: impl std::fmt::Display) -> OcrError {
    OcrError::Engine(error.to_string())
}
