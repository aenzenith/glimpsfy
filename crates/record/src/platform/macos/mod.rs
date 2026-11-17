//! AVFoundation's side of recording on macOS: the take's writer, reading recordings back for
//! export and the trim timeline, the devices, and the Trash.

pub mod devices;
pub mod reader;
pub mod writer;

use std::ffi::c_void;
use std::path::Path;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::Message;
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMAudioFormatDescriptionCreate, CMFormatDescription, CMTime};
use objc2_foundation::{NSDictionary, NSError, NSFileManager, NSNumber, NSString, NSURL};

use crate::{RecordError, Result};

/// Moves `path` to the Trash with `NSFileManager`, so the user can still put it back.
pub fn trash(path: &Path) -> std::io::Result<()> {
    let url = file_url(path).map_err(|e| std::io::Error::other(e.to_string()))?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| std::io::Error::other(describe(&e)))
}

/// Any Objective-C object as `AnyObject`, for the values of a settings dictionary.
pub(crate) fn any<T: Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an `AnyObject`.
    unsafe { Retained::cast_unchecked(object) }
}

pub(crate) fn int(n: i64) -> Retained<AnyObject> {
    any(NSNumber::new_i64(n))
}

pub(crate) fn float(n: f64) -> Retained<AnyObject> {
    any(NSNumber::new_f64(n))
}

pub(crate) fn boolean(b: bool) -> Retained<AnyObject> {
    any(NSNumber::new_bool(b))
}

pub(crate) fn dict(pairs: &[(&NSString, Retained<AnyObject>)]) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<&NSString> = pairs.iter().map(|(k, _)| *k).collect();
    let values: Vec<&AnyObject> = pairs.iter().map(|(_, v)| &**v).collect();
    NSDictionary::from_slices(&keys, &values)
}

/// An AVFoundation string constant, which is a weak symbol and may be missing on an old system.
pub(crate) fn constant(value: Option<&'static NSString>, name: &str) -> Result<&'static NSString> {
    value.ok_or_else(|| RecordError::Encoder(format!("this macOS version lacks {name}")))
}

pub(crate) fn file_url(path: &Path) -> Result<Retained<NSURL>> {
    let text = path
        .to_str()
        .ok_or_else(|| RecordError::InvalidConfig(format!("the path {} is not valid UTF-8", path.display())))?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(text)))
}

pub(crate) fn describe(error: &NSError) -> String {
    let text = error.localizedDescription().to_string();
    let reason = error.localizedFailureReason().map(|r| r.to_string()).unwrap_or_default();
    if reason.is_empty() {
        format!("{text} ({})", error.code())
    } else {
        format!("{text} {reason} ({})", error.code())
    }
}

/// `ns` nanoseconds as a `CMTime` with `scale` ticks per second.
pub(crate) fn time(ns: u64, scale: i32) -> CMTime {
    let value = (ns as i128 * scale as i128 / 1_000_000_000) as i64;
    // SAFETY: `CMTimeMake` builds a value; it has no preconditions.
    unsafe { CMTime::new(value, scale) }
}

/// A `CMTime` in nanoseconds; an invalid or negative time is 0.
pub(crate) fn nanos(t: CMTime) -> u64 {
    if t.timescale <= 0 || t.value < 0 {
        return 0;
    }
    (t.value as i128 * 1_000_000_000 / t.timescale as i128) as u64
}

pub(crate) const FORMAT_LPCM: u32 = u32::from_be_bytes(*b"lpcm");
const FLAG_FLOAT: u32 = 1;
const FLAG_PACKED: u32 = 1 << 3;

/// The format of the audio the session hands to the writer: 48 kHz stereo 32-bit float,
/// interleaved.
pub(crate) fn pcm_format() -> Result<CFRetained<CMFormatDescription>> {
    let asbd = AudioStreamBasicDescription {
        mSampleRate: crate::audio::RATE as f64,
        mFormatID: FORMAT_LPCM,
        mFormatFlags: FLAG_FLOAT | FLAG_PACKED,
        mBytesPerPacket: 8,
        mFramesPerPacket: 1,
        mBytesPerFrame: 8,
        mChannelsPerFrame: 2,
        mBitsPerChannel: 32,
        mReserved: 0,
    };
    let mut out: *const CMFormatDescription = std::ptr::null();
    // SAFETY: `asbd` lives for the call; no layout and no cookie are passed; `out` is a valid
    // out pointer that receives a +1 reference.
    let status = unsafe {
        CMAudioFormatDescriptionCreate(
            None,
            NonNull::from(&asbd),
            0,
            std::ptr::null(),
            0,
            std::ptr::null::<c_void>(),
            None,
            NonNull::from(&mut out),
        )
    };
    let out = NonNull::new(out as *mut CMFormatDescription)
        .filter(|_| status == 0)
        .ok_or_else(|| RecordError::Encoder(format!("the audio format could not be described ({status})")))?;
    // SAFETY: the create function returned a +1 reference.
    Ok(unsafe { CFRetained::from_raw(out) })
}
