//! The system permissions Glimpsfy needs: screen recording for every capture, the microphone and
//! the camera for recordings, and accessibility for the keystroke overlay.
//!
//! Only screen recording blocks the screenshot core. When it is missing, a capture asks the
//! system once (`CGRequestScreenCaptureAccess`, which shows the system prompt the first time),
//! tells every page with `permissions://changed` and opens the first-run window, which explains
//! the switch in System Settings.
//!
//! macOS answers the screen recording question once per process: a permission granted while
//! Glimpsfy runs reads as missing until Glimpsfy starts again. A short-lived copy of the app
//! (`probe`) asks afresh, so the first-run window can say "restart needed" instead of leaving
//! the user to toggle the switch again.

use std::sync::atomic::{AtomicBool, Ordering};

use glimpsfy_capture::PermissionState;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::commands::windows::WindowKind;
use crate::{events, windows, AppState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    ScreenRecording,
    Microphone,
    Camera,
    Accessibility,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    pub screen_recording: PermissionState,
    pub microphone: PermissionState,
    pub camera: PermissionState,
    pub accessibility: PermissionState,
    /// Screen recording was granted while Glimpsfy ran, and it works after a restart.
    pub restart_needed: bool,
}

/// The last answer of the probe: screen recording is granted, but not to this process yet.
static RESTART_NEEDED: AtomicBool = AtomicBool::new(false);

/// Every permission without prompting; `restart_needed` is the probe's last answer.
pub fn status(app: &AppHandle) -> Permissions {
    let screen_recording = app.state::<AppState>().backend.permission();
    Permissions {
        screen_recording,
        microphone: platform::media(platform::Media::Audio),
        camera: platform::media(platform::Media::Video),
        accessibility: platform::accessibility(),
        restart_needed: screen_recording != PermissionState::Granted && RESTART_NEEDED.load(Ordering::SeqCst),
    }
}

/// Like `status`, and when screen recording reads as missing, asks a fresh process whether it
/// was granted meanwhile. Blocks for the probe's run (tens of milliseconds): never on the main
/// thread.
pub fn status_fresh(app: &AppHandle) -> Permissions {
    if app.state::<AppState>().backend.permission() != PermissionState::Granted {
        RESTART_NEEDED.store(platform::granted_to_a_new_process(), Ordering::SeqCst);
    }
    status(app)
}

/// The environment variable that turns a launch into the probe.
const PROBE: &str = "GLIMPSFY_PERMISSION_PROBE";

/// When this process is the probe, its exit code: 0 when screen recording is granted, 1 when it
/// is not. `run` calls it first, before anything starts.
pub fn probe() -> Option<i32> {
    std::env::var_os(PROBE)?;
    Some(if platform::screen_recording_granted() { 0 } else { 1 })
}

/// Asks the system for `permission`. Screen recording shows the system prompt only the first
/// time; after that the user grants it in System Settings, which `open_settings` opens.
pub fn request(app: &AppHandle, permission: Permission) -> PermissionState {
    let state = match permission {
        Permission::ScreenRecording => app.state::<AppState>().backend.request_permission(),
        Permission::Microphone => platform::request_media(app, platform::Media::Audio),
        Permission::Camera => platform::request_media(app, platform::Media::Video),
        // The system's own alert explains the switch and opens the pane.
        Permission::Accessibility => platform::request_accessibility(),
    };
    let _ = app.emit(events::PERMISSIONS_CHANGED, status(app));
    state
}

/// Opens the pane of System Settings where the user grants `permission`.
pub fn open_settings(app: &AppHandle, permission: Permission) {
    let anchor = match permission {
        Permission::ScreenRecording => "Privacy_ScreenCapture",
        Permission::Microphone => "Privacy_Microphone",
        Permission::Camera => "Privacy_Camera",
        Permission::Accessibility => "Privacy_Accessibility",
    };
    if cfg!(target_os = "macos") {
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        let _ = app.opener().open_url(url, None::<&str>);
    }
}

/// At launch, the first-run window opens until the user finished it, and whenever the screen
/// recording permission is missing.
pub fn at_launch(app: &AppHandle) {
    let done = app.state::<AppState>().settings.get().general.first_run_done;
    let granted = app.state::<AppState>().backend.permission() == PermissionState::Granted;
    if !done || !granted {
        windows::open(app, WindowKind::Firstrun);
    }
}

/// A capture found the screen recording permission missing.
pub fn screen_recording_missing(app: &AppHandle) {
    request(app, Permission::ScreenRecording);
    windows::open(app, WindowKind::Firstrun);
}

#[cfg(target_os = "macos")]
mod platform {
    use glimpsfy_capture::PermissionState;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, Bool};
    use objc2_foundation::NSString;
    use tauri::{AppHandle, Emitter};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    }

    // AVCaptureDevice lives in AVFoundation; linking it is enough to reach the class by name.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    #[derive(Clone, Copy)]
    pub enum Media {
        Audio,
        Video,
    }

    impl Media {
        /// `AVMediaTypeAudio` and `AVMediaTypeVideo`.
        fn name(self) -> &'static str {
            match self {
                Media::Audio => "soun",
                Media::Video => "vide",
            }
        }
    }

    pub fn accessibility() -> PermissionState {
        // SAFETY: a plain query without arguments.
        if unsafe { AXIsProcessTrusted() } {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }

    /// Asks for accessibility with the system's alert, which opens the pane from its button.
    pub fn request_accessibility() -> PermissionState {
        use objc2_foundation::{NSDictionary, NSNumber};
        let key = NSString::from_str("AXTrustedCheckOptionPrompt");
        let yes = NSNumber::new_bool(true);
        let options = NSDictionary::from_slices(&[&*key], &[&*yes]);
        // SAFETY: an NSDictionary is toll-free bridged to the CFDictionary the call takes, and
        // it lives across the call.
        let trusted =
            unsafe { AXIsProcessTrustedWithOptions(&*options as *const NSDictionary<NSString, NSNumber> as *const _) };
        if trusted {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }

    pub fn screen_recording_granted() -> bool {
        objc2_core_graphics::CGPreflightScreenCaptureAccess()
    }

    /// Starts this executable as the probe (`super::probe`) and reads its answer. A new process
    /// sees the permission as it is now, and the system counts it as Glimpsfy itself, because
    /// Glimpsfy started it.
    pub fn granted_to_a_new_process() -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let child = std::process::Command::new(exe)
            .env(super::PROBE, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return false;
        };
        // The probe exits at once; one that hangs is stopped and counts as "no".
        for _ in 0..40 {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        false
    }

    pub fn media(media: Media) -> PermissionState {
        let Some(class) = AnyClass::get(c"AVCaptureDevice") else {
            return PermissionState::NotDetermined;
        };
        let kind = NSString::from_str(media.name());
        // SAFETY: +[AVCaptureDevice authorizationStatusForMediaType:] takes an AVMediaType
        // (an NSString) and returns an NSInteger.
        let status: isize = unsafe { msg_send![class, authorizationStatusForMediaType: &*kind] };
        match status {
            3 => PermissionState::Granted,
            0 => PermissionState::NotDetermined,
            _ => PermissionState::Denied,
        }
    }

    /// Shows the system prompt when the user was never asked; the answer arrives later as
    /// `permissions://changed`.
    pub fn request_media(app: &AppHandle, which: Media) -> PermissionState {
        let current = media(which);
        if current != PermissionState::NotDetermined {
            return current;
        }
        let Some(class) = AnyClass::get(c"AVCaptureDevice") else {
            return current;
        };
        let kind = NSString::from_str(which.name());
        let app = app.clone();
        let done = block2::RcBlock::new(move |_granted: Bool| {
            let _ = app.emit(crate::events::PERMISSIONS_CHANGED, super::status(&app));
        });
        // SAFETY: +[AVCaptureDevice requestAccessForMediaType:completionHandler:] takes an
        // AVMediaType and a block of (BOOL) -> void, which it copies.
        let _: () = unsafe { msg_send![class, requestAccessForMediaType: &*kind, completionHandler: &*done] };
        current
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use glimpsfy_capture::PermissionState;
    use tauri::AppHandle;

    #[derive(Clone, Copy)]
    pub enum Media {
        Audio,
        Video,
    }

    pub fn accessibility() -> PermissionState {
        PermissionState::NotRequired
    }

    pub fn request_accessibility() -> PermissionState {
        PermissionState::NotRequired
    }

    pub fn screen_recording_granted() -> bool {
        true
    }

    pub fn granted_to_a_new_process() -> bool {
        false
    }

    pub fn media(_: Media) -> PermissionState {
        PermissionState::NotRequired
    }

    pub fn request_media(_: &AppHandle, _: Media) -> PermissionState {
        PermissionState::NotRequired
    }
}
