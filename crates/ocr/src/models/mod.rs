//! PP-OCR model files: the reviewed table of where they come from and what their bytes hash to,
//! and listing, downloading, verifying and removing them in the app's data folder.
//!
//! # The reviewed table
//!
//! [`MODELS`] is the only list of download URLs in the app. Every URL pins a commit of its
//! repository, so the bytes behind it cannot change, and every file carries its SHA-256 and its
//! size. A change to the table is reviewed like code: the new hash is taken from a download of the
//! file, compared with the hash the host publishes (Hugging Face shows the SHA-256 of every large
//! file), and the model's licence is checked (both repositories below are Apache-2.0).
//!
//! # Rules this module keeps
//!
//! - Nothing is downloaded unless the user asks: [`ModelStore::download`] is the only function
//!   that fetches, and only the model download command (a button in Settings) calls it.
//! - [`download::fetch`] is the only code in the OCR area that opens a connection. It accepts the
//!   URLs in the table and redirects to [`ALLOWED_HOSTS`] over HTTPS, and nothing else.
//! - A file streams into `<name>.part` while it is hashed, and is renamed into place only when its
//!   hash and size match the table. A mismatch deletes it.
//! - Before an engine loads a model, [`ModelStore::verified`] hashes the files again; a file that
//!   changed on disk is deleted, and the model counts as not downloaded.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{CancelToken, LanguageTag, OcrError, Result};

mod download;

/// One file of a model, as the reviewed table names it.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelFile {
    /// The file's name inside the model's folder.
    pub name: &'static str,
    /// Where it is downloaded from: an `https` URL that pins a commit.
    pub url: &'static str,
    /// The SHA-256 of the file's bytes, in lower-case hex.
    pub sha256: &'static str,
    pub bytes: u64,
}

/// One downloadable model: the files PP-OCR needs to read one script.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: &'static str,
    /// The model's name in Settings; it is a product name and is not translated.
    pub title: &'static str,
    /// The languages the recognition model reads, as BCP 47 tags.
    pub languages: &'static [&'static str],
    pub files: &'static [ModelFile],
}

/// The hosts a download may reach. `huggingface.co` serves the URLs in [`MODELS`] and answers with
/// a redirect to its content network, whose host names change by region and all end in `.hf.co`.
pub const ALLOWED_HOSTS: &[&str] = &["huggingface.co"];
/// Host name endings a redirect may lead to; see [`ALLOWED_HOSTS`].
pub const ALLOWED_HOST_SUFFIXES: &[&str] = &[".hf.co"];

/// The reviewed table of models. Reviewed on 2026-11-16: every hash below was computed from a
/// download of the pinned URL and matched the SHA-256 that Hugging Face publishes for the file.
pub const MODELS: &[ModelSpec] = &[ModelSpec {
    id: "ppocr-latin",
    title: "PP-OCR Latin",
    // The languages PaddleOCR lists for its PP-OCRv5 Latin recognition model.
    languages: &[
        "en", "tr", "de", "fr", "es", "it", "pt", "nl", "pl", "cs", "sk", "hr", "bs", "sr-Latn", "sl", "da", "no",
        "sv", "is", "et", "lt", "hu", "sq", "cy", "ga", "id", "ms", "af", "sw", "tl", "uz", "oc", "mi", "la",
    ],
    files: &[
        // PP-OCRv4 mobile text detection as RapidOCR converted it to ONNX; it finds the text
        // lines of every script.
        ModelFile {
            name: "det.onnx",
            url: "https://huggingface.co/SWHL/RapidOCR/resolve/1cfba2e90fc938db55889873735088de210cc173/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx",
            sha256: "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
            bytes: 4_745_517,
        },
        // PP-OCRv5 mobile Latin recognition (latin_PP-OCRv5_mobile_rec), which reads Turkish
        // letters such as ğ, ı, İ and ş.
        ModelFile {
            name: "rec.onnx",
            url: "https://huggingface.co/monkt/paddleocr-onnx/resolve/7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65/languages/latin/rec.onnx",
            sha256: "614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409",
            bytes: 7_862_832,
        },
        // The recognition model's alphabet, one character per line.
        ModelFile {
            name: "dict.txt",
            url: "https://huggingface.co/monkt/paddleocr-onnx/resolve/7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65/languages/latin/dict.txt",
            sha256: "3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff",
            bytes: 1_634,
        },
    ],
}];

/// The model in the table with this id.
pub fn spec(id: &str) -> Result<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id).ok_or_else(|| OcrError::ModelMissing(id.to_string()))
}

/// A model as Settings lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub title: String,
    pub languages: Vec<LanguageTag>,
    /// The size of the download in bytes.
    pub bytes: u64,
    /// Whether every file is on disk with the size the table names. The hashes are checked again
    /// when the engine loads the model.
    pub installed: bool,
}

/// How far a download is, reported while the files stream in.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub model: String,
    pub received: u64,
    pub total: u64,
}

/// The files of a model whose hashes were just checked, ready to load.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedModel {
    pub spec: &'static ModelSpec,
    pub dir: PathBuf,
}

impl VerifiedModel {
    /// The path of one of the model's files, by its name in the table.
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// The folder that holds the downloaded models, one subfolder per model id. The app passes
/// `<app data>/models`.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelStore {
    root: PathBuf,
}

static SHARED_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Remembers the app's model folder for engines created without one ([`crate::engine`]). The
/// first call wins; the folder does not change while the app runs.
pub fn set_shared_root(root: impl Into<PathBuf>) {
    let _ = SHARED_ROOT.set(root.into());
}

/// The model folder [`set_shared_root`] remembered, if it was called.
pub fn shared_store() -> Option<ModelStore> {
    SHARED_ROOT.get().map(ModelStore::new)
}

impl ModelStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ModelStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, spec: &ModelSpec) -> PathBuf {
        self.root.join(spec.id)
    }

    /// Every model in the table and whether it is downloaded. Reads file sizes only.
    pub fn list(&self) -> Vec<ModelInfo> {
        MODELS
            .iter()
            .map(|spec| {
                let dir = self.dir(spec);
                let installed = spec
                    .files
                    .iter()
                    .all(|f| fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.bytes));
                ModelInfo {
                    id: spec.id.to_string(),
                    title: spec.title.to_string(),
                    languages: spec.languages.iter().map(|l| LanguageTag::new(*l)).collect(),
                    bytes: spec.files.iter().map(|f| f.bytes).sum(),
                    installed,
                }
            })
            .collect()
    }

    /// Downloads the model's files from the reviewed table into its folder, reporting progress.
    /// Files already on disk with the right hash are kept. Blocks; the user starts it, and
    /// `cancel` stops it.
    pub fn download(&self, id: &str, progress: &mut dyn FnMut(Progress), cancel: &CancelToken) -> Result<()> {
        self.download_with(id, &download::fetch, progress, cancel)
    }

    /// [`ModelStore::download`] with the fetching step passed in, so the checks after it can be
    /// tested without a network.
    fn download_with(
        &self,
        id: &str,
        fetch: &Fetch,
        progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        let spec = spec(id)?;
        let dir = self.dir(spec);
        fs::create_dir_all(&dir)?;
        let total: u64 = spec.files.iter().map(|f| f.bytes).sum();
        let mut done = 0u64;
        for file in spec.files {
            cancel.check()?;
            let path = dir.join(file.name);
            if matches(&path, file)? {
                done += file.bytes;
                progress(Progress { model: id.to_string(), received: done, total });
                continue;
            }
            let part = dir.join(format!("{}.part", file.name));
            let written = (|| {
                let mut sink = Hashing::new(BufWriter::new(File::create(&part)?), file.bytes);
                fetch(
                    file,
                    &mut sink,
                    &mut |n| progress(Progress { model: id.to_string(), received: done + n, total }),
                    cancel,
                )?;
                let (bytes, hash) = sink.finish()?;
                if bytes != file.bytes || hash != file.sha256 {
                    return Err(OcrError::ChecksumMismatch { model: format!("{id}/{}", file.name) });
                }
                fs::rename(&part, &path)?;
                Ok(())
            })();
            if let Err(error) = written {
                // A partial or wrong file never stays behind, so nothing can load it later.
                let _ = fs::remove_file(&part);
                return Err(error);
            }
            done += file.bytes;
        }
        Ok(())
    }

    /// Deletes a downloaded model. Removing a model that is not there succeeds.
    pub fn remove(&self, id: &str) -> Result<()> {
        let spec = spec(id)?;
        match fs::remove_dir_all(self.dir(spec)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    /// Checks the hash of every file of the model before it is loaded. A missing file fails with
    /// [`OcrError::ModelMissing`]; a file whose bytes differ from the table is deleted and fails
    /// with [`OcrError::ChecksumMismatch`], so a damaged or replaced file is never loaded.
    pub fn verified(&self, id: &str) -> Result<VerifiedModel> {
        let spec = spec(id)?;
        let dir = self.dir(spec);
        for file in spec.files {
            let path = dir.join(file.name);
            if !path.is_file() {
                return Err(OcrError::ModelMissing(id.to_string()));
            }
            if !matches(&path, file)? {
                let _ = fs::remove_file(&path);
                return Err(OcrError::ChecksumMismatch { model: format!("{id}/{}", file.name) });
            }
        }
        Ok(VerifiedModel { spec, dir })
    }
}

/// Fetches one file of the table into the sink, reporting the bytes received so far.
type Fetch = dyn Fn(&ModelFile, &mut dyn Write, &mut dyn FnMut(u64), &CancelToken) -> Result<()>;

/// Whether the file on disk has the table's size and hash.
fn matches(path: &Path, file: &ModelFile) -> Result<bool> {
    match fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() == file.bytes => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let mut reader = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()) == file.sha256)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes through to a file while hashing, and refuses more bytes than the table names, so an
/// oversized answer stops early instead of filling the disk.
struct Hashing<W: Write> {
    inner: W,
    hasher: Sha256,
    written: u64,
    limit: u64,
}

impl<W: Write> Hashing<W> {
    fn new(inner: W, limit: u64) -> Self {
        Hashing { inner, hasher: Sha256::new(), written: 0, limit }
    }

    fn finish(mut self) -> io::Result<(u64, String)> {
        self.inner.flush()?;
        Ok((self.written, hex(&self.hasher.finalize())))
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > self.limit {
            return Err(io::Error::other("the server sent more bytes than the model table names"));
        }
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetch that writes the right number of bytes with the wrong content, as a tampered or
    /// corrupted download would.
    fn wrong_bytes(
        file: &ModelFile,
        sink: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
        _: &CancelToken,
    ) -> Result<()> {
        sink.write_all(&vec![0x42; file.bytes as usize])?;
        progress(file.bytes);
        Ok(())
    }

    #[test]
    fn a_download_with_the_wrong_checksum_is_deleted_and_never_loaded() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let store = ModelStore::new(dir.path());
        let model = MODELS[0].id;

        let result = store.download_with(model, &wrong_bytes, &mut |_| {}, &CancelToken::new());
        assert!(matches!(result, Err(OcrError::ChecksumMismatch { .. })), "{result:?}");

        // Neither the partial file nor a final file is left behind.
        let folder = dir.path().join(model);
        let left: Vec<_> = fs::read_dir(&folder).expect("the model folder").filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "files left behind: {left:?}");
        assert!(!store.list()[0].installed);
        assert!(matches!(store.verified(model), Err(OcrError::ModelMissing(_))));
    }

    #[test]
    fn a_stored_model_whose_bytes_changed_is_deleted_before_loading() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let store = ModelStore::new(dir.path());
        let spec = &MODELS[0];
        // Files with the table's sizes but other bytes, as if replaced on disk after a download.
        let folder = dir.path().join(spec.id);
        fs::create_dir_all(&folder).expect("the model folder");
        for file in spec.files {
            fs::write(folder.join(file.name), vec![0u8; file.bytes as usize]).expect("a file");
        }
        assert!(store.list()[0].installed, "the size check alone passes");

        let result = store.verified(spec.id);
        assert!(matches!(result, Err(OcrError::ChecksumMismatch { .. })), "{result:?}");
        assert!(!folder.join(spec.files[0].name).exists(), "the mismatching file was deleted");
    }
}
