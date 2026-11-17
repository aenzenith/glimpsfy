// Capture: start a capture, read displays and windows, and drive the overlay. Mirrors
// src-tauri/src/commands/capture.rs and crates/capture/src/{display,geometry,frame}.rs.
//
// Coordinates: everything here is in logical points unless the name says pixels. A `Display`'s
// `frame` and a `WindowInfo`'s `bounds` are global (origin at the primary display's top-left
// corner, y down); a `Selection`'s `rect` and an `OverlayWindow`'s `rect` are local to their
// display. Pixel sizes belong to frames only.

import { call } from "./core";

export type CaptureId = number;
export type DisplayId = number;
export type WindowId = number;

export type LogicalPoint = { x: number; y: number };
export type LogicalSize = { width: number; height: number };
export type LogicalRect = { x: number; y: number; width: number; height: number };
export type PhysicalSize = { width: number; height: number };

export type ColorSpace = "srgb" | "display-p3";
export type FrameFormat = "rgba" | "bmp" | "png";

export type Display = {
  id: DisplayId;
  name: string;
  /** Global, logical. */
  frame: LogicalRect;
  /** Physical pixels per point. */
  scale: number;
  pixelSize: PhysicalSize;
  isPrimary: boolean;
  isBuiltin: boolean;
  colorSpace: ColorSpace;
  refreshRate: number | null;
};

export type WindowInfo = {
  id: WindowId;
  title: string | null;
  appName: string;
  bundleId: string | null;
  pid: number;
  /** Global, logical. */
  bounds: LogicalRect;
  layer: number;
  /** 0 is the frontmost window. */
  zIndex: number;
  alpha: number;
  isOnScreen: boolean;
  cornerRadius: number | null;
};

/** A rectangle of one display's frozen frame, display-local and logical. Output, OCR and upload
 * commands all take it. */
export type Selection = { captureId: CaptureId; displayId: DisplayId; rect: LogicalRect };

/** `area`, `window` and `copyText` open the overlay; `display`, `allDisplays` and
 * `repeatLastArea` capture and deliver at once; `areaDelayed` counts down, then opens it. */
export type CaptureMode =
  | "area"
  | "window"
  | "display"
  | "allDisplays"
  | "repeatLastArea"
  | "areaDelayed"
  | "copyText";

/** What the overlay does when a selection is finished or a window clicked: stay for annotation,
 * copy and close, save and close, or copy the text in it (`ocr_copy_text`). */
export type AfterSelection = "annotate" | "copy" | "save" | "copyText";

/** A window the overlay can snap to, display-local and logical. */
export type OverlayWindow = {
  id: WindowId;
  appName: string;
  title: string | null;
  rect: LogicalRect;
  cornerRadius: number | null;
};

/** The payload of `capture://started`, sent to one overlay window. */
export type OverlayFrame = {
  captureId: CaptureId;
  display: Display;
  frameUrl: string;
  frameSize: PhysicalSize;
  format: FrameFormat;
  colorSpace: ColorSpace;
  /** Frontmost first. */
  windows: OverlayWindow[];
  /** Display-local; present on the display the pointer is on. */
  pointer: LogicalPoint | null;
  mode: CaptureMode;
  afterSelection: AfterSelection;
};

/** The result of `capture_pick_window`: the window's own image is ready, and Copy or Save on a
 * selection that still covers the window deliver it instead of the crop. */
export type PickedWindow = { windowId: WindowId; width: number; height: number };

/** The delayed capture: the one-shot delay of the next capture (0 is off) and the countdown that
 * runs now. The payload of `capture://countdown`. */
export type DelayState = {
  pendingSeconds: number;
  remainingSeconds: number | null;
  mode: CaptureMode | null;
};

export type CloseReason = "cancelled" | "copied" | "saved" | "failed" | "uploaded" | "editor" | "recording" | "pinned";

/** The payload of `capture://closed`. */
export type CaptureClosed = { captureId: CaptureId; reason: CloseReason };

/** The overlay page's timings of its first frame, for the dev summary line. */
export type PaintReport = { fetchMs: number; decodeMs: number; drawMs: number; paintMs: number; bytes: number };

export const capture_start = (mode: CaptureMode) => call<void>("capture_start", { mode });
export const capture_current = () => call<OverlayFrame | null>("capture_current");
/** Ends the capture: `cancelled` (Esc, Close), or `pinned` and `recording` when the page handed
 * the selection on itself. */
export const capture_close = (captureId: CaptureId, reason: "cancelled" | "pinned" | "recording" = "cancelled") =>
  call<void>("capture_close", { captureId, reason });
export const overlay_ready = (captureId: CaptureId, displayId: DisplayId) =>
  call<void>("overlay_ready", { captureId, displayId });
export const overlay_painted = (captureId: CaptureId, displayId: DisplayId, report: PaintReport) =>
  call<void>("overlay_painted", { captureId, displayId, report });
/** The page could not load or draw its frame; the core reloads it once, then gives up on it. */
export const overlay_failed = (captureId: CaptureId, displayId: DisplayId, message: string) =>
  call<void>("overlay_failed", { captureId, displayId, message });
/** Captures the clicked window on its own (window mode, or Space in area mode). */
export const capture_pick_window = (captureId: CaptureId, displayId: DisplayId, windowId: WindowId) =>
  call<PickedWindow>("capture_pick_window", { captureId, displayId, windowId });
/** Sets the one-shot delay of the next capture: 0, 3, 5 or 10 seconds. */
export const capture_set_delay = (seconds: number) => call<DelayState>("capture_set_delay", { seconds });
export const capture_delay = () => call<DelayState>("capture_delay");
/** Stops a running countdown; answers whether one was running. */
export const capture_cancel_delay = () => call<boolean>("capture_cancel_delay");
export const capture_displays = () => call<Display[]>("capture_displays");
export const capture_windows = () => call<WindowInfo[]>("capture_windows");

/** A delayed capture's countdown started, ticked or ended. Payload: `DelayState`. */
export const CAPTURE_COUNTDOWN = "capture://countdown";
