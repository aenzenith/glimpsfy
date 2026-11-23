//! The xdg-desktop-portal side of a Wayland session: the Screenshot portal for stills, the
//! ScreenCast portal for recordings, and the restore tokens that keep the system's picker from
//! appearing on every recording.
//!
//! `ashpd` is asynchronous. It runs here on zbus's own executor thread (the `async-io` flavour,
//! the one Tauri's plugins already use; switching zbus to tokio would switch it for them too),
//! and every call blocks its calling worker thread on the future with a time limit, so nothing
//! here needs a runtime and nothing can deadlock on one.

use std::collections::BTreeMap;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::screenshot::Screenshot;
use ashpd::desktop::{PersistMode, ResponseError, Session};
use ashpd::enumflags2::BitFlags;

use crate::error::{CaptureError, Result};
use crate::geometry::PhysicalSize;

/// How long one portal call may take when no person has to answer it.
const QUICK: Duration = Duration::from_secs(10);
/// How long the system's picker (or its permission question) may stay open.
const PICKER: Duration = Duration::from_secs(300);
/// The file under the state directory that holds the restore tokens.
const TOKENS_FILE: &str = "screencast-tokens";

static STATE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Sets the folder the restore tokens are kept in; without one they are not kept at all.
pub fn set_state_dir(path: Option<PathBuf>) {
    *STATE_DIR.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

fn state_dir() -> Option<PathBuf> {
    STATE_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Blocks the calling thread on `future` for at most `limit`.
fn wait<T>(limit: Duration, future: impl Future<Output = T>) -> Result<T> {
    async_io::block_on(futures_lite::future::or(async { Ok(future.await) }, async {
        async_io::Timer::after(limit).await;
        Err(CaptureError::Timeout(limit.as_millis() as u64))
    }))
}

/// An `ashpd` error in this crate's words: a refusal is the missing permission, a missing
/// portal says so, and everything else keeps the portal's own words.
fn portal_error(error: ashpd::Error, what: &str) -> CaptureError {
    match &error {
        ashpd::Error::Response(ResponseError::Cancelled) => return CaptureError::PermissionDenied,
        ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)) => return CaptureError::PermissionDenied,
        _ => {}
    }
    let words = error.to_string();
    let missing = ["ServiceUnknown", "NameHasNoOwner", "UnknownMethod", "UnknownInterface", "No such interface"]
        .iter()
        .any(|sign| words.contains(sign) || format!("{error:?}").contains(sign));
    if missing {
        CaptureError::Backend(format!(
            "this desktop has no portal for {what} (xdg-desktop-portal or its backend for this desktop is missing): {words}"
        ))
    } else {
        CaptureError::Backend(format!("the desktop portal failed at {what}: {words}"))
    }
}

/// The file under the state directory whose presence says the Screenshot portal answered before.
const SCREENSHOTS_FILE: &str = "screenshots-granted";

/// Whether an earlier run took a screenshot through the portal, so this run need not ask first.
pub(super) fn screenshots_worked_before() -> bool {
    state_dir().is_some_and(|dir| dir.join(SCREENSHOTS_FILE).is_file())
}

/// Remembers (or forgets, after a refusal) that the Screenshot portal answers this app.
pub(super) fn note_screenshots_work(works: bool) {
    let Some(dir) = state_dir() else { return };
    let path = dir.join(SCREENSHOTS_FILE);
    if works {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(path, b"");
    } else if path.is_file() {
        let _ = std::fs::remove_file(path);
    }
}

/// A decoded portal screenshot: tight BGRA rows of the whole desktop.
pub(super) struct Shot {
    pub size: PhysicalSize,
    pub data: Vec<u8>,
}

/// Takes a screenshot of the whole desktop through the Screenshot portal, without the portal's
/// own dialog. `limit` covers the permission question the desktop may show on the first call.
pub(super) fn screenshot(limit: Duration) -> Result<Shot> {
    let answer = wait(limit, async {
        let request = Screenshot::request().interactive(false).modal(false).send().await?;
        request.response()
    })?;
    let shot = answer.map_err(|e| portal_error(e, "screenshots"))?;
    let uri = shot.uri();
    if uri.scheme() != "file" {
        return Err(CaptureError::Backend(format!("the portal answered with {uri}, which is not a file")));
    }
    let path = uri
        .to_file_path()
        .map_err(|_| CaptureError::Backend(format!("the portal answered with {uri}, which names no local file")))?;
    let bytes = std::fs::read(&path);
    // The portal wrote this file for this one request (into the user's Pictures folder on most
    // desktops); it goes whether or not it could be read, so captures leave nothing behind.
    let _ = remove_portal_file(&path);
    let bytes = bytes.map_err(|e| {
        CaptureError::Backend(format!("the portal's screenshot {} could not be read: {e}", path.display()))
    })?;
    decode_png(&bytes)
}

/// Deletes the file the Screenshot portal wrote, and nothing else: the path must be absolute and
/// name a regular file. A folder, a symbolic link or anything that is not a file is left alone,
/// and nothing is ever removed recursively.
pub(super) fn remove_portal_file(path: &Path) -> std::io::Result<()> {
    let refuse = |why: &str| Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_string()));
    if !path.is_absolute() {
        return refuse("the path is not absolute");
    }
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return refuse("the path does not name a regular file");
    }
    std::fs::remove_file(path)
}

/// A PNG of any colour type as tight BGRA rows.
fn decode_png(bytes: &[u8]) -> Result<Shot> {
    let invalid = |e: png::DecodingError| CaptureError::Backend(format!("the portal's screenshot is not a PNG: {e}"));
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(invalid)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| CaptureError::Backend("the portal's screenshot is too large to decode".into()))?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(invalid)?;
    let (width, height) = (info.width as usize, info.height as usize);
    let channels = info.color_type.samples();
    let decoded = buffer.get(..width * height * channels).ok_or_else(|| {
        CaptureError::Backend("the portal's screenshot does not have the size its header names".into())
    })?;
    let mut data = Vec::with_capacity(width * height * 4);
    for pixel in decoded.chunks_exact(channels) {
        let bgra = match info.color_type {
            png::ColorType::Rgba => [pixel[2], pixel[1], pixel[0], pixel[3]],
            png::ColorType::Rgb => [pixel[2], pixel[1], pixel[0], 255],
            png::ColorType::GrayscaleAlpha => [pixel[0], pixel[0], pixel[0], pixel[1]],
            _ => [pixel[0], pixel[0], pixel[0], 255],
        };
        // A screen is opaque; a portal that writes an alpha channel means nothing by it.
        data.extend_from_slice(&[bgra[0], bgra[1], bgra[2], 255]);
    }
    Ok(Shot { size: PhysicalSize::new(width as u32, height as u32), data })
}

/// What a screen cast should record.
pub(super) struct CastRequest {
    /// A single window, which the system's picker chooses; a monitor otherwise.
    pub window: bool,
    pub with_pointer: bool,
    /// The name the restore token of this source is kept under; `None` always shows the picker.
    pub token_key: Option<String>,
}

/// A started screen cast: the PipeWire remote and the node that carries the frames. The session
/// lives until [`Cast::close`] or the drop.
pub(super) struct Cast {
    proxy: Screencast<'static>,
    session: Option<Session<'static, Screencast<'static>>>,
    pub remote: OwnedFd,
    pub node: u32,
    /// Where the stream lies on the compositor's desktop and how large it is there, in the
    /// compositor's own logical units, when the portal says.
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    /// The token this start handed out; it is filed once the caller knows which display the
    /// user picked.
    token: Option<String>,
}

impl Cast {
    /// Files the session's restore token under `key`, so the next recording of that source
    /// starts without the picker. Does nothing without a state directory or without a token.
    pub fn keep_token(&mut self, key: &str) {
        if let Some(token) = self.token.take() {
            store_token(key, Some(&token));
        }
    }

    /// Ends the session, which also ends the system's "this screen is shared" indicator.
    pub fn close(&mut self) {
        if let Some(session) = self.session.take() {
            let _ = wait(Duration::from_secs(3), async { session.close().await });
        }
    }
}

impl Drop for Cast {
    fn drop(&mut self) {
        self.close();
    }
}

/// Opens a screen cast session and starts it. The system's picker appears unless a restore
/// token of an earlier session still holds; a refusal comes back as `PermissionDenied`.
pub(super) fn open_cast(request: &CastRequest) -> Result<Cast> {
    let what = "screen recording";
    let (proxy, session, types, cursors) = wait(QUICK, async {
        let proxy = Screencast::new().await?;
        let session = proxy.create_session().await?;
        let types = proxy.available_source_types().await.unwrap_or_else(|_| SourceType::Monitor.into());
        let cursors = proxy.available_cursor_modes().await.unwrap_or_else(|_| CursorMode::Hidden.into());
        Ok::<_, ashpd::Error>((proxy, session, types, cursors))
    })?
    .map_err(|e| portal_error(e, what))?;
    let mut cast = Cast {
        proxy,
        session: Some(session),
        remote: placeholder_fd()?,
        node: 0,
        position: None,
        size: None,
        token: None,
    };
    let Some(session) = cast.session.as_ref() else {
        return Err(CaptureError::Backend("the screen cast session did not open".into()));
    };

    let source = if request.window { SourceType::Window } else { SourceType::Monitor };
    if !types.contains(source) {
        return Err(CaptureError::Backend(if request.window {
            "this desktop's screen sharing cannot record a single window".into()
        } else {
            "this desktop's screen sharing cannot record a monitor".into()
        }));
    }
    let wanted = if request.with_pointer { CursorMode::Embedded } else { CursorMode::Hidden };
    let other = if request.with_pointer { CursorMode::Hidden } else { CursorMode::Embedded };
    // A desktop that cannot do what was asked still records, with the pointer its own way.
    let cursor = if cursors.contains(wanted) || !cursors.contains(other) { wanted } else { other };
    let persists = state_dir().is_some() && request.token_key.is_some();
    let token = request.token_key.as_deref().and_then(load_token);
    let persist = if persists { PersistMode::ExplicitlyRevoked } else { PersistMode::DoNot };

    wait(QUICK, async {
        cast.proxy
            .select_sources(session, cursor, BitFlags::from(source), false, token.as_deref(), persist)
            .await?
            .response()
    })?
    .map_err(|e| portal_error(e, what))?;
    let streams = wait(PICKER, async { cast.proxy.start(session, None).await?.response() })?
        .map_err(|e| portal_error(e, what))?;
    let stream = streams
        .streams()
        .first()
        .ok_or_else(|| CaptureError::Backend("the portal started a screen cast without a stream".into()))?;
    let (node, position, size) = (stream.pipe_wire_node_id(), stream.position(), stream.size());
    let new_token = streams.restore_token().map(str::to_string);
    let remote = wait(QUICK, cast.proxy.open_pipe_wire_remote(session))?.map_err(|e| portal_error(e, what))?;
    if token.is_some() && new_token.is_none() {
        // The token was used up and none came back: it must not be offered again.
        if let Some(key) = &request.token_key {
            store_token(key, None);
        }
    }
    cast.remote = remote;
    cast.node = node;
    cast.position = position;
    cast.size = size;
    cast.token = new_token.filter(|_| persists);
    Ok(cast)
}

/// Forgets the token kept under `key`, so the next recording of that source shows the picker.
pub(super) fn forget_token(key: &str) {
    store_token(key, None);
}

/// A descriptor that stands in until the PipeWire remote is open, so `Cast` can own the session
/// (and close it on every early return) from the start.
fn placeholder_fd() -> Result<OwnedFd> {
    std::fs::File::open("/dev/null")
        .map(OwnedFd::from)
        .map_err(|e| CaptureError::Backend(format!("/dev/null could not be opened: {e}")))
}

fn tokens_path() -> Option<PathBuf> {
    state_dir().map(|dir| dir.join(TOKENS_FILE))
}

/// The tokens file as key and token per line, separated by a tab.
fn read_tokens(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(key, token)| !key.is_empty() && !token.is_empty())
        .map(|(key, token)| (key.to_string(), token.to_string()))
        .collect()
}

fn load_token(key: &str) -> Option<String> {
    read_tokens(&tokens_path()?).remove(&clean(key))
}

/// Writes or removes one token. The file is replaced as a whole through a rename, so a crash
/// never leaves half a file; a failure only means the picker shows again next time.
fn store_token(key: &str, token: Option<&str>) {
    let Some(path) = tokens_path() else { return };
    let mut tokens = read_tokens(&path);
    match token {
        Some(token) => tokens.insert(clean(key), clean(token)),
        None => tokens.remove(&clean(key)),
    };
    let text: String = tokens.iter().map(|(key, token)| format!("{key}\t{token}\n")).collect();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temporary = path.with_extension("new");
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}

/// A key or a token without the two characters the file's layout uses.
fn clean(text: &str) -> String {
    text.chars().filter(|c| *c != '\t' && *c != '\n' && *c != '\r').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The portal's screenshot is deleted after it was read, in the user's own Pictures folder.
    /// That deletion must take exactly the named file: never a folder, never what a link points
    /// at, never a neighbour, because nothing brings a user's pictures back.
    #[test]
    fn only_the_portals_own_file_is_ever_deleted() {
        let folder = std::env::temp_dir().join(format!("glimpsfy-portal-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("Album")).unwrap();
        let shot = folder.join("Screenshot from 2026-11-22 10-00-00.png");
        let neighbour = folder.join("Holiday.png");
        let inside = folder.join("Album").join("Photo.png");
        let link = folder.join("Link.png");
        for file in [&shot, &neighbour, &inside] {
            std::fs::write(file, b"picture").unwrap();
        }
        std::os::unix::fs::symlink(&neighbour, &link).unwrap();

        // A folder, a link and a relative path are refused and nothing changes.
        assert!(remove_portal_file(&folder.join("Album")).is_err());
        assert!(remove_portal_file(&folder).is_err());
        assert!(remove_portal_file(&link).is_err());
        assert!(remove_portal_file(Path::new("Holiday.png")).is_err());
        assert!(remove_portal_file(&folder.join("Missing.png")).is_err());
        for kept in [&shot, &neighbour, &inside] {
            assert!(kept.is_file(), "{} was deleted", kept.display());
        }
        assert!(link.is_symlink());

        // The portal's file goes, through the URI the portal names it by, and only it.
        let uri = ashpd::url::Url::from_file_path(&shot).unwrap();
        assert!(uri.as_str().contains("%20"), "the URI escapes the spaces of the file's name");
        remove_portal_file(&uri.to_file_path().unwrap()).unwrap();
        assert!(!shot.exists());
        assert!(neighbour.is_file() && inside.is_file() && link.is_symlink() && folder.join("Album").is_dir());

        std::fs::remove_dir_all(&folder).unwrap();
    }
}
