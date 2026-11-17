//! Microphones and cameras through `AVCaptureDevice`, with the ids a configuration stores
//! (`uniqueID`, stable across launches) and the state of each permission.
//!
//! Listing devices needs no permission; only opening one does, so the list never shows a
//! system prompt.

use objc2::rc::Retained;
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDevicePosition,
    AVCaptureDeviceType, AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeContinuityCamera,
    AVCaptureDeviceTypeExternal, AVCaptureDeviceTypeMicrophone, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
};
use objc2_foundation::NSArray;

use crate::{Device, DeviceTransport, PermissionState, RecordingDevices};

fn transport(code: i32) -> DeviceTransport {
    match &(code as u32).to_be_bytes() {
        b"bltn" => DeviceTransport::BuiltIn,
        b"usb " => DeviceTransport::Usb,
        b"blue" | b"blea" => DeviceTransport::Bluetooth,
        b"virt" | b"grup" => DeviceTransport::Virtual,
        _ => DeviceTransport::Other,
    }
}

fn permission(media: &AVMediaType) -> PermissionState {
    // SAFETY: a class method that reads the permission without asking.
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) } {
        AVAuthorizationStatus::Authorized => PermissionState::Granted,
        AVAuthorizationStatus::NotDetermined => PermissionState::NotDetermined,
        _ => PermissionState::Denied,
    }
}

fn discover(types: &[&AVCaptureDeviceType], media: &AVMediaType) -> Vec<Device> {
    // SAFETY: a discovery session lists devices without opening them; every property read is
    // a plain getter.
    unsafe {
        let types = NSArray::from_slice(types);
        let session = AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &types,
            Some(media),
            AVCaptureDevicePosition::Unspecified,
        );
        let default: Option<Retained<AVCaptureDevice>> = AVCaptureDevice::defaultDeviceWithMediaType(media);
        let default_id = default.map(|d| d.uniqueID().to_string());
        session
            .devices()
            .iter()
            .filter(|d| d.isConnected())
            .map(|d| {
                let id = d.uniqueID().to_string();
                Device {
                    is_default: default_id.as_deref() == Some(id.as_str()),
                    name: d.localizedName().to_string(),
                    transport: transport(d.transportType()),
                    id,
                }
            })
            .collect()
    }
}

pub fn list() -> RecordingDevices {
    let (Some(audio), Some(video)) = (
        // SAFETY: reading framework string constants.
        unsafe { AVMediaTypeAudio },
        unsafe { AVMediaTypeVideo },
    ) else {
        return RecordingDevices {
            microphones: vec![],
            cameras: vec![],
            system_audio: true,
            microphone_permission: PermissionState::NotDetermined,
            camera_permission: PermissionState::NotDetermined,
        };
    };
    // SAFETY: reading framework string constants.
    let (microphones, cameras) = unsafe {
        (
            discover(&[AVCaptureDeviceTypeMicrophone, AVCaptureDeviceTypeExternal], audio),
            discover(
                &[
                    AVCaptureDeviceTypeBuiltInWideAngleCamera,
                    AVCaptureDeviceTypeExternal,
                    AVCaptureDeviceTypeContinuityCamera,
                ],
                video,
            ),
        )
    };
    RecordingDevices {
        microphones,
        cameras,
        // ScreenCaptureKit records system audio on every macOS the app supports (13 and later).
        system_audio: true,
        microphone_permission: permission(audio),
        camera_permission: permission(video),
    }
}
