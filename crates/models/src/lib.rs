//! Model files the app downloads when the user asks for them (the PP-OCR models of
//! `glimpsfy-ocr`, the speech model of `glimpsfy-speech`): the shape of a reviewed table, the one
//! network fetch, and storing, verifying and removing a model's files in its folder.
//!
//! # Rules this crate keeps for every table
//!
//! - [`download`] is the only function that fetches, and [`download::fetch`] the only code that
//!   opens a connection. It accepts the URLs of the caller's [`Reviewed`] table and redirects to
//!   its hosts over HTTPS, and nothing else, and it asks `online` first.
//! - A file streams into `<name>.part` while it is hashed, and is renamed into place only when its
//!   hash and size match the table. A mismatch, a failure and a cancel delete the part.
//! - [`verify`] hashes the files again before an engine loads them; a file that changed on disk is
//!   deleted, and the model counts as not downloaded.
//! - [`read_verified`] checks the hash of exactly the bytes it returns, so no file can be swapped
//!   between a check and a load.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

pub mod download;

/// One file of a model, as a reviewed table names it.
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

/// A reviewed table as the fetch checks it: every file it may download and the hosts a download
/// and its redirects may reach.
#[derive(Clone, Copy, Debug)]
pub struct Reviewed<'a> {
    pub files: &'a [&'a ModelFile],
    pub hosts: &'a [&'a str],
    /// Host name endings a redirect may lead to (a content network whose hosts change by region).
    pub host_suffixes: &'a [&'a str],
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The download failed; the text names the host's or the network's reason.
    #[error("the model download failed: {0}")]
    Download(String),
    /// The app is set to stay offline, so nothing was fetched.
    #[error("the app is offline, so the model was not downloaded")]
    Offline,
    /// A file of the model is not on disk.
    #[error("the model {0} is not downloaded")]
    Missing(String),
    /// A downloaded or stored file did not have the bytes the table names. It was deleted and is
    /// never loaded.
    #[error("the model {model} failed its checksum and was deleted")]
    ChecksumMismatch { model: String },
    #[error("the model download was cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, ModelError>;

/// Fetches one file of the table into the sink, reporting the bytes received so far.
type Fetch<'a> = dyn Fn(&ModelFile, &mut dyn Write, &mut dyn FnMut(u64), &dyn Fn() -> bool) -> Result<()> + 'a;

/// Downloads `files` of the reviewed table into `dir`, reporting `(received, total)` bytes. Files
/// already on disk with the right hash are kept. Blocks; the user starts it, `cancelled` stops it,
/// and `online` is asked before any connection. `model` names the model in errors.
pub fn download(
    dir: &Path,
    model: &str,
    files: &[ModelFile],
    reviewed: &Reviewed,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
    online: &dyn Fn() -> bool,
) -> Result<()> {
    let fetch =
        |file: &ModelFile, sink: &mut dyn Write, progress: &mut dyn FnMut(u64), cancelled: &dyn Fn() -> bool| {
            download::fetch(file, reviewed, sink, progress, cancelled, online)
        };
    download_with(dir, model, files, &fetch, progress, cancelled)
}

/// [`download`] with the fetching step passed in, so the checks after it can be tested without a
/// network.
fn download_with(
    dir: &Path,
    model: &str,
    files: &[ModelFile],
    fetch: &Fetch<'_>,
    progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    let mut done = 0u64;
    for file in files {
        if cancelled() {
            return Err(ModelError::Cancelled);
        }
        let path = dir.join(file.name);
        if matches(&path, file)? {
            done += file.bytes;
            progress(done, total);
            continue;
        }
        let part = dir.join(format!("{}.part", file.name));
        let written = (|| {
            let mut sink = Hashing::new(BufWriter::new(File::create(&part)?), file.bytes);
            fetch(file, &mut sink, &mut |n| progress(done + n, total), cancelled)?;
            let (bytes, hash) = sink.finish()?;
            if bytes != file.bytes || hash != file.sha256 {
                return Err(ModelError::ChecksumMismatch { model: format!("{model}/{}", file.name) });
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

/// Whether every file is in `dir` with the size the table names. Reads sizes only; the hashes
/// are checked by [`verify`] before a load.
pub fn installed(dir: &Path, files: &[ModelFile]) -> bool {
    files.iter().all(|f| fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.bytes))
}

/// The bytes of the model's files on disk, finished or partial, for Settings to show.
pub fn bytes_on_disk(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
        })
        .unwrap_or(0)
}

/// Checks the hash of every file before the model is loaded. A missing file fails with
/// [`ModelError::Missing`]; a file whose bytes differ from the table is deleted and fails with
/// [`ModelError::ChecksumMismatch`], so a damaged or replaced file is never loaded.
pub fn verify(dir: &Path, model: &str, files: &[ModelFile]) -> Result<()> {
    for file in files {
        let path = dir.join(file.name);
        if !path.is_file() {
            return Err(ModelError::Missing(model.to_string()));
        }
        if !matches(&path, file)? {
            let _ = fs::remove_file(&path);
            return Err(ModelError::ChecksumMismatch { model: format!("{model}/{}", file.name) });
        }
    }
    Ok(())
}

/// The bytes of one file, for an engine to load. The hash is checked on the very bytes that are
/// returned, so what the engine loads is what the table names even when the file was replaced
/// after [`verify`] ran. A mismatch deletes the file.
pub fn read_verified(dir: &Path, model: &str, file: &ModelFile) -> Result<Vec<u8>> {
    let path = dir.join(file.name);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(ModelError::Missing(model.to_string())),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() as u64 != file.bytes || hex(&Sha256::digest(&bytes)) != file.sha256 {
        let _ = fs::remove_file(&path);
        return Err(ModelError::ChecksumMismatch { model: format!("{model}/{}", file.name) });
    }
    Ok(bytes)
}

/// Deletes a model's folder. Removing a model that is not there succeeds.
pub fn remove(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

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
    let mut buf = vec![0u8; 1024 * 1024];
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

    /// Two small files with their real hashes, as a table would name them.
    const FILES: &[ModelFile] = &[
        ModelFile {
            name: "a.bin",
            url: "https://huggingface.co/t/m/resolve/0/a.bin",
            // SHA-256 of three bytes "abc".
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            bytes: 3,
        },
        ModelFile {
            name: "b.bin",
            url: "https://huggingface.co/t/m/resolve/0/b.bin",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            bytes: 3,
        },
    ];

    /// A fetch that writes the right number of bytes with the wrong content, as a tampered or
    /// corrupted download would.
    fn wrong_bytes(
        file: &ModelFile,
        sink: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
        _: &dyn Fn() -> bool,
    ) -> Result<()> {
        sink.write_all(&vec![0x42; file.bytes as usize])?;
        progress(file.bytes);
        Ok(())
    }

    #[test]
    fn a_download_with_the_wrong_checksum_is_deleted_and_never_loaded() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("m");

        let result = download_with(&folder, "m", FILES, &wrong_bytes, &mut |_, _| {}, &|| false);
        assert!(matches!(result, Err(ModelError::ChecksumMismatch { .. })), "{result:?}");

        // Neither the partial file nor a final file is left behind.
        let left: Vec<_> = fs::read_dir(&folder).expect("the model folder").filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "files left behind: {left:?}");
        assert!(!installed(&folder, FILES));
        assert!(matches!(verify(&folder, "m", FILES), Err(ModelError::Missing(_))));

        // A download cancelled in the middle of a file leaves nothing either.
        let asked = std::cell::Cell::new(0);
        let cancelled = download_with(
            &folder,
            "m",
            FILES,
            &|file, sink, _, cancelled| {
                sink.write_all(&b"abc"[..file.bytes as usize - 1])?;
                if cancelled() {
                    Err(ModelError::Cancelled)
                } else {
                    Ok(())
                }
            },
            &mut |_, _| {},
            &|| {
                asked.set(asked.get() + 1);
                asked.get() > 1
            },
        );
        assert!(matches!(cancelled, Err(ModelError::Cancelled)), "{cancelled:?}");
        let left: Vec<_> = fs::read_dir(&folder).expect("the model folder").filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "files left behind after a cancel: {left:?}");
    }

    #[test]
    fn a_stored_model_whose_bytes_changed_is_deleted_before_loading() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("m");
        // Files with the table's sizes but other bytes, as if replaced on disk after a download.
        fs::create_dir_all(&folder).expect("the model folder");
        for file in FILES {
            fs::write(folder.join(file.name), vec![0u8; file.bytes as usize]).expect("a file");
        }
        assert!(installed(&folder, FILES), "the size check alone passes");

        let result = verify(&folder, "m", FILES);
        assert!(matches!(result, Err(ModelError::ChecksumMismatch { .. })), "{result:?}");
        assert!(!folder.join(FILES[0].name).exists(), "the mismatching file was deleted");

        // The engine's own read checks the bytes it is about to load, so a file swapped after
        // `verify` passed is refused and deleted as well.
        let result = read_verified(&folder, "m", &FILES[1]);
        assert!(matches!(result, Err(ModelError::ChecksumMismatch { .. })), "{:?}", result.map(|b| b.len()));
        assert!(!folder.join(FILES[1].name).exists(), "the swapped file was deleted");

        // The right bytes pass.
        fs::write(folder.join(FILES[1].name), b"abc").expect("a file");
        assert_eq!(read_verified(&folder, "m", &FILES[1]).expect("verified bytes"), b"abc");
    }
}
