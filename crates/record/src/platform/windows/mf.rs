//! The Media Foundation plumbing the writer and the reader share: the entry points, looked up at
//! run time, the scopes that pair `CoInitializeEx` and `MFStartup` with their undo, the worker
//! thread every call runs on, locked buffers, media type attributes, and failures in the API's
//! own words.
//!
//! Media Foundation's free functions live in `mfplat.dll` and `mfreadwrite.dll`, which the N
//! editions of Windows ship without until the Media Feature Pack is installed. Linking them the
//! ordinary way would stop the whole app from starting there, so they are loaded on first use
//! and a missing DLL becomes an error the recorder can show. The COM interfaces need no such
//! care: their methods are reached through the objects these functions return.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc;
use std::sync::OnceLock;

use windows::core::{Interface, GUID, HRESULT, PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFByteStream, IMFMediaBuffer, IMFMediaType, IMFSample, IMFSinkWriter,
    IMFSourceReader, MFMediaType_Video, MFSTARTUP_NOSOCKET, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ASYNCMFT,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_REGISTER_TYPE_INFO,
    MF_FILEFLAGS_NONE, MF_FILE_ACCESSMODE, MF_FILE_FLAGS, MF_FILE_OPENMODE, MF_VERSION,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::Variant::{VT_I8, VT_LPWSTR, VT_UI8};

use crate::{RecordError, Result};

type Startup = unsafe extern "system" fn(u32, u32) -> HRESULT;
type Shutdown = unsafe extern "system" fn() -> HRESULT;
type CreateAttributes = unsafe extern "system" fn(*mut *mut c_void, u32) -> HRESULT;
type CreateObject = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type CreateMemoryBuffer = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type CreateFile =
    unsafe extern "system" fn(MF_FILE_ACCESSMODE, MF_FILE_OPENMODE, MF_FILE_FLAGS, PCWSTR, *mut *mut c_void) -> HRESULT;
type EnumTransforms = unsafe extern "system" fn(
    GUID,
    u32,
    *const MFT_REGISTER_TYPE_INFO,
    *const MFT_REGISTER_TYPE_INFO,
    *mut *mut *mut c_void,
    *mut u32,
) -> HRESULT;
type CreateSinkWriter = unsafe extern "system" fn(PCWSTR, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;
type CreateSourceReader = unsafe extern "system" fn(PCWSTR, *mut c_void, *mut *mut c_void) -> HRESULT;

/// Media Foundation's free functions, with the signatures `mfapi.h` and `mfreadwrite.h` give them.
pub struct Api {
    startup: Startup,
    shutdown: Shutdown,
    create_attributes: CreateAttributes,
    create_media_type: CreateObject,
    create_sample: CreateObject,
    create_memory_buffer: CreateMemoryBuffer,
    create_file: CreateFile,
    enum_transforms: EnumTransforms,
    create_sink_writer: CreateSinkWriter,
    create_source_reader: CreateSourceReader,
}

/// What the recorder says when Media Foundation cannot be loaded.
const MISSING: &str =
    "Media Foundation is not available on this Windows; Windows N editions need the Media Feature Pack to record";

/// The entry points, loaded once for the life of the process.
pub fn api() -> Result<&'static Api> {
    static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(|missing| RecordError::Encoder(format!("{MISSING} ({missing} is missing)")))
}

fn library(name: &str) -> std::result::Result<HMODULE, String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the name is a NUL-terminated UTF-16 string that outlives the call. The search is
    // limited to System32, so a DLL of the same name next to the app is never loaded. The module
    // is never freed, which keeps the entry points valid for the life of the process.
    unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|_| name.to_string())
}

/// The export `$name` of `$module` as a function pointer of type `$kind`.
macro_rules! entry {
    ($module:expr, $dll:literal, $name:literal, $kind:ty) => {{
        // SAFETY: the module handle comes from `LoadLibraryExW` and is never freed, and the name
        // is a NUL-terminated ASCII string.
        let found = unsafe { GetProcAddress($module, PCSTR(concat!($name, "\0").as_ptr())) };
        match found {
            // SAFETY: the export of this name has exactly the signature `$kind` spells out (see
            // the Media Foundation headers); a function pointer keeps its address when its type
            // is changed to another function pointer type of the same size.
            Some(found) => unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $kind>(found) },
            None => return Err(format!("{} in {}", $name, $dll)),
        }
    }};
}

fn load() -> std::result::Result<Api, String> {
    let platform = library("mfplat.dll")?;
    let readwrite = library("mfreadwrite.dll")?;
    Ok(Api {
        startup: entry!(platform, "mfplat.dll", "MFStartup", Startup),
        shutdown: entry!(platform, "mfplat.dll", "MFShutdown", Shutdown),
        create_attributes: entry!(platform, "mfplat.dll", "MFCreateAttributes", CreateAttributes),
        create_media_type: entry!(platform, "mfplat.dll", "MFCreateMediaType", CreateObject),
        create_sample: entry!(platform, "mfplat.dll", "MFCreateSample", CreateObject),
        create_memory_buffer: entry!(platform, "mfplat.dll", "MFCreateMemoryBuffer", CreateMemoryBuffer),
        create_file: entry!(platform, "mfplat.dll", "MFCreateFile", CreateFile),
        enum_transforms: entry!(platform, "mfplat.dll", "MFTEnumEx", EnumTransforms),
        create_sink_writer: entry!(readwrite, "mfreadwrite.dll", "MFCreateSinkWriterFromURL", CreateSinkWriter),
        create_source_reader: entry!(readwrite, "mfreadwrite.dll", "MFCreateSourceReaderFromURL", CreateSourceReader),
    })
}

/// A failed call as a `RecordError`, with the system's message when it has one and the code.
pub fn failed(what: &str, error: &windows::core::Error) -> RecordError {
    let code = error.code().0 as u32;
    let message = error.message();
    let message = message.trim_end();
    if message.is_empty() {
        RecordError::Encoder(format!("{what} failed with 0x{code:08X}"))
    } else {
        RecordError::Encoder(format!("{what} failed: {message} (0x{code:08X})"))
    }
}

/// The object a creating function wrote to `raw`, which the caller now owns.
fn owned<T: Interface>(what: &str, result: HRESULT, raw: *mut c_void) -> Result<T> {
    result.ok().map_err(|e| failed(what, &e))?;
    if raw.is_null() {
        return Err(RecordError::Encoder(format!("{what} returned nothing")));
    }
    // SAFETY: on success the function wrote one reference of the interface its documentation
    // names (and `T` is that interface), which is handed over here exactly once.
    Ok(unsafe { T::from_raw(raw) })
}

impl Api {
    pub fn attributes(&self, count: u32) -> Result<IMFAttributes> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the function writes one new reference to the pointer it is given.
        let result = unsafe { (self.create_attributes)(&mut raw, count) };
        owned("MFCreateAttributes", result, raw)
    }

    pub fn media_type(&self) -> Result<IMFMediaType> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: as in `attributes`.
        let result = unsafe { (self.create_media_type)(&mut raw) };
        owned("MFCreateMediaType", result, raw)
    }

    pub fn sample(&self) -> Result<IMFSample> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: as in `attributes`.
        let result = unsafe { (self.create_sample)(&mut raw) };
        owned("MFCreateSample", result, raw)
    }

    pub fn memory_buffer(&self, bytes: usize) -> Result<IMFMediaBuffer> {
        let bytes =
            u32::try_from(bytes).map_err(|_| RecordError::Encoder(format!("a {bytes}-byte buffer is too large")))?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: as in `attributes`.
        let result = unsafe { (self.create_memory_buffer)(bytes, &mut raw) };
        owned("MFCreateMemoryBuffer", result, raw)
    }

    /// A byte stream on the file at `path`.
    pub fn file(&self, access: MF_FILE_ACCESSMODE, open: MF_FILE_OPENMODE, path: &Path) -> Result<IMFByteStream> {
        let wide = wide_path(path);
        let mut raw = std::ptr::null_mut();
        // SAFETY: the path is a NUL-terminated UTF-16 string that outlives the call, and the
        // function writes one new reference to the pointer it is given.
        let result = unsafe { (self.create_file)(access, open, MF_FILEFLAGS_NONE, PCWSTR(wide.as_ptr()), &mut raw) };
        if let Err(error) = result.ok() {
            return Err(file_error(path, &error));
        }
        owned("MFCreateFile", result, raw)
    }

    /// A sink writer onto `bytes`; `url` only tells it which file it writes, the container comes
    /// from `MF_TRANSCODE_CONTAINERTYPE` in `attributes`.
    pub fn sink_writer(&self, url: &Path, bytes: &IMFByteStream, attributes: &IMFAttributes) -> Result<IMFSinkWriter> {
        let wide = wide_path(url);
        let mut raw = std::ptr::null_mut();
        // SAFETY: the URL outlives the call, the byte stream and the attributes are live objects
        // the call borrows (it takes its own references), and it writes one new reference.
        let result =
            unsafe { (self.create_sink_writer)(PCWSTR(wide.as_ptr()), bytes.as_raw(), attributes.as_raw(), &mut raw) };
        owned("MFCreateSinkWriterFromURL", result, raw)
    }

    /// A source reader on the file at `path`.
    pub fn source_reader(&self, path: &Path, attributes: &IMFAttributes) -> Result<IMFSourceReader> {
        if !path.is_file() {
            let missing = format!("{} does not exist", path.display());
            return Err(RecordError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, missing)));
        }
        let wide = wide_path(path);
        let mut raw = std::ptr::null_mut();
        // SAFETY: as in `sink_writer`.
        let result = unsafe { (self.create_source_reader)(PCWSTR(wide.as_ptr()), attributes.as_raw(), &mut raw) };
        if let Err(error) = result.ok() {
            return Err(failed(&format!("opening {} for reading", path.display()), &error));
        }
        owned("MFCreateSourceReaderFromURL", result, raw)
    }

    /// Whether an encoder from uncompressed video to `subtype` is registered (in hardware or
    /// software). HEVC has no encoder in the box on many systems; it comes with a GPU driver or
    /// the HEVC Video Extensions.
    pub fn has_video_encoder(&self, subtype: GUID) -> bool {
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
        let flags =
            (MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER).0
                as u32;
        let mut list: *mut *mut c_void = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: the type info outlives the call; a null input type matches any input. On
        // success the function hands over an array of `count` activation objects allocated with
        // `CoTaskMemAlloc`, which is released below.
        let result = unsafe {
            (self.enum_transforms)(MFT_CATEGORY_VIDEO_ENCODER, flags, std::ptr::null(), &output, &mut list, &mut count)
        };
        if result.is_err() || list.is_null() {
            return false;
        }
        for index in 0..count as usize {
            // SAFETY: the array holds `count` pointers, and each non-null one is a reference of
            // IMFActivate that this code owns and releases once by dropping it.
            unsafe {
                let raw = *list.add(index);
                if !raw.is_null() {
                    drop(IMFActivate::from_raw(raw));
                }
            }
        }
        // SAFETY: the array was allocated by the function with `CoTaskMemAlloc`; its entries are
        // released above and it is freed once.
        unsafe { CoTaskMemFree(Some(list as *const c_void)) };
        count > 0
    }
}

/// A failure to open a file as the `io::Error` behind it when it is a Win32 one (it exists, the
/// folder is gone, access is denied), so the recorder words it as a file problem.
fn file_error(path: &Path, error: &windows::core::Error) -> RecordError {
    let code = error.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        let io = std::io::Error::from_raw_os_error((code & 0xFFFF) as i32);
        return RecordError::Io(std::io::Error::new(io.kind(), format!("{}: {io}", path.display())));
    }
    failed(&format!("opening {}", path.display()), error)
}

/// `path` as a NUL-terminated UTF-16 string.
pub fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// The calling thread inside the multithreaded apartment until this is dropped. Every thread of
/// the backend is one this module starts, so nothing about the caller's apartment is assumed.
/// Declare it before the objects that live on the thread, so they are released first.
pub struct Apartment {
    entered: bool,
    /// The apartment belongs to its thread, so the scope must not move to another one.
    _thread: PhantomData<*const ()>,
}

impl Apartment {
    pub fn enter() -> Apartment {
        // SAFETY: the call initialises COM for the calling thread, a fresh thread started here,
        // so it never meets a single-threaded apartment; success (also "already initialised") is
        // paired with `CoUninitialize` in `drop`.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Apartment { entered, _thread: PhantomData }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: pairs with this thread's successful `CoInitializeEx` (the scope is not
            // `Send`, so it is dropped on the thread that made it).
            unsafe { CoUninitialize() };
        }
    }
}

/// Media Foundation started until this is dropped; `MFStartup` and `MFShutdown` are counted, so
/// every user holds one.
pub struct Platform {
    api: &'static Api,
}

impl Platform {
    pub fn start() -> Result<Platform> {
        let api = api()?;
        // SAFETY: the call has no preconditions besides a version, which is the one the headers
        // of these bindings define. Sockets are not needed for local files.
        unsafe { (api.startup)(MF_VERSION, MFSTARTUP_NOSOCKET) }.ok().map_err(|e| failed("MFStartup", &e))?;
        Ok(Platform { api })
    }

    pub fn api(&self) -> &'static Api {
        self.api
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful `MFStartup` in `start`; the threads of this backend
        // are not Media Foundation work queue threads, where the call is not allowed.
        let _ = unsafe { (self.api.shutdown)() };
    }
}

/// Runs `work` on a thread of its own inside the multithreaded apartment with Media Foundation
/// started, while the calling thread handles each message the work sends with `on_message`. The
/// caller's callbacks (a progress bar, the GIF encoder) need not be `Send`, and the caller's
/// apartment does not matter. When `on_message` fails, the work's next send fails, the work
/// stops, and that failure is returned.
pub fn on_worker<T: Send, M: Send>(
    name: &str,
    depth: usize,
    work: impl FnOnce(&Platform, &mpsc::SyncSender<M>) -> Result<T> + Send,
    mut on_message: impl FnMut(M) -> Result<()>,
) -> Result<T> {
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<M>(depth);
        let worker = std::thread::Builder::new()
            .name(format!("glimpsfy-{name}"))
            .spawn_scoped(scope, move || {
                let _apartment = Apartment::enter();
                let platform = Platform::start()?;
                work(&platform, &tx)
            })
            .map_err(|e| RecordError::Encoder(format!("could not start the {name} thread: {e}")))?;
        let mut failure = None;
        for message in rx.iter() {
            if let Err(error) = on_message(message) {
                failure = Some(error);
                break;
            }
        }
        // Dropping the receiver makes the worker's next send fail, which stops it.
        drop(rx);
        let result = worker.join().unwrap_or_else(|_| Err(RecordError::Encoder(format!("the {name} thread failed"))));
        match failure {
            Some(error) => Err(error),
            None => result,
        }
    })
}

/// `on_worker` for work that sends nothing.
pub fn on_thread<T: Send>(name: &str, work: impl FnOnce(&Platform) -> Result<T> + Send) -> Result<T> {
    on_worker::<T, ()>(name, 0, |platform, _| work(platform), |_| Ok(()))
}

pub fn set_u32(attributes: &IMFAttributes, key: &GUID, value: u32) -> Result<()> {
    // SAFETY: the key is a valid GUID the call reads; the object stores a copy of the value.
    unsafe { attributes.SetUINT32(key, value) }.map_err(|e| failed("setting an attribute", &e))
}

pub fn set_u64(attributes: &IMFAttributes, key: &GUID, value: u64) -> Result<()> {
    // SAFETY: as in `set_u32`.
    unsafe { attributes.SetUINT64(key, value) }.map_err(|e| failed("setting an attribute", &e))
}

pub fn set_guid(attributes: &IMFAttributes, key: &GUID, value: &GUID) -> Result<()> {
    // SAFETY: as in `set_u32`.
    unsafe { attributes.SetGUID(key, value) }.map_err(|e| failed("setting an attribute", &e))
}

pub fn get_u32(attributes: &IMFAttributes, key: &GUID) -> Option<u32> {
    // SAFETY: the key is a valid GUID; a missing attribute is an error, read as `None`.
    unsafe { attributes.GetUINT32(key) }.ok()
}

pub fn get_u64(attributes: &IMFAttributes, key: &GUID) -> Option<u64> {
    // SAFETY: as in `get_u32`.
    unsafe { attributes.GetUINT64(key) }.ok()
}

pub fn get_guid(attributes: &IMFAttributes, key: &GUID) -> Option<GUID> {
    // SAFETY: as in `get_u32`.
    unsafe { attributes.GetGUID(key) }.ok()
}

/// The raw bytes of a blob attribute, `None` when it is missing.
pub fn get_blob(attributes: &IMFAttributes, key: &GUID) -> Option<Vec<u8>> {
    // SAFETY: the key is a valid GUID; the call only reads the attribute's size.
    let size = unsafe { attributes.GetBlobSize(key) }.ok()? as usize;
    let mut blob = vec![0u8; size];
    let mut written = 0u32;
    // SAFETY: the slice is as long as the blob, and the call writes at most its length.
    unsafe { attributes.GetBlob(key, &mut blob, Some(&mut written as *mut u32)) }.ok()?;
    blob.truncate(written as usize);
    Some(blob)
}

/// A `PROPVARIANT` the system filled in, cleared (and whatever it points to freed) when dropped.
pub struct Variant(pub PROPVARIANT);

impl Variant {
    /// The value when it is an unsigned 64-bit integer (`VT_UI8`), as a duration is.
    pub fn u64(&self) -> Option<u64> {
        // SAFETY: the tag says which member of the union is valid; it is read only when it
        // names this one.
        unsafe {
            let inner = &self.0.Anonymous.Anonymous;
            (inner.vt == VT_UI8).then_some(inner.Anonymous.uhVal)
        }
    }

    /// The value when it is a string (`VT_LPWSTR`), as a device's name is.
    pub fn text(&self) -> Option<String> {
        // SAFETY: as in `u64`; a `VT_LPWSTR` member is a NUL-terminated string the variant owns
        // until it is cleared, which happens after this copy.
        unsafe {
            let inner = &self.0.Anonymous.Anonymous;
            if inner.vt != VT_LPWSTR || inner.Anonymous.pwszVal.is_null() {
                return None;
            }
            inner.Anonymous.pwszVal.to_string().ok()
        }
    }
}

impl Drop for Variant {
    fn drop(&mut self) {
        // SAFETY: the variant is valid (filled in by the system, or empty) and cleared once.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}

/// A position for `IMFSourceReader::SetCurrentPosition`: 100 ns units as `VT_I8`. It owns
/// nothing, so it needs no clearing.
pub fn position(hns: i64) -> PROPVARIANT {
    let mut value = PROPVARIANT::default();
    // SAFETY: a default PROPVARIANT is VT_EMPTY; setting the tag and the member it names gives a
    // valid VT_I8 value.
    unsafe {
        let inner = &mut *value.Anonymous.Anonymous;
        inner.vt = VT_I8;
        inner.Anonymous.hVal = hns;
    }
    value
}

/// A media buffer locked for direct access until this is dropped.
pub struct Locked<'a> {
    buffer: &'a IMFMediaBuffer,
    data: *mut u8,
    max: usize,
    current: usize,
}

impl<'a> Locked<'a> {
    pub fn lock(buffer: &'a IMFMediaBuffer) -> Result<Locked<'a>> {
        let mut data = std::ptr::null_mut();
        let (mut max, mut current) = (0u32, 0u32);
        // SAFETY: the call writes the buffer's address and its lengths to the pointers it is
        // given; a successful lock is undone once, in `drop`.
        unsafe { buffer.Lock(&mut data, Some(&mut max as *mut u32), Some(&mut current as *mut u32)) }
            .map_err(|e| failed("IMFMediaBuffer::Lock", &e))?;
        if data.is_null() {
            // SAFETY: the lock succeeded, so it is undone even though it gave no memory.
            let _ = unsafe { buffer.Unlock() };
            return Err(RecordError::Encoder("a media buffer had no memory".into()));
        }
        Ok(Locked { buffer, data, max: max as usize, current: current.min(max) as usize })
    }

    /// The valid bytes.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: while locked, the buffer's memory is `max` bytes at `data`, of which the first
        // `current` are valid; the borrow ends before `drop` unlocks it.
        unsafe { std::slice::from_raw_parts(self.data, self.current) }
    }

    /// The whole buffer, for filling it.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `bytes`; this lock is the only access to the memory while it is held.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.max) }
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful `Lock` in `lock`.
        let _ = unsafe { self.buffer.Unlock() };
    }
}

/// A 2-D media buffer locked in its native layout until this is dropped. The pitch is negative
/// for a bottom-up picture, whose first row is then the last one in memory.
pub struct Locked2D<'a> {
    buffer: &'a IMF2DBuffer,
    pub scanline0: *const u8,
    pub pitch: i32,
}

impl<'a> Locked2D<'a> {
    pub fn lock(buffer: &'a IMF2DBuffer) -> Result<Locked2D<'a>> {
        let mut scanline0 = std::ptr::null_mut();
        let mut pitch = 0i32;
        // SAFETY: the call writes the first row's address and the pitch to the pointers it is
        // given; a successful lock is undone once, in `drop`.
        unsafe { buffer.Lock2D(&mut scanline0, &mut pitch) }.map_err(|e| failed("IMF2DBuffer::Lock2D", &e))?;
        if scanline0.is_null() {
            // SAFETY: the lock succeeded, so it is undone even though it gave no memory.
            let _ = unsafe { buffer.Unlock2D() };
            return Err(RecordError::Encoder("a picture buffer had no memory".into()));
        }
        Ok(Locked2D { buffer, scanline0, pitch })
    }

    /// The memory of `rows` rows of `row_bytes` each, from the lowest address on, and the stride
    /// that walks it (`logic::Pixels` takes a negative stride the same way).
    pub fn rows(&self, rows: usize, row_bytes: usize) -> Option<(&[u8], isize)> {
        let step = self.pitch.unsigned_abs() as usize;
        if rows == 0 || step < row_bytes {
            return None;
        }
        let span = step.checked_mul(rows - 1)?.checked_add(row_bytes)?;
        let lowest = if self.pitch >= 0 {
            self.scanline0
        } else {
            // SAFETY: with a negative pitch, rows 1 and on lie below the first row in memory,
            // inside the buffer, so the last row's address is within the same allocation.
            unsafe { self.scanline0.sub(step * (rows - 1)) }
        };
        // SAFETY: a locked 2-D buffer of `rows` rows at this pitch spans `span` bytes from its
        // lowest row; the borrow ends before `drop` unlocks it.
        let memory = unsafe { std::slice::from_raw_parts(lowest, span) };
        Some((memory, self.pitch as isize))
    }
}

impl Drop for Locked2D<'_> {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful `Lock2D` in `lock`.
        let _ = unsafe { self.buffer.Unlock2D() };
    }
}
