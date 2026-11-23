//! The microphones of Linux, as the sound server (PulseAudio, or PipeWire's pulse server) lists
//! them through the capture crate, and whether it can give the system's own sound. Cameras are
//! left out until the shell decides how the webcam bubble works on Linux.

use crate::{Device, DeviceTransport, PermissionState, RecordingDevices};

/// How a source is connected, read from the name the sound server gives it
/// (`alsa_input.usb-…`, `bluez_input.…`, `alsa_input.pci-…`).
fn transport(id: &str) -> DeviceTransport {
    let id = id.to_ascii_lowercase();
    if id.starts_with("bluez") {
        DeviceTransport::Bluetooth
    } else if id.contains(".usb-") {
        DeviceTransport::Usb
    } else if id.contains(".pci-") || id.contains(".platform-") {
        DeviceTransport::BuiltIn
    } else if id.contains("virtual") || id.contains("null") || id.contains("remap") || id.contains("echo-cancel") {
        DeviceTransport::Virtual
    } else {
        DeviceTransport::Other
    }
}

pub fn list() -> RecordingDevices {
    // No sound server, or no PulseAudio plugin, lists no microphone; a recording then has none.
    let microphones = glimpsfy_capture::linux_microphones()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name)| Device { transport: transport(&id), id, name, is_default: false })
        .collect();
    RecordingDevices {
        microphones,
        cameras: vec![],
        system_audio: glimpsfy_capture::linux_system_audio_available(),
        // The sound server lets every program of the session record; there is nothing to ask.
        microphone_permission: PermissionState::NotRequired,
        camera_permission: PermissionState::NotRequired,
    }
}
