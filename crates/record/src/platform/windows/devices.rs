//! The microphones of Windows (Core Audio's active capture endpoints), the system audio that
//! WASAPI loopback always offers, and the privacy switches that can keep an app from the
//! microphone. Nothing here asks the user anything; the switches are only read.

use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::{PKEY_Device_EnumeratorName, PKEY_Device_FriendlyName};
use windows::Win32::Foundation::{ERROR_SUCCESS, PROPERTYKEY};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::logic::{self, Consent};
use super::mf::{Apartment, Variant};
use crate::{Device, DeviceTransport, PermissionState, RecordingDevices};

/// Where Windows keeps the privacy switches, per capability.
const CONSENT_STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

pub fn list() -> RecordingDevices {
    RecordingDevices {
        microphones: microphones(),
        cameras: vec![],
        // WASAPI loopback records what the speakers play on every supported Windows.
        system_audio: true,
        microphone_permission: permission("microphone"),
        // Cameras are not recorded on Windows yet, so their switch does not matter.
        camera_permission: PermissionState::NotRequired,
    }
}

/// The active microphones, listed on a thread of their own inside the multithreaded apartment,
/// whatever the caller's apartment is. A failure lists none.
fn microphones() -> Vec<Device> {
    std::thread::scope(|scope| {
        let lister = std::thread::Builder::new().name("glimpsfy-microphones".into()).spawn_scoped(scope, || {
            let _apartment = Apartment::enter();
            endpoints().unwrap_or_default()
        });
        lister.ok().and_then(|thread| thread.join().ok()).unwrap_or_default()
    })
}

fn endpoints() -> windows::core::Result<Vec<Device>> {
    // SAFETY: creates the system's device enumerator in this thread's apartment, which it has
    // entered; the calls below take constants and return new objects.
    let (devices, default_id) = unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let default_id = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole).ok().and_then(|d| device_id(&d));
        (enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?, default_id)
    };
    // SAFETY: the collection is live; its count bounds the indexes asked for below.
    let count = unsafe { devices.GetCount() }?;
    let mut found = Vec::new();
    for index in 0..count {
        // SAFETY: the index is below the count.
        let Ok(device) = (unsafe { devices.Item(index) }) else { continue };
        let Some(id) = device_id(&device) else { continue };
        // SAFETY: the device is live; read access to its properties needs no rights.
        let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok();
        let name = store.as_ref().and_then(|s| text(s, &PKEY_Device_FriendlyName)).unwrap_or_else(|| id.clone());
        let transport = store
            .as_ref()
            .and_then(|s| text(s, &PKEY_Device_EnumeratorName))
            .map_or(DeviceTransport::Other, |e| transport(&e));
        found.push(Device { is_default: default_id.as_deref() == Some(id.as_str()), id, name, transport });
    }
    Ok(found)
}

/// The endpoint id, which the capture crate opens the microphone by.
fn device_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: the call returns a NUL-terminated string allocated for the caller, which is copied
    // and then freed with the allocator its documentation names.
    unsafe {
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text
    }
}

/// A string property of a device.
fn text(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: the key outlives the call, and the variant it returns is cleared by the guard.
    let value = Variant(unsafe { store.GetValue(key) }.ok()?);
    value.text().filter(|t| !t.is_empty())
}

/// How a device is attached, from the bus its driver enumerates it on.
fn transport(enumerator: &str) -> DeviceTransport {
    match enumerator.to_ascii_uppercase().as_str() {
        "USB" => DeviceTransport::Usb,
        "BTHENUM" | "BTHHFENUM" | "BTHLEDEVICE" | "BTHLE" => DeviceTransport::Bluetooth,
        "HDAUDIO" | "INTELAUDIO" | "ACPI" | "PCI" => DeviceTransport::BuiltIn,
        "SWD" | "ROOT" | "SW" => DeviceTransport::Virtual,
        _ => DeviceTransport::Other,
    }
}

/// What the privacy settings allow for `capability`: the switch for the whole device, the
/// user's switch for apps, and the one for desktop apps, which this app is.
fn permission(capability: &str) -> PermissionState {
    let key = format!(r"{CONSENT_STORE}\{capability}");
    let desktop = format!(r"{key}\NonPackaged");
    let values = [
        consent_value(HKEY_LOCAL_MACHINE, &key),
        consent_value(HKEY_CURRENT_USER, &key),
        consent_value(HKEY_CURRENT_USER, &desktop),
    ];
    match logic::consent(values.iter().map(|v| v.as_deref())) {
        Consent::Denied => PermissionState::Denied,
        Consent::Allowed => PermissionState::Granted,
        // Windows before the privacy switches, or a policy that hides them.
        Consent::Unknown => PermissionState::NotRequired,
    }
}

/// The `Value` string under `root\subkey`, if it can be read.
fn consent_value(root: HKEY, subkey: &str) -> Option<String> {
    let subkey: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let name: Vec<u16> = "Value".encode_utf16().chain(std::iter::once(0)).collect();
    let mut buffer = [0u16; 64];
    let mut bytes = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: the key and value names are NUL-terminated and outlive the call, which writes at
    // most `bytes` bytes into the buffer and the length it wrote back into `bytes`.
    let status = unsafe {
        RegGetValueW(
            root,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes as *mut u32),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let units = (bytes as usize / 2).min(buffer.len());
    let end = buffer[..units].iter().position(|&u| u == 0).unwrap_or(units);
    Some(String::from_utf16_lossy(&buffer[..end]))
}
