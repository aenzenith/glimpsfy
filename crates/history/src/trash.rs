//! Moving a user's file to the Trash, the only way the history ever removes a file it does not own.

use std::io;
use std::path::Path;

/// Moves files to the Trash. The store holds one, and tests give it one that records the calls.
pub trait Trash: Send + Sync {
    /// Moves `path` to the Trash, where the user can still put it back.
    fn trash(&self, path: &Path) -> io::Result<()>;
}

/// The system's Trash: `NSFileManager trashItemAtURL:` on macOS, which needs no permission to
/// automate the Finder, the Recycle Bin on Windows, and the freedesktop.org Trash of the file's
/// own mount on Linux. A file that cannot be trashed stays where it is and the reason comes back:
/// nothing here unlinks a file instead.
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

    #[cfg(windows)]
    fn trash(&self, path: &Path) -> io::Result<()> {
        if !path.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display())));
        }
        recycle_bin::trash(path)
    }

    /// The file goes to `$XDG_DATA_HOME/Trash` when it lies on the home's mount and to the
    /// `.Trash/$uid` or `.Trash-$uid` folder at the top of its own mount otherwise, with the
    /// `.trashinfo` that lets a file manager put it back. A mount whose top cannot hold a trash
    /// folder has its file copied whole into the home trash before the original goes, and a file
    /// neither trash takes (a read-only share) is refused.
    #[cfg(target_os = "linux")]
    fn trash(&self, path: &Path) -> io::Result<()> {
        if !path.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display())));
        }
        ::trash::delete(path).map_err(|error| match error {
            ::trash::Error::FileSystem { path, source } => {
                io::Error::new(source.kind(), format!("{}: {source}", path.display()))
            }
            ::trash::Error::Unknown { description } | ::trash::Error::Os { description, .. } => {
                io::Error::other(description)
            }
            ::trash::Error::CouldNotAccess { target } => io::Error::other(format!("{target} could not be reached")),
            ::trash::Error::CanonicalizePath { original } => {
                io::Error::other(format!("{} could not be resolved", original.display()))
            }
            ::trash::Error::TargetedRoot => io::Error::other("a root folder cannot go to the Trash"),
            other => io::Error::other(other.to_string()),
        })
    }

    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    fn trash(&self, path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("moving {} to the Trash is not supported on this platform yet", path.display()),
        ))
    }
}

/// The Recycle Bin, through the shell's `IFileOperation`.
///
/// The shell deletes a file for good when its drive has no Recycle Bin (a network share, most
/// removable drives) or the file is larger than the bin, and with its dialogs off it does so
/// without asking. The sink below is told before each delete whether the item will be recycled,
/// and stops the operation when it will not, so the file stays.
#[cfg(windows)]
mod recycle_bin {
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Path, Prefix};

    use windows::core::{implement, ComObject, Ref, HRESULT, PCWSTR};
    use windows::Win32::Foundation::E_ABORT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{
        FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
        SHCreateItemFromParsingName, FOFX_ADDUNDORECORD, FOFX_RECYCLEONDELETE, FOF_NO_UI,
        TSF_DELETE_RECYCLE_IF_POSSIBLE,
    };

    /// Recycles `path` on a thread of its own. The shell's objects need a single-threaded COM
    /// apartment, and the caller's thread may already live in another kind (which cannot be
    /// changed) or belong to a pool whose later work must not inherit one, so the apartment is
    /// entered and left on a thread nobody else uses.
    pub(super) fn trash(path: &Path) -> io::Result<()> {
        let path = path.to_path_buf();
        let worker = std::thread::Builder::new().name("glimpsfy-recycle-bin".into()).spawn(move || {
            let _apartment = Apartment::enter()?;
            recycle(&path)
        })?;
        worker.join().map_err(|_| io::Error::other("the thread that fills the Recycle Bin stopped"))?
    }

    /// The COM apartment of the current thread, left again when this is dropped.
    struct Apartment;

    impl Apartment {
        fn enter() -> io::Result<Apartment> {
            // SAFETY: the reserved pointer is null as the API asks, and every successful call is
            // paired with the `CoUninitialize` in `drop`.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                .ok()
                .map_err(shell_error)?;
            Ok(Apartment)
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: `enter` initialised COM on this thread, and every COM object of `recycle`
            // was released when that function returned.
            unsafe { CoUninitialize() }
        }
    }

    fn recycle(path: &Path) -> io::Result<()> {
        let name = shell_name(path)?;
        let sink = ComObject::new(Sink { unrecyclable: Cell::new(false), failure: Cell::new(None) });
        let events: IFileOperationProgressSink = sink.to_interface();
        // SAFETY: COM is initialised on this thread (`Apartment`), `name` is a NUL-terminated
        // UTF-16 string that outlives the call it is passed to, and the interfaces are used on
        // the thread that made them.
        let (performed, aborted) = unsafe {
            let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(shell_error)?;
            // No dialog may appear over the user's work, so every question is answered by the
            // sink instead.
            operation.SetOperationFlags(FOF_NO_UI | FOFX_ADDUNDORECORD | FOFX_RECYCLEONDELETE).map_err(shell_error)?;
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None).map_err(shell_error)?;
            operation.DeleteItem(&item, &events).map_err(shell_error)?;
            (operation.PerformOperations(), operation.GetAnyOperationsAborted())
        };
        if sink.unrecyclable.get() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "{} cannot go to the Recycle Bin (its drive has none, or the file is too large for it), so it was left where it is",
                    path.display()
                ),
            ));
        }
        if let Some(code) = sink.failure.get() {
            return Err(shell_error(code.into()));
        }
        performed.map_err(shell_error)?;
        if aborted.map_err(shell_error)?.as_bool() {
            return Err(io::Error::other(format!("the shell stopped before {} was recycled", path.display())));
        }
        Ok(())
    }

    /// The path in the form the shell parses: absolute, and without the `\\?\` prefix that long
    /// and canonical paths carry, which `SHCreateItemFromParsingName` does not accept.
    fn shell_name(path: &Path) -> io::Result<Vec<u16>> {
        let absolute = std::path::absolute(path)?;
        let mut components = absolute.components();
        let mut name = OsString::new();
        match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(letter) => name.push(format!("{}:", char::from(letter))),
                Prefix::VerbatimUNC(server, share) => {
                    name.push(r"\\");
                    name.push(server);
                    name.push(r"\");
                    name.push(share);
                }
                _ => name.push(prefix.as_os_str()),
            },
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} names no drive or share", path.display()),
                ))
            }
        }
        name.push(components.as_path().as_os_str());
        Ok(name.encode_wide().chain(std::iter::once(0)).collect())
    }

    fn shell_error(error: windows::core::Error) -> io::Error {
        // The shell's own codes (0x8027xxxx) have no system message, so the code is always shown.
        io::Error::other(format!("{} ({:#010x})", error.message(), error.code().0))
    }

    /// Listens to the one delete of an operation: refuses a delete that would not recycle, and
    /// keeps the shell's reason when the delete failed.
    #[implement(IFileOperationProgressSink)]
    struct Sink {
        unrecyclable: Cell<bool>,
        failure: Cell<Option<HRESULT>>,
    }

    // The method names are COM's.
    #[allow(non_snake_case)]
    impl IFileOperationProgressSink_Impl for Sink_Impl {
        fn StartOperations(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn FinishOperations(&self, _result: HRESULT) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreRenameItem(&self, _flags: u32, _item: Ref<'_, IShellItem>, _name: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostRenameItem(
            &self,
            _flags: u32,
            _item: Ref<'_, IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _renamed: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreMoveItem(
            &self,
            _flags: u32,
            _item: Ref<'_, IShellItem>,
            _destination: Ref<'_, IShellItem>,
            _name: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostMoveItem(
            &self,
            _flags: u32,
            _item: Ref<'_, IShellItem>,
            _destination: Ref<'_, IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _moved: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreCopyItem(
            &self,
            _flags: u32,
            _item: Ref<'_, IShellItem>,
            _destination: Ref<'_, IShellItem>,
            _name: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostCopyItem(
            &self,
            _flags: u32,
            _item: Ref<'_, IShellItem>,
            _destination: Ref<'_, IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _copy: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PreDeleteItem(&self, flags: u32, _item: Ref<'_, IShellItem>) -> windows::core::Result<()> {
            // Without this flag the shell is about to delete the item for good. An error here
            // cancels the delete, and the file stays.
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                self.unrecyclable.set(true);
                return Err(E_ABORT.into());
            }
            Ok(())
        }

        fn PostDeleteItem(
            &self,
            _flags: u32,
            _item: Ref<'_, IShellItem>,
            result: HRESULT,
            _recycled: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            if result.is_err() {
                self.failure.set(Some(result));
            }
            Ok(())
        }

        fn PreNewItem(
            &self,
            _flags: u32,
            _destination: Ref<'_, IShellItem>,
            _name: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn PostNewItem(
            &self,
            _flags: u32,
            _destination: Ref<'_, IShellItem>,
            _name: &PCWSTR,
            _template: &PCWSTR,
            _attributes: u32,
            _result: HRESULT,
            _created: Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn UpdateProgress(&self, _total: u32, _so_far: u32) -> windows::core::Result<()> {
            Ok(())
        }

        fn ResetTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn PauseTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn ResumeTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
    }
}
