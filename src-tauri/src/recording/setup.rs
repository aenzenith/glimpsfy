//! The recording setup: what the take will record (a display, an area or a window), where the
//! setup and control bars sit beside it, and the `RecordingConfig` Record builds from the setup
//! and Settings › Recording.

use glimpsfy_capture::{
    Display, DisplayId, LogicalPoint, LogicalRect, MicrophoneSource, PermissionState, PhysicalSize, StreamTarget,
    WindowId, WindowInfo, WindowListOptions,
};
use glimpsfy_record::{
    AudioConfig, ClickHighlight, Corner, KeystrokeOverlay, KeystrokePosition, OverlaySize, RecordingConfig, Resolution,
    RingColor, RingSize, VideoCodec, WebcamOverlay, WebcamShape,
};
use serde::{Deserialize, Serialize};

use crate::permissions::Permissions;
use crate::settings::{self, Recording};

/// The gap between a bar and the area, and between a bar and its popover.
const GAP: f64 = 8.0;
/// A display's bar sits this far above the Dock, or above the bottom when the Dock hides.
const ABOVE_DOCK: f64 = 16.0;
const ABOVE_BOTTOM: f64 = 24.0;
/// The bubble's sizes and its inset from the recorded area's corner.
pub const BUBBLE_SMALL: f64 = 132.0;
pub const BUBBLE_LARGE: f64 = 200.0;
pub const BUBBLE_INSET: f64 = 16.0;
/// The keystroke overlay's margin, and how far above the Dock it rises in a display recording.
const KEYS_MARGIN: f64 = 24.0;
const KEYS_ABOVE_DOCK: f64 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetMode {
    Area,
    Window,
    Display,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupDisplay {
    pub id: DisplayId,
    pub name: String,
    /// Global.
    pub frame: LogicalRect,
    /// The part without the menu bar and the Dock, global.
    pub visible: LogicalRect,
    pub scale: f64,
    pub pixel_size: PhysicalSize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupWindow {
    pub id: WindowId,
    pub title: Option<String>,
    pub app_name: String,
    /// Local to the setup's display.
    pub rect: LogicalRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BubbleSize {
    Small,
    Large,
}

impl BubbleSize {
    pub fn points(self) -> f64 {
        match self {
            BubbleSize::Small => BUBBLE_SMALL,
            BubbleSize::Large => BUBBLE_LARGE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraSetup {
    pub on: bool,
    pub device_id: Option<String>,
    pub size: BubbleSize,
    pub corner: Corner,
}

/// The setup surface's state (`record://setup`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordSetup {
    pub mode: TargetMode,
    pub display: SetupDisplay,
    /// Local to the display.
    pub area: Option<LogicalRect>,
    pub window: Option<SetupWindow>,
    pub windows: Vec<SetupWindow>,
    pub output_size: PhysicalSize,
    pub chip: bool,
    pub camera: CameraSetup,
    /// Whether this take records the microphone; it starts from Settings › Recording and the bar
    /// changes it for this take only.
    pub microphone: bool,
    pub target: Option<StreamTarget>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupRequest {
    pub mode: Option<TargetMode>,
    pub display_id: Option<DisplayId>,
    pub rect: Option<LogicalRect>,
    pub window_id: Option<WindowId>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupUpdate {
    pub mode: Option<TargetMode>,
    pub rect: Option<LogicalRect>,
    pub window_id: Option<WindowId>,
    pub display_id: Option<DisplayId>,
    pub camera: Option<bool>,
    /// `Some(None)` picks the default camera.
    #[serde(default, deserialize_with = "some")]
    pub camera_device: Option<Option<String>>,
    pub chip_seen: Option<bool>,
    /// The microphone on or off for this take; settings keep their value.
    pub microphone: Option<bool>,
}

/// Tells a field that came as `null` from one that did not come at all.
fn some<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PopoverSide {
    Below,
    Above,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BarLayout {
    pub popover_side: PopoverSide,
}

pub fn to_global(rect: LogicalRect, display: &SetupDisplay) -> LogicalRect {
    LogicalRect { x: rect.x + display.frame.x, y: rect.y + display.frame.y, ..rect }
}

fn to_local(rect: LogicalRect, display: &SetupDisplay) -> LogicalRect {
    LogicalRect { x: rect.x - display.frame.x, y: rect.y - display.frame.y, ..rect }
}

pub fn describe(display: &Display, visible: Option<LogicalRect>) -> SetupDisplay {
    SetupDisplay {
        id: display.id,
        name: display.name.clone(),
        frame: display.frame,
        visible: visible.unwrap_or(display.frame),
        scale: display.scale.0,
        pixel_size: display.pixel_size,
    }
}

/// The display under `point`, or the primary one.
pub fn display_at(displays: &[Display], point: LogicalPoint) -> Option<&Display> {
    displays
        .iter()
        .find(|d| d.frame.contains(point))
        .or_else(|| displays.iter().find(|d| d.is_primary))
        .or(displays.first())
}

/// The ordinary windows that show on `display`, frontmost first, local to it.
pub fn windows_on(all: &[WindowInfo], display: &SetupDisplay) -> Vec<SetupWindow> {
    let mut on: Vec<&WindowInfo> = all
        .iter()
        .filter(|w| (0..=8).contains(&w.layer) && w.alpha > 0.0 && w.bounds.width >= 40.0 && w.bounds.height >= 40.0)
        .filter(|w| w.bounds.intersection(&display.frame).is_some())
        .collect();
    on.sort_by_key(|w| w.z_index);
    on.into_iter()
        .map(|w| SetupWindow {
            id: w.id,
            title: w.title.clone(),
            app_name: w.app_name.clone(),
            rect: to_local(w.bounds, display),
        })
        .collect()
}

pub fn window_options() -> WindowListOptions {
    WindowListOptions::default()
}

/// The first area of a new setup: the middle of the visible part of the display.
fn default_area(display: &SetupDisplay) -> LogicalRect {
    let v = to_local(display.visible, display);
    let (w, h) = ((v.width * 0.6).round(), (v.height * 0.6).round());
    LogicalRect { x: (v.x + (v.width - w) / 2.0).round(), y: (v.y + (v.height - h) / 2.0).round(), width: w, height: h }
}

fn clamp_area(rect: LogicalRect, display: &SetupDisplay) -> LogicalRect {
    let (dw, dh) = (display.frame.width, display.frame.height);
    let width = rect.width.clamp(16.0, dw);
    let height = rect.height.clamp(16.0, dh);
    LogicalRect { x: rect.x.clamp(0.0, dw - width), y: rect.y.clamp(0.0, dh - height), width, height }
}

fn even(n: f64) -> u32 {
    ((n.max(2.0) as u32) & !1).max(2)
}

impl RecordSetup {
    pub fn new(
        mode: TargetMode,
        display: SetupDisplay,
        windows: Vec<SetupWindow>,
        rect: Option<LogicalRect>,
        window: Option<WindowId>,
        camera: CameraSetup,
        microphone: bool,
    ) -> RecordSetup {
        let area = Some(rect.map(|r| clamp_area(r, &display)).unwrap_or_else(|| default_area(&display)));
        let window = window.and_then(|id| windows.iter().find(|w| w.id == id).cloned());
        let mut setup = RecordSetup {
            mode,
            chip: mode == TargetMode::Display,
            display,
            area,
            window,
            windows,
            output_size: PhysicalSize { width: 2, height: 2 },
            camera,
            microphone,
            target: None,
        };
        setup.settle(Resolution::Native);
        setup
    }

    /// The recorded rectangle, local to the display.
    pub fn local_rect(&self) -> Option<LogicalRect> {
        match self.mode {
            TargetMode::Area => self.area,
            TargetMode::Window => self.window.as_ref().map(|w| w.rect),
            TargetMode::Display => Some(LogicalRect { x: 0.0, y: 0.0, ..self.display.frame }),
        }
    }

    /// The recorded rectangle in global points.
    pub fn global_rect(&self) -> Option<LogicalRect> {
        self.local_rect().map(|r| to_global(r, &self.display))
    }

    /// Works out the target and the file's pixel size after a change.
    pub fn settle(&mut self, resolution: Resolution) {
        self.target = match self.mode {
            TargetMode::Area => self.area.map(|rect| StreamTarget::Area { display: self.display.id, rect }),
            TargetMode::Window => self.window.as_ref().map(|w| StreamTarget::Window { window: w.id }),
            TargetMode::Display => Some(StreamTarget::Display { display: self.display.id }),
        };
        let divisor = match resolution {
            Resolution::Native => 1.0,
            Resolution::Half => 2.0,
        };
        let rect = self.local_rect().unwrap_or(self.display.frame);
        let scale = self.display.scale.max(1.0);
        self.output_size =
            PhysicalSize { width: even(rect.width * scale / divisor), height: even(rect.height * scale / divisor) };
    }

    pub fn set_area(&mut self, rect: LogicalRect) {
        self.area = Some(clamp_area(rect, &self.display));
        self.chip = false;
    }
}

// ------------------------------------------------------------------------------- the bars

/// Where the bar window goes for a bar of `bar` with an optional popover of `pop`, beside the
/// recorded `target` (global): a display's bar sits centred above the Dock; an area's or a
/// window's bar is right-aligned under it, then above it, then beside it, then inside its
/// corner, and it never shrinks. The popover hangs away from the area, so it never covers it.
pub fn place_bar(
    mode: TargetMode,
    target: LogicalRect,
    display: &SetupDisplay,
    bar: Size,
    pop: Option<Size>,
) -> (LogicalRect, PopoverSide) {
    let v = display.visible;
    let f = display.frame;
    let hang = pop.map(|p| GAP + p.height).unwrap_or(0.0);
    let width = bar.width.max(pop.map(|p| p.width).unwrap_or(0.0));
    let (bar_right, bar_top, side) = if mode == TargetMode::Display {
        let dock_hidden = (v.y + v.height - (f.y + f.height)).abs() < 1.0;
        let bottom = if dock_hidden { f.y + f.height - ABOVE_BOTTOM } else { v.y + v.height - ABOVE_DOCK };
        let left = f.x + (f.width - bar.width) / 2.0;
        (left + bar.width, bottom - bar.height, PopoverSide::Above)
    } else {
        let right = target.x + target.width;
        let below = target.y + target.height + GAP;
        let above = target.y - GAP - bar.height;
        if below + bar.height + hang <= v.y + v.height {
            (right, below, PopoverSide::Below)
        } else if above - hang >= v.y {
            (right, above, PopoverSide::Above)
        } else if right + GAP + bar.width <= v.x + v.width {
            // Beside, level with the area's bottom; the popover hangs where there is room.
            let top = (target.y + target.height - bar.height).max(v.y);
            let side = if top + bar.height + hang <= v.y + v.height { PopoverSide::Below } else { PopoverSide::Above };
            (right + GAP + bar.width, top, side)
        } else {
            let top = target.y + target.height - GAP - bar.height;
            let side = if top - hang >= v.y { PopoverSide::Above } else { PopoverSide::Below };
            (right - GAP, top, side)
        }
    };
    let height = bar.height + hang;
    let top = match side {
        PopoverSide::Below => bar_top,
        PopoverSide::Above => bar_top - hang,
    };
    let left = (bar_right - width).clamp(v.x, (v.x + v.width - width).max(v.x));
    let top = top.clamp(f.y, (f.y + f.height - height).max(f.y));
    (LogicalRect { x: left.round(), y: top.round(), width: width.ceil(), height: height.ceil() }, side)
}

// ----------------------------------------------------------------------- the bubble's place

/// The bubble's four corner slots inside the recorded rectangle (global), 16 points in.
pub fn bubble_slots(area: LogicalRect, size: f64) -> [(Corner, LogicalRect); 4] {
    let left = area.x + BUBBLE_INSET;
    let right = area.x + area.width - BUBBLE_INSET - size;
    let top = area.y + BUBBLE_INSET;
    let bottom = area.y + area.height - BUBBLE_INSET - size;
    let at = |x: f64, y: f64| LogicalRect { x, y, width: size, height: size };
    [
        (Corner::TopLeft, at(left, top)),
        (Corner::TopRight, at(right, top)),
        (Corner::BottomLeft, at(left, bottom)),
        (Corner::BottomRight, at(right, bottom)),
    ]
}

/// The slot nearest to a bubble at `rect`.
pub fn nearest_corner(slots: &[(Corner, LogicalRect); 4], rect: LogicalRect) -> Corner {
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let distance = |r: &LogicalRect| {
        let (x, y) = (r.x + r.width / 2.0 - cx, r.y + r.height / 2.0 - cy);
        x * x + y * y
    };
    slots.iter().min_by(|a, b| distance(&a.1).total_cmp(&distance(&b.1))).map(|(corner, _)| *corner).unwrap_or_default()
}

/// The part of a display recording the bubble and the overlays keep to: without the menu bar
/// and the Dock, which cover the recording's edges.
pub fn usable(mode: TargetMode, rect: LogicalRect, display: &SetupDisplay) -> LogicalRect {
    if mode == TargetMode::Display {
        rect.intersection(&display.visible).unwrap_or(rect)
    } else {
        rect
    }
}

/// How far the keystroke overlay's bottom place rises and its top place drops.
pub fn keys_offsets(mode: TargetMode, display: &SetupDisplay) -> (f64, f64) {
    if mode != TargetMode::Display {
        return (0.0, 0.0);
    }
    let (f, v) = (display.frame, display.visible);
    let dock = (f.y + f.height) - (v.y + v.height);
    let menu = v.y - f.y;
    (if dock > 1.0 { dock + KEYS_ABOVE_DOCK - KEYS_MARGIN } else { 0.0 }, menu.max(0.0))
}

// ------------------------------------------------------------------------- the configuration

pub fn granted(state: PermissionState) -> bool {
    matches!(state, PermissionState::Granted | PermissionState::NotRequired)
}

pub fn bubble_size(size: settings::Size3) -> BubbleSize {
    match size {
        settings::Size3::Large => BubbleSize::Large,
        _ => BubbleSize::Small,
    }
}

pub fn corner(corner: settings::Corner) -> Corner {
    match corner {
        settings::Corner::BottomRight => Corner::BottomRight,
        settings::Corner::BottomLeft => Corner::BottomLeft,
        settings::Corner::TopRight => Corner::TopRight,
        settings::Corner::TopLeft => Corner::TopLeft,
    }
}

pub fn settings_corner(corner: Corner) -> settings::Corner {
    match corner {
        Corner::BottomRight => settings::Corner::BottomRight,
        Corner::BottomLeft => settings::Corner::BottomLeft,
        Corner::TopRight => settings::Corner::TopRight,
        Corner::TopLeft => settings::Corner::TopLeft,
    }
}

pub fn resolution(r: &Recording) -> Resolution {
    match r.resolution {
        settings::RecordingResolution::Native => Resolution::Native,
        settings::RecordingResolution::Half => Resolution::Half,
    }
}

/// The take's configuration: the setup's target and camera with Settings › Recording. A source
/// whose permission is missing is left out, as the bar shows it.
pub fn config(
    r: &Recording,
    setup: &RecordSetup,
    target: StreamTarget,
    permissions: &Permissions,
    system_audio: bool,
) -> RecordingConfig {
    let microphone = (setup.microphone && granted(permissions.microphone))
        .then(|| MicrophoneSource { device_id: r.microphone_device.clone() });
    RecordingConfig {
        target,
        fps: if r.fps == 30 { 30 } else { 60 },
        resolution: resolution(r),
        codec: match r.codec {
            settings::VideoCodec::H264 => VideoCodec::H264,
            settings::VideoCodec::Hevc => VideoCodec::Hevc,
        },
        shows_cursor: true,
        audio: AudioConfig { system: r.system_audio && system_audio, microphone, separate_tracks: r.separate_tracks },
        clicks: ClickHighlight {
            enabled: r.highlight_clicks,
            color: match r.click_ring_color {
                settings::RingColor::Yellow => RingColor::Yellow,
                settings::RingColor::Red => RingColor::Red,
                settings::RingColor::Blue => RingColor::Blue,
                settings::RingColor::White => RingColor::White,
            },
            size: match r.click_ring_size {
                settings::Size2::Small => RingSize::Small,
                settings::Size2::Large => RingSize::Large,
            },
        },
        keystrokes: KeystrokeOverlay {
            enabled: r.show_keys && granted(permissions.accessibility),
            position: match r.keys_position {
                settings::KeysPosition::BottomCentre => KeystrokePosition::BottomCenter,
                settings::KeysPosition::BottomLeft => KeystrokePosition::BottomLeft,
                settings::KeysPosition::BottomRight => KeystrokePosition::BottomRight,
                settings::KeysPosition::TopCentre => KeystrokePosition::TopCenter,
            },
            size: match r.keys_size {
                settings::Size3::Small => OverlaySize::Small,
                settings::Size3::Medium => OverlaySize::Medium,
                settings::Size3::Large => OverlaySize::Large,
            },
            only_shortcuts: r.keys_only_shortcuts,
        },
        webcam: WebcamOverlay {
            enabled: setup.camera.on && granted(permissions.camera),
            device_id: setup.camera.device_id.clone().or_else(|| r.webcam_device.clone()),
            shape: match r.webcam_shape {
                settings::WebcamShape::Circle => WebcamShape::Circle,
                settings::WebcamShape::RoundedSquare => WebcamShape::RoundedSquare,
            },
            size: match setup.camera.size {
                BubbleSize::Small => OverlaySize::Small,
                BubbleSize::Large => OverlaySize::Large,
            },
            corner: setup.camera.corner,
        },
        countdown_seconds: r.countdown_seconds.min(10),
        output_folder: r.recordings_folder.clone(),
    }
}
