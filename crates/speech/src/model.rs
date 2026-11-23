//! The speech model: the reviewed table of the one file Whisper needs, and downloading, verifying,
//! reading and removing it in the app's data folder.
//!
//! # The reviewed table
//!
//! [`MODEL_FILE`] is the only download URL of the speech area. It pins a commit of the
//! repository, so the bytes behind it cannot change, and it carries the file's SHA-256 and size.
//! A change to the table is reviewed like code: the hash is taken from the LFS pointer Hugging
//! Face serves for the pinned commit and from a download of the file, and the licences are
//! checked.
//!
//! Reviewed on 2026-11-23: `ggml-large-v3-turbo-q5_0.bin` of `ggerganov/whisper.cpp` at commit
//! `5359861c739e955e79d9a303bcbc70fb988958b1` is 574,041,195 bytes; its LFS pointer
//! (`/raw/<commit>/ggml-large-v3-turbo-q5_0.bin`) and the `x-linked-etag` of its `/resolve/` URL
//! both name the SHA-256 below, and a download of the URL hashed to the same value. whisper.cpp,
//! which converted and quantised the weights, is MIT; OpenAI's Whisper weights are MIT.
//!
//! # Rules this module keeps
//!
//! - Nothing is downloaded unless the user asks: [`SpeechModelStore::download`] is the only
//!   function that fetches, and only the app's model download command calls it.
//! - Fetching, hashing into `<name>.part`, verifying and reading verified bytes are
//!   `glimpsfy-models`', which holds the one connection of every model area and accepts the URL of
//!   this table and redirects to [`ALLOWED_HOSTS`] and [`ALLOWED_HOST_SUFFIXES`] over HTTPS only.
//! - The engine never hands a path to whisper.cpp. It loads the bytes [`SpeechModelStore::read`]
//!   returns, whose hash is checked on exactly those bytes, so no file can be swapped between the
//!   check and the load.

use std::path::{Path, PathBuf};

use glimpsfy_models::{ModelFile, Reviewed};
use serde::Serialize;

use crate::{CancelToken, Result};

/// The model's id, which names its folder and is written into every transcript.
pub const MODEL_ID: &str = "whisper-large-v3-turbo-q5";

/// The model's name in the app; it is a product name and is not translated.
pub const TITLE: &str = "Whisper large-v3-turbo";

/// Whisper large-v3-turbo in whisper.cpp's format, quantised to 5 bits (q5_0).
pub const MODEL_FILE: ModelFile = ModelFile {
    name: "ggml-large-v3-turbo-q5_0.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3-turbo-q5_0.bin",
    sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    bytes: 574_041_195,
};

/// The hosts a download may reach. `huggingface.co` serves the URL and answers with a redirect
/// to its content network, whose host names change by region and all end in `.hf.co`.
pub const ALLOWED_HOSTS: &[&str] = &["huggingface.co"];
/// Host name endings a redirect may lead to; see [`ALLOWED_HOSTS`].
pub const ALLOWED_HOST_SUFFIXES: &[&str] = &[".hf.co"];

static FILES: [ModelFile; 1] = [MODEL_FILE];
static REVIEWED_FILES: [&ModelFile; 1] = [&FILES[0]];

/// The table as the fetch checks it.
pub fn reviewed() -> Reviewed<'static> {
    Reviewed { files: &REVIEWED_FILES, hosts: ALLOWED_HOSTS, host_suffixes: ALLOWED_HOST_SUFFIXES }
}

/// The speech model as Settings and the trim window list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModelInfo {
    pub id: String,
    pub title: String,
    /// The size of the download in bytes.
    pub bytes: u64,
    /// Whether the file is on disk with the size the table names. The hash is checked again
    /// when the engine loads it.
    pub installed: bool,
    /// The folder that holds the model.
    pub folder: String,
}

/// The folder that holds the downloaded models; the speech model lives in its own subfolder. The
/// app passes `<app data>/models`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechModelStore {
    root: PathBuf,
}

impl SpeechModelStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        SpeechModelStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The model's own folder, `<root>/whisper-large-v3-turbo-q5`.
    pub fn dir(&self) -> PathBuf {
        self.root.join(MODEL_ID)
    }

    /// Whether the model is on disk with the size the table names. Reads the file's size only.
    pub fn installed(&self) -> bool {
        glimpsfy_models::installed(&self.dir(), &FILES)
    }

    pub fn info(&self) -> SpeechModelInfo {
        SpeechModelInfo {
            id: MODEL_ID.to_string(),
            title: TITLE.to_string(),
            bytes: MODEL_FILE.bytes,
            installed: self.installed(),
            folder: self.dir().to_string_lossy().into_owned(),
        }
    }

    /// Downloads the model from the reviewed table into its folder, reporting `(received, total)`
    /// bytes. A file already on disk with the right hash is kept. Blocks; the user starts it,
    /// `cancel` stops it, and `online` is asked before any connection (the app's offline switch).
    pub fn download(
        &self,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &CancelToken,
        online: &dyn Fn() -> bool,
    ) -> Result<()> {
        glimpsfy_models::download(
            &self.dir(),
            MODEL_ID,
            &FILES,
            &reviewed(),
            progress,
            &|| cancel.is_cancelled(),
            online,
        )?;
        Ok(())
    }

    /// Deletes the downloaded model. Removing a model that is not there succeeds.
    pub fn remove(&self) -> Result<()> {
        Ok(glimpsfy_models::remove(&self.dir())?)
    }

    /// The model's bytes for the engine to load, with the hash checked on exactly these bytes. A
    /// file whose bytes differ from the table is deleted and fails with a checksum error.
    pub fn read(&self) -> Result<Vec<u8>> {
        Ok(glimpsfy_models::read_verified(&self.dir(), MODEL_ID, &MODEL_FILE)?)
    }
}
