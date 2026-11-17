//! The history's folder: the database, the thumbnails, the history's own copies of captures and
//! the editor's edits (a document and the image it draws on), and the one check that decides
//! whether a path is the history's to delete.
//!
//! The database stores the history's own files relative to this folder, so the folder can move
//! (a restored backup, a renamed app data directory) without breaking a row.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

const DATABASE: &str = "history.sqlite3";
const THUMBNAILS: &str = "thumbnails";
const CAPTURES: &str = "captures";
const EDITS: &str = "edits";
/// An edit's image sits beside its document (`<stem>.json`) as `<stem>.base`.
const EDIT_BASE: &str = "base";
/// Files being written carry this prefix until they are renamed to their final name.
pub(crate) const TEMP_PREFIX: &str = ".writing-";

#[derive(Clone, Debug)]
pub struct HistoryPaths {
    root: PathBuf,
}

impl HistoryPaths {
    /// The history's folder, usually `<app data dir>/history`. It is made when the store opens.
    pub fn new(root: impl Into<PathBuf>) -> HistoryPaths {
        HistoryPaths { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database(&self) -> PathBuf {
        self.root.join(DATABASE)
    }

    pub fn thumbnails(&self) -> PathBuf {
        self.root.join(THUMBNAILS)
    }

    pub fn captures(&self) -> PathBuf {
        self.root.join(CAPTURES)
    }

    /// The editor's edits: an entry's annotation document and the image its drawings sit on.
    pub fn edits(&self) -> PathBuf {
        self.root.join(EDITS)
    }

    /// The image an edit's document draws on, which lives beside the document.
    pub fn edit_base(document: &Path) -> PathBuf {
        document.with_extension(EDIT_BASE)
    }

    pub(crate) fn create(&self) -> io::Result<()> {
        fs::create_dir_all(self.thumbnails())?;
        fs::create_dir_all(self.edits())?;
        fs::create_dir_all(self.captures())
    }

    /// The folders whose files belong to the history and nothing else.
    pub(crate) fn own_folders(&self) -> [PathBuf; 3] {
        [self.thumbnails(), self.captures(), self.edits()]
    }

    /// How a path is written into the database: relative when it lies inside the history's
    /// folder, absolute otherwise.
    pub(crate) fn to_stored(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(relative) if is_plain_relative(relative) => relative.to_string_lossy().into_owned(),
            _ => path.to_string_lossy().into_owned(),
        }
    }

    /// The absolute path of a stored path.
    pub(crate) fn resolve(&self, stored: &str) -> PathBuf {
        let path = Path::new(stored);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }

    /// Whether `path` is a file inside one of the history's own folders, which the history may
    /// unlink. A path that leads elsewhere (through `..`, a symbolic link or a damaged row) is
    /// never the history's, whatever column it came from.
    pub(crate) fn owns(&self, path: &Path) -> bool {
        let Ok(real) = path.canonicalize() else {
            return false;
        };
        self.own_folders().iter().any(|folder| match folder.canonicalize() {
            Ok(folder) => real.starts_with(&folder) && real != folder && real.is_file(),
            Err(_) => false,
        })
    }

    /// Whether `path` lies anywhere inside the history's folder.
    pub(crate) fn contains(&self, path: &Path) -> bool {
        match (path.canonicalize(), self.root.canonicalize()) {
            (Ok(real), Ok(root)) => real.starts_with(root),
            _ => false,
        }
    }

    /// Writes `bytes` to `path` so that the file appears under its final name complete or not at
    /// all: a temporary file in the same folder is written, synced and renamed.
    pub(crate) fn write_atomically(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let folder = path.parent().ok_or_else(|| io::Error::other("the path has no folder"))?;
        let name = path.file_name().ok_or_else(|| io::Error::other("the path has no file name"))?;
        let temp = folder.join(format!("{TEMP_PREFIX}{}", name.to_string_lossy()));
        let result = (|| {
            let mut file = fs::File::create(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

fn is_plain_relative(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
}
