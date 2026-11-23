//! Subject lifting: the editor's Remove background (⌥⌘B) and the overlay's Copy subject. Owner:
//! annotation; the masks come from `glimpsfy_ocr::find_subjects` (Vision's foreground request,
//! macOS 14, entirely on the Mac). Windows and Linux have no engine yet, so `subjectLift` is false
//! there and the pages show neither action; the commands would fail with `not_implemented`.
//!
//! # The editor
//!
//! `subject_find` takes the editor image's pixels as the page shows them, before anything is drawn
//! on them, as the raw body of the call with the `x-glimpsfy-subject` header (never as base64).
//! The page sends them, and the core does not decode the file again, so the masks lie exactly on
//! the pixels the page shows, whatever the file's orientation and colour profile. The subjects
//! found are kept with the image while it is open, and the answer is one byte per pixel naming
//! the subject that covers it (0 for the background), from which the page counts the subjects,
//! outlines the one under the pointer and draws the chip's thumbnails. An empty answer means the
//! image has no subject.
//!
//! `subject_cut` makes the image without its background the editor's base and answers its RGBA
//! rows: only the alpha of the original pixels changes, so a Display P3 image stays Display P3.
//! While a cut is the base, the editor's outputs belong to a record of the history forked from the
//! image's own (`EditorRecord::fork`): the first one adds an entry of its own, and an entry the
//! image was reopened from keeps its original pixels. Each set of kept subjects keeps its record
//! while the image is open, so going back to a set revises the entry that set made. `None` makes
//! the original the base again.
//!
//! # The overlay
//!
//! `subject_copy` and `subject_copy_rendered` copy every subject of a selection as one transparent
//! PNG. The masks are always found in the frozen frame's crop before any drawing, and they are
//! laid over the pixels that leave: with drawings, a blur over a subject stays and one over the
//! background goes with it. Without a subject the answer says 0, nothing is copied and the capture
//! stays open; otherwise the capture closes as it does for Copy, and the Subject copied toast
//! says how many subjects went.
//!
//! # Threads
//!
//! Vision takes half a second to two seconds, so every search runs on a blocking worker thread
//! and registers under its `job` with the text recognition's jobs: `subject_cancel` stops it, and
//! so does closing the capture it reads.

use std::sync::{Arc, Mutex, MutexGuard};

use glimpsfy_capture::{ColorSpace, Frame, PhysicalRect, PhysicalSize, PixelFormat, ScaleFactor};
use glimpsfy_ocr::Subjects;
use serde::{Deserialize, Serialize};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Manager};

use super::capture::Selection;
use super::error::{CommandError, CommandResult};
use super::history::{EditorBase, EditorRecord};
use super::ocr::JobGuard;
use super::render::{self, RenderSource};
use crate::output::{self, Origin};
use crate::AppState;

/// The header that describes the raw pixels of `subject_find`: JSON (`SubjectHeader`),
/// percent-encoded, because a header takes ASCII only.
pub const SUBJECT_HEADER: &str = "x-glimpsfy-subject";

/// The header that names the search of `subject_copy_rendered`, for `subject_cancel`; the render
/// header it comes with is shared with the other rendered outputs, which have no search.
pub const JOB_HEADER: &str = "x-glimpsfy-job";

/// What the page sends with the editor image's pixels.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectHeader {
    /// The editor image the pixels are, by the token of its `EditorImage`.
    pub token: String,
    /// The image's size in pixels; the body holds `width × height × 4` bytes of RGBA.
    pub width: u32,
    pub height: u32,
    /// The colour space the page decoded the image in, which must be the image's.
    pub color_space: ColorSpace,
    /// A name for the search, for `subject_cancel`.
    #[serde(default)]
    pub job: Option<String>,
}

/// What Copy subject did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectCopy {
    /// How many subjects went to the clipboard; 0 when the selection has none, and then nothing
    /// was copied and the capture is still open.
    pub subjects: u32,
}

// -------------------------------------------------------------------------------- commands

/// Finds the subjects of the editor image the page sends (see the module documentation) and
/// answers one byte per pixel: the number of the subject that covers it, from 1, or 0. An image
/// whose subjects were found already answers at once. An image without a subject answers an
/// empty body. Body: RGBA bytes; header: `x-glimpsfy-subject`.
#[tauri::command]
pub async fn subject_find(app: AppHandle, request: Request<'_>) -> CommandResult<Response> {
    let (header, pixels) = read_pixels(&request)?;
    off_main(move || {
        let (color, scale, record) = render::editor_facts(&header.token)?;
        if color.space != header.color_space {
            return Err(CommandError::invalid(format!(
                "the pixels are in {}, and the image is in {}",
                header.color_space.css_name(),
                color.space.css_name()
            )));
        }
        let size = PhysicalSize::new(header.width, header.height);
        if let Some(labels) = cached_labels(&header.token, size) {
            return Ok(Response::new(labels));
        }
        let base = Arc::new(Frame::new(
            size,
            header.width as usize * 4,
            PixelFormat::Rgba8,
            color,
            ScaleFactor(scale),
            pixels,
        )?);
        let guard = JobGuard::start(&app, header.job, None);
        let subjects = glimpsfy_ocr::find_subjects(&base, &guard.cancel)?;
        let labels = if subjects.count() == 0 { Vec::new() } else { subjects.labels() };
        keep_lift(Lift {
            token: header.token,
            base,
            record,
            subjects: Arc::new(subjects),
            labels: labels.clone(),
            cuts: Vec::new(),
            active: None,
        });
        Ok(Response::new(labels))
    })
    .await
}

/// Makes the editor image `token` without its background its base, keeping the subjects of
/// `keep` (numbers from `subject_find`), and answers the cut image's RGBA rows; `None` makes the
/// original the base again and answers an empty body. The image's subjects must have been found.
/// An empty `keep` or a number that is no subject fails with `invalid_argument`.
#[tauri::command]
pub async fn subject_cut(token: String, keep: Option<Vec<u32>>) -> CommandResult<Response> {
    off_main(move || {
        let Some(mut keep) = keep else {
            // An image whose subjects were never found has its original as its base already.
            if let Some(lift) = lifts().iter_mut().find(|l| l.token == token) {
                lift.active = None;
            }
            return Ok(Response::new(Vec::new()));
        };
        keep.sort_unstable();
        keep.dedup();
        let found = with_lift(&token, |lift| {
            lift.cuts
                .iter()
                .find(|c| c.keep == keep)
                .map(|c| c.frame.clone())
                .ok_or_else(|| (lift.base.clone(), lift.subjects.clone(), lift.record.clone()))
        })?;
        let frame = match found {
            Ok(frame) => frame,
            Err((base, subjects, record)) => {
                // The cut is made outside the lock: it takes a moment on a large image.
                let frame = Arc::new(subjects.cut(&base, &keep)?);
                let cut = Cut {
                    keep: keep.clone(),
                    frame: frame.clone(),
                    record: record.fork(EditorBase::Frame(frame.clone())),
                };
                with_lift(&token, |lift| {
                    lift.cuts.retain(|c| c.keep != keep);
                    lift.cuts.push(cut);
                    // A cut is a whole image; the oldest sets go first.
                    let extra = lift.cuts.len().saturating_sub(CUTS);
                    lift.cuts.drain(..extra);
                })?;
                frame
            }
        };
        with_lift(&token, |lift| lift.active = Some(keep))?;
        Ok(Response::new(frame.to_rgba()))
    })
    .await
}

/// Copies every subject of a selection without drawings as one transparent PNG (⌥⌘B in the
/// overlay), from the picked window's own image when the selection is still that window. Without
/// a subject it answers 0 and the capture stays open; otherwise the capture closes and the
/// Subject copied toast appears.
#[tauri::command]
pub async fn subject_copy(app: AppHandle, selection: Selection, job: Option<String>) -> CommandResult<SubjectCopy> {
    off_main(move || {
        let (frame, origin) = output::take(&app, &selection)?;
        let guard = JobGuard::start(&app, job, Some(selection.capture_id));
        let subjects = glimpsfy_ocr::find_subjects(&frame, &guard.cancel)?;
        drop(guard);
        copy_cut(&app, &subjects, &frame, origin)
    })
    .await
}

/// Copy subject for a selection with drawings: the masks are found in the frozen frame's crop of
/// the header's `area`, before any drawing, and laid over the rendered pixels. Body: RGBA bytes;
/// header: `x-glimpsfy-render`, whose source must be a capture and whose `area` must be set.
#[tauri::command]
pub async fn subject_copy_rendered(app: AppHandle, request: Request<'_>) -> CommandResult<SubjectCopy> {
    let (header, pixels) = render::read_rendered(&request)?;
    let job = request.headers().get(JOB_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string);
    off_main(move || {
        let RenderSource::Capture { capture_id, display_id } = header.source else {
            return Err(CommandError::invalid("Copy subject takes a capture's selection"));
        };
        let area = header.area.ok_or_else(|| CommandError::invalid("the rendered selection has no area"))?;
        let plain = {
            let state = app.state::<AppState>();
            let session = state
                .sessions
                .get(capture_id)
                .ok_or_else(|| CommandError::not_found(format!("capture #{}", capture_id.0)))?;
            let entry = session
                .display(display_id)
                .ok_or_else(|| CommandError::not_found(format!("display {}", display_id.0)))?;
            let rect = PhysicalRect::new(area.x.round() as i32, area.y.round() as i32, header.width, header.height);
            entry.frame.crop(rect)?
        };
        let guard = JobGuard::start(&app, job, Some(capture_id));
        let subjects = glimpsfy_ocr::find_subjects(&plain, &guard.cancel)?;
        drop(guard);
        if subjects.count() == 0 {
            return Ok(SubjectCopy { subjects: 0 });
        }
        let ready = render::prepare(&app, header, pixels, None, true)?;
        let origin = ready.output_origin(&app);
        copy_cut(&app, &subjects, &ready.frame, origin)
    })
    .await
}

/// Stops the subject search started under `job`; a search that already ended is left alone.
#[tauri::command]
pub fn subject_cancel(job: String) {
    super::ocr::cancel_job(&job);
}

// ------------------------------------------------------------------------------- the work

/// The cut sets one editor image keeps; each is a whole image in memory.
const CUTS: usize = 3;

/// The subjects of one editor image and the cuts made of it.
struct Lift {
    token: String,
    /// The pixels the subjects were found in, the image under every drawing.
    base: Arc<Frame>,
    /// The image's own history record, which every cut's record is forked from.
    record: Arc<EditorRecord>,
    subjects: Arc<Subjects>,
    /// What `subject_find` answered, kept for the next time it is asked.
    labels: Vec<u8>,
    cuts: Vec<Cut>,
    /// The subjects of the cut that is the editor's base now; `None` while the original is.
    active: Option<Vec<u32>>,
}

/// The image with only the subjects of `keep` left, and the record its outputs belong to.
struct Cut {
    keep: Vec<u32>,
    frame: Arc<Frame>,
    record: Arc<EditorRecord>,
}

/// The lifts of the editor's images. `render::prepare` reads it while it holds the editor's own
/// lock, so this one is never held while the editor's is taken.
static LIFTS: Mutex<Vec<Lift>> = Mutex::new(Vec::new());

fn lifts() -> MutexGuard<'static, Vec<Lift>> {
    LIFTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The record the outputs of the editor image `token` belong to while a cut is its base.
pub(super) fn record_for(token: &str) -> Option<Arc<EditorRecord>> {
    let lifts = lifts();
    let lift = lifts.iter().find(|l| l.token == token)?;
    let keep = lift.active.as_ref()?;
    lift.cuts.iter().find(|c| &c.keep == keep).map(|c| c.record.clone())
}

/// Forgets the subjects of an image the editor let go.
pub(super) fn forget(token: &str) {
    lifts().retain(|l| l.token != token);
}

fn cached_labels(token: &str, size: PhysicalSize) -> Option<Vec<u8>> {
    let lifts = lifts();
    lifts.iter().find(|l| l.token == token && l.base.size == size).map(|l| l.labels.clone())
}

/// Keeps a new lift, and drops those of images the editor no longer holds.
fn keep_lift(lift: Lift) {
    let open = render::editor_tokens();
    let mut lifts = lifts();
    lifts.retain(|l| l.token != lift.token && open.contains(&l.token));
    lifts.push(lift);
}

fn with_lift<T>(token: &str, work: impl FnOnce(&mut Lift) -> T) -> CommandResult<T> {
    let mut lifts = lifts();
    let lift = lifts
        .iter_mut()
        .find(|l| l.token == token)
        .ok_or_else(|| CommandError::not_found("the image's subjects have not been found"))?;
    Ok(work(lift))
}

/// Copies `frame` with every subject kept, or answers 0 when there is none.
fn copy_cut(app: &AppHandle, subjects: &Subjects, frame: &Frame, origin: Origin) -> CommandResult<SubjectCopy> {
    let count = subjects.count() as u32;
    if count == 0 {
        return Ok(SubjectCopy { subjects: 0 });
    }
    let cut = subjects.cut(frame, &subjects.all())?;
    output::deliver_subjects(app, cut, origin, count)?;
    Ok(SubjectCopy { subjects: count })
}

/// The header and the pixels of `subject_find`, checked against each other.
fn read_pixels(request: &Request<'_>) -> CommandResult<(SubjectHeader, Vec<u8>)> {
    let raw = request
        .headers()
        .get(SUBJECT_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| CommandError::invalid(format!("the call has no {SUBJECT_HEADER} header")))?;
    let json = render::percent_decode(raw)
        .ok_or_else(|| CommandError::invalid("the subject header is not percent-encoded UTF-8"))?;
    let header: SubjectHeader = serde_json::from_str(&json)
        .map_err(|e| CommandError::invalid(format!("the subject header does not parse: {e}")))?;
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(CommandError::invalid("the pixels must be sent as raw bytes"));
    };
    let expected = header.width as u64 * header.height as u64 * 4;
    if header.width == 0 || header.height == 0 || bytes.len() as u64 != expected {
        return Err(CommandError::invalid(format!(
            "{} bytes do not hold {} × {} RGBA pixels",
            bytes.len(),
            header.width,
            header.height
        )));
    }
    // The body belongs to the call, and Vision reads on another thread.
    Ok((header, bytes.clone()))
}

async fn off_main<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| CommandError::internal(e.to_string()))?
}
