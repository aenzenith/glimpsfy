//! Moving a user's file to the Trash, the only way the history ever removes a file it does not own.

use std::io;
use std::path::Path;

/// Moves files to the Trash. The store holds one, and tests give it one that records the calls.
pub trait Trash: Send + Sync {
    /// Moves `path` to the Trash, where the user can still put it back.
    fn trash(&self, path: &Path) -> io::Result<()>;
}

/// The system's Trash: `NSFileManager trashItemAtURL:` on macOS, which needs no permission to
/// automate the Finder.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTrash;

impl Trash for SystemTrash {
    #[cfg(target_os = "macos")]
    fn trash(&self, path: &Path) -> io::Result<()> {
        use objc2_foundation::{NSFileManager, NSString, NSURL};

        if !path.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display())));
        }
        objc2::rc::autoreleasepool(|_| {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            NSFileManager::defaultManager()
                .trashItemAtURL_resultingItemURL_error(&url, None)
                .map_err(|error| io::Error::other(error.localizedDescription().to_string()))
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn trash(&self, path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("moving {} to the Trash is not supported on this platform yet", path.display()),
        ))
    }
}
