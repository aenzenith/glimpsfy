//! GStreamer's side of recording on Linux: the take's writer, reading recordings back for export
//! and the trim timeline, the devices, and the Trash.

pub mod devices;
mod elements;
mod mp4;
pub mod reader;
pub mod writer;

use std::path::Path;

/// Moves `path` to the Trash the way the history does, so the user can still put it back. A file
/// that cannot be trashed stays where it is.
pub fn trash(path: &Path) -> std::io::Result<()> {
    use glimpsfy_history::Trash;
    glimpsfy_history::SystemTrash.trash(path)
}
