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
/// The room a side needs for the bar's popovers (the options card is the tallest) to open there.
const POPOVER_ROOM: f64 = 280.0;
/// The bar window's room for what hangs from the bar, and its least width: a glass popover is
/// 296 points wide (`--pop-w`), a device menu 300.
const STAGE_HANG: f64 = 480.0;
const STAGE_WIDTH: f64 = 320.0;
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
    /// The window's own corner radius, when the system reports one.
    pub corner_radius: Option<f64>,
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
    /// A still of the display that the target draws under the setup, where windows cannot be
    /// transparent (`frames::set_backdrop`); `None` where the live screen shows through.
    pub backdrop: Option<String>,
    /// The free bytes on the recordings folder's volume when the setup opened, which the options
    /// compare with what a take of the time limit would write; `None` when it could not be read.
    pub free_bytes: Option<u64>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PopoverSide {
    #[default]
    Below,
    Above,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BarLayout {
    pub popover_side: PopoverSide,
    /// The stage's size: the page measures where a popover hangs inside it.
    pub width: f64,
    pub height: f64,
}

/// How the bar page draws its glass: the side it lays the bar out for (the bar keeps the stage's
/// other edge), the bar's corner radius and, for what hangs from the bar, its corner radius, its
/// distance from the window's right edge and whether it casts the kit's small shadow (a tooltip,
/// a strong card) instead of the floating one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BarGlass {
    pub side: PopoverSide,
    pub bar_radius: f64,
    pub popover_radius: f64,
    pub popover_right: f64,
    pub popover_small: bool,
}

/// One piece of glass in the bar window, in points from the window's top-left corner; the
/// native backdrop blurs the screen under it and casts its shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassPiece {
    pub rect: LogicalRect,
    pub radius: f64,
    pub small_shadow: bool,
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
            corner_radius: w.corner_radius,
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
        // A window setup starts with the asked window, or the front one.
        let window = match window {
            Some(id) => windows.iter().find(|w| w.id == id).cloned(),
            None if mode == TargetMode::Window => windows.first().cloned(),
            None => None,
        };
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
            backdrop: None,
            free_bytes: None,
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
/// corner, and it never shrinks.
///
/// The window is a stage that holds the bar and, on one side of it, the room for everything that
/// hangs from it (`STAGE_HANG` points, or up to the screen's edge), `STAGE_WIDTH` points wide at
/// least. The side and the stage come from the bar alone, so a popover or a tooltip opens inside
/// a window that neither moves nor grows: a window that grew upward would show the bar too high
/// until WebKit drew it again. Popovers hang away from the area when the screen has room there
/// and over it when it has not. The stage's empty part lets clicks through (`watch`).
pub fn place_bar(
    mode: TargetMode,
    target: LogicalRect,
    display: &SetupDisplay,
    bar: Size,
    pop: Option<Size>,
) -> (LogicalRect, PopoverSide) {
    let v = display.visible;
    let f = display.frame;
    // The preferred side when it has room for a popover, the other when that one has, and else
    // the side with more room.
    let side_for = |top: f64, preferred: PopoverSide| {
        let room = |side: PopoverSide| match side {
            PopoverSide::Below => v.y + v.height - (top + bar.height),
            PopoverSide::Above => top - v.y,
        };
        let other = match preferred {
            PopoverSide::Below => PopoverSide::Above,
            PopoverSide::Above => PopoverSide::Below,
        };
        if room(preferred) >= POPOVER_ROOM {
            preferred
        } else if room(other) >= POPOVER_ROOM || room(other) > room(preferred) {
            other
        } else {
            preferred
        }
    };
    let (bar_right, bar_top, side) = if mode == TargetMode::Display {
        let dock_hidden = (v.y + v.height - (f.y + f.height)).abs() < 1.0;
        let bottom = if dock_hidden { f.y + f.height - ABOVE_BOTTOM } else { v.y + v.height - ABOVE_DOCK };
        let left = f.x + (f.width - bar.width) / 2.0;
        (left + bar.width, bottom - bar.height, PopoverSide::Above)
    } else {
        let right = target.x + target.width;
        let below = target.y + target.height + GAP;
        let above = target.y - GAP - bar.height;
        if below + bar.height <= v.y + v.height {
            (right, below, side_for(below, PopoverSide::Below))
        } else if above >= v.y {
            (right, above, side_for(above, PopoverSide::Above))
        } else if right + GAP + bar.width <= v.x + v.width {
            // Beside, level with the area's bottom.
            let top = (target.y + target.height - bar.height).max(v.y);
            (right + GAP + bar.width, top, side_for(top, PopoverSide::Below))
        } else {
            let top = target.y + target.height - GAP - bar.height;
            (right - GAP, top, side_for(top, PopoverSide::Above))
        }
    };
    let room = match side {
        PopoverSide::Below => v.y + v.height - (bar_top + bar.height),
        PopoverSide::Above => bar_top - v.y,
    };
    // A popover taller than the stage's room makes it grow, once, as far as the screen allows.
    let wanted = STAGE_HANG.max(pop.map(|p| GAP + p.height).unwrap_or(0.0));
    let hang = wanted.min(room).max(0.0);
    let height = bar.height + hang;
    let top = match side {
        PopoverSide::Below => bar_top,
        PopoverSide::Above => bar_top - hang,
    };
    // Wider to the left than the bar, as far as the screen goes, so the bar keeps the right edge.
    let wanted = bar.width.max(STAGE_WIDTH).max(pop.map(|p| p.width).unwrap_or(0.0));
    let width = wanted.min(bar_right - v.x).max(bar.width);
    let left = (bar_right - width).clamp(v.x, (v.x + v.width - width).max(v.x));
    let top = top.clamp(f.y, (f.y + f.height - height).max(f.y));
    (LogicalRect { x: left.round(), y: top.round(), width: width.ceil(), height: height.ceil() }, side)
}

/// Where the bar page draws its glass inside the stage `place_bar` made (`frame`): the bar keeps
/// the stage's right edge and its edge away from the popover's side, and the popover hangs `GAP`
/// from the bar, `popover_right` points in from the right edge (`Bar.vue` lays them out the same
/// way). In points from the stage's top-left corner.
pub fn glass_pieces(
    frame: LogicalRect,
    bar: Size,
    pop: Option<Size>,
    side: PopoverSide,
    glass: BarGlass,
) -> Vec<GlassPiece> {
    let (width, height) = (frame.width, frame.height);
    let piece = |x: f64, y: f64, size: Size, radius: f64, small_shadow: bool| GlassPiece {
        rect: LogicalRect { x, y, width: size.width, height: size.height },
        radius: radius.clamp(0.0, size.width.min(size.height) / 2.0),
        small_shadow,
    };
    let bar_top = match side {
        PopoverSide::Below => 0.0,
        PopoverSide::Above => height - bar.height,
    };
    let mut pieces = vec![piece(width - bar.width, bar_top, bar, glass.bar_radius, false)];
    if let Some(p) = pop.filter(|p| p.width > 0.0 && p.height > 0.0) {
        let top = match side {
            PopoverSide::Below => bar.height + GAP,
            PopoverSide::Above => height - bar.height - GAP - p.height,
        };
        let left = (width - p.width - glass.popover_right).max(0.0);
        pieces.push(piece(left, top, p, glass.popover_radius, glass.popover_small));
    }
    pieces
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
