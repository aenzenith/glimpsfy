//! Media Foundation's side of recording on Windows: the take's writer, reading recordings back
//! for export and the trim timeline, the devices, and the Recycle Bin.

pub mod devices;
mod logic;
mod mf;
pub mod reader;
pub mod writer;

use std::path::Path;

/// Moves `path` to the Recycle Bin the way the history does, so the user can still put it back. A file
/// that cannot be trashed stays where it is.
pub fn trash(path: &Path) -> std::io::Result<()> {
    use glimpsfy_history::Trash;
    glimpsfy_history::SystemTrash.trash(path)
}
