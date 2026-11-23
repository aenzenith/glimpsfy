//! The system's side of an output: the clipboard and the shutter sound.

use tauri::AppHandle;

use crate::commands::error::{CommandError, CommandResult};

/// Puts text on the clipboard: recognised text, a colour from the picker.
pub fn copy_text(text: &str) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    let copied = crate::macos::copy_text(text);
    #[cfg(not(target_os = "macos"))]
    let copied = crate::desktop::copy_text(text);
    copied.map_err(CommandError::internal)
}

/// Puts PNG images on the clipboard: one image as `public.png`, several (one per display) as
/// one pasteboard item each, in order, so an app that takes many pastes them all and an app
/// that takes one gets the first.
#[cfg(target_os = "macos")]
pub fn copy_pngs(app: &AppHandle, pngs: &[Vec<u8>], frames: &[&glimpsfy_capture::Frame]) -> CommandResult<()> {
    let _ = (app, frames);
    match pngs {
        [] => Ok(()),
        [one] => crate::macos::copy_png(one).map_err(CommandError::internal),
        many => copy_items(many),
    }
}

#[cfg(target_os = "macos")]
fn copy_items(pngs: &[Vec<u8>]) -> CommandResult<()> {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSData};

    let pasteboard = NSPasteboard::generalPasteboard();
    // SAFETY: an AppKit constant, valid for the life of the process.
    let png_type = unsafe { NSPasteboardTypePNG };
    let mut items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::with_capacity(pngs.len());
    for png in pngs {
        let item = NSPasteboardItem::new();
        if !item.setData_forType(&NSData::with_bytes(png), png_type) {
            return Err(CommandError::internal("the pasteboard refused an image"));
        }
        items.push(ProtocolObject::from_retained(item));
    }
    pasteboard.clearContents();
    let array = NSArray::from_retained_slice(&items);
    if pasteboard.writeObjects(&array) {
        Ok(())
    } else {
        Err(CommandError::internal("the pasteboard refused the images"))
    }
}

/// Puts the images on the clipboard; outside macOS only the first, which is all the clipboard
/// plugin takes.
#[cfg(not(target_os = "macos"))]
pub fn copy_pngs(app: &AppHandle, pngs: &[Vec<u8>], frames: &[&glimpsfy_capture::Frame]) -> CommandResult<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let _ = pngs;
    let Some(frame) = frames.first() else {
        return Ok(());
    };
    let image = tauri::image::Image::new_owned(frame.to_rgba(), frame.size.width, frame.size.height);
    app.clipboard().write_image(&image).map_err(|e| CommandError::internal(e.to_string()))
}

/// Brings Glimpsfy to the front, so a panel it opens (Save as) is not behind the app the user is
/// in.
#[cfg(target_os = "macos")]
pub fn bring_forward(app: &AppHandle) {
    crate::macos::on_main(app, |mtm| {
        let application = objc2_app_kit::NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        application.activateIgnoringOtherApps(true);
    });
}

#[cfg(not(target_os = "macos"))]
pub fn bring_forward(_app: &AppHandle) {}

/// Plays the system's screenshot sound. It is a user interface sound, so the system keeps it
/// silent when "Play user interface sound effects" is off or the output is muted, and plays it
/// at the alert volume otherwise.
#[cfg(target_os = "macos")]
pub fn shutter() {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    #[link(name = "AudioToolbox", kind = "framework")]
    extern "C" {
        fn AudioServicesCreateSystemSoundID(url: *const c_void, sound: *mut u32) -> i32;
        fn AudioServicesPlaySystemSound(sound: u32);
    }

    const SOUNDS: &str = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/system";
    static SOUND: OnceLock<Option<u32>> = OnceLock::new();
    let sound = *SOUND.get_or_init(|| {
        ["Screen Capture.aif", "Grab.aif", "Shutter.aif"].iter().find_map(|name| {
            let path = std::path::Path::new(SOUNDS).join(name);
            if !path.exists() {
                return None;
            }
            let url = objc2_foundation::NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(
                &path.to_string_lossy(),
            ));
            let mut id = 0u32;
            // SAFETY: an NSURL is toll-free bridged to the CFURL the call takes, it outlives the
            // call, and `id` is a valid place for the answer.
            let status = unsafe { AudioServicesCreateSystemSoundID(objc2::rc::Retained::as_ptr(&url).cast(), &mut id) };
            (status == 0 && id != 0).then_some(id)
        })
    });
    if let Some(sound) = sound {
        // SAFETY: a sound made above; playing it is asynchronous and never fails loudly.
        unsafe { AudioServicesPlaySystemSound(sound) };
    }
}

#[cfg(not(target_os = "macos"))]
pub fn shutter() {}
