// Capture: start a capture, read displays and windows, and drive the overlay. Mirrors
// src-tauri/src/commands/capture.rs and crates/capture/src/{display,geometry,frame}.rs.
//
// Coordinates: everything here is in logical points unless the name says pixels. A `Display`'s
// `frame` and a `WindowInfo`'s `bounds` are global (origin at the primary display's top-left
// corner, y down); a `Selection`'s `rect` and an `OverlayWindow`'s `rect` are local to their
// display. Pixel sizes belong to frames only.

import { call } from "./core";
import type { EditorImage } from "./render";
import type { IntervalPlan, IntervalTarget } from "../settings";

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
  /** The window keeps itself out of captures, so a frame shows what lies behind it. */
  captureBlocked: boolean;
};

/** A rectangle of one display's frozen frame, display-local and logical. Output, OCR and upload
 * commands all take it. */
export type Selection = { captureId: CaptureId; displayId: DisplayId; rect: LogicalRect };

/** `area`, `window` and `copyText` open the overlay, `ruler` opens it to measure without a
 * selection, `pickColor` opens the magnifier alone, and `scrollCapture` opens it for a scrolling
 * capture (macOS); `display`, `allDisplays` and `repeatLastArea` capture and deliver at once;
 * `areaDelayed` counts down, then opens it. */
export type CaptureMode =
  | "area"
  | "window"
  | "display"
  | "allDisplays"
  | "repeatLastArea"
  | "areaDelayed"
  | "copyText"
  | "ruler"
  | "pickColor"
  | "scrollCapture";

/** What the overlay does when a selection is finished or a window clicked: stay for annotation,
 * copy and close, save and close, or copy the text in it (`ocr_copy_text`).
 * `interval`: the selection becomes the area of an interval series (`capture_interval_area`).
 * `agent`: an agent asked for an area the user selects, and the selection goes to it
 * (`agents_take_selection`). `scroll`: the selection becomes a scrolling capture
 * (`capture_start_scroll`). */
export type AfterSelection = "annotate" | "copy" | "save" | "copyText" | "interval" | "agent" | "scroll";

/** A window the overlay can snap to, display-local and logical. */
export type OverlayWindow = {
  id: WindowId;
  appName: string;
  title: string | null;
  rect: LogicalRect;
  cornerRadius: number | null;
  /** The window keeps itself out of captures; the ruler hatches it and measures nothing there,
   * and the colour picker reads nothing there. */
  captureBlocked: boolean;
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
  /** The beautify style the capture's images leave with (the automatic preset, or what the page
   * set since), or `null` for the images as they are. Read it with `readBeautify`. */
  beautify: unknown;
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

export type CloseReason =
  | "cancelled"
  | "copied"
  | "saved"
  | "failed"
  | "uploaded"
  | "editor"
  | "recording"
  | "pinned"
  | "interval"
  /** The selection went to the agent that asked for it (`agents_take_selection`). */
  | "agent";

/** The interval series that runs (`IntervalState` in commands/capture.rs), the payload of
 * `capture://interval`. An idle state has `running` false and nothing else filled in. */
export type IntervalState = {
  running: boolean;
  target: IntervalTarget | null;
  /** What the series follows: the window's app, or the display's name. */
  label: string | null;
  /** The series' name in the history: the followed window, or its folder's name. */
  name: string | null;
  everyMs: number;
  /** The frames due so far, skipped ones included. */
  taken: number;
  /** The frames the series takes at most: its end, or the cap of a manual end. */
  total: number;
  /** The series runs until it is stopped (and at most 12 hours). */
  manual: boolean;
  skipped: number;
  lastSkippedAtMs: number | null;
  startedAtMs: number;
  nextAtMs: number | null;
  /** The series' history entry, once its first frame is on disk and the history is on. */
  entryId: string | null;
  /** The newest frame on disk, for `frameUrl(entryId, latestIndex)`. */
  latestIndex: number | null;
  latestAtMs: number | null;
  folder: string | null;
  folderName: string | null;
};

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
/** Sets the beautify style this capture's images leave with (`writeBeautify` of a style), or
 * `null` to leave them as they are; it belongs to this capture alone. A capture that is gone
 * throws `not_found`. */
export const capture_set_beautify = (captureId: CaptureId, style: unknown) =>
  call<void>("capture_set_beautify", { captureId, style });
/** Sets the one-shot delay of the next capture: 0, 3, 5 or 10 seconds. */
export const capture_set_delay = (seconds: number) => call<DelayState>("capture_set_delay", { seconds });
export const capture_delay = () => call<DelayState>("capture_delay");
/** Stops a running countdown; answers whether one was running. */
export const capture_cancel_delay = () => call<boolean>("capture_cancel_delay");
export const capture_displays = () => call<Display[]>("capture_displays");
export const capture_windows = () => call<WindowInfo[]>("capture_windows");

/** Starts an interval series with `plan` (kept in Settings for the next time), or the last
 * series again without one. The area target opens the overlay first; `busy` while a series runs,
 * `permission_denied` without the screen recording permission. */
export const capture_start_interval = (plan: IntervalPlan | null) =>
  call<IntervalState>("capture_start_interval", { plan });
/** The overlay's selection becomes the area of the series it was opened for. */
export const capture_interval_area = (selection: Selection) =>
  call<IntervalState>("capture_interval_area", { selection });
/** Stops the series that runs; its frames stay. Answers whether one ran. */
export const capture_stop_interval = () => call<boolean>("capture_stop_interval");
export const capture_interval = () => call<IntervalState>("capture_interval");

/** The state of a scrolling capture, with `capture://scroll` and from every `capture_*_scroll`
 * command. Sizes are physical pixels of the result. */
export type ScrollProgress = {
  captureId: CaptureId;
  displayId: DisplayId;
  width: number;
  height: number;
  /** Frames that brought rows in or moved the view. */
  frames: number;
  /** The sticky header and bottom bar in rows; `found` once a second move showed them again. */
  header: number;
  headerFound: boolean;
  footer: number;
  footerFound: boolean;
  /** Where one piece of the result ends and the next begins, in rows. */
  gaps: number[];
  /** The part of the result the selection shows now: its first row and its height. */
  view: [number, number] | null;
  /** Which way the gap lies from what the selection shows: the user scrolls that way. */
  gap: "above" | "below" | null;
  /** The memory the compressed rows take. */
  bytes: number;
  /** The tallest result; the mode finishes by itself there. */
  limit: number;
  auto: boolean;
  /** Glimpsfy may post scroll events (Accessibility). */
  autoAllowed: boolean;
  /** Automatic scrolling stopped by itself at the page's end. */
  ended: boolean;
  /** The strip's preview of the result, once there is one (`glimpsfy-frame://scroll/…`). */
  previewUrl: string | null;
};

/** Turns the selection into a scrolling capture (macOS): the page under it can be scrolled, and
 * the frames grow one tall image. */
export const capture_start_scroll = (selection: Selection) => call<ScrollProgress>("capture_start_scroll", { selection });
/** Starts or stops automatic scrolling; starting throws `permission_denied` without Accessibility
 * and `not_found` when no window lies under the selection. */
export const capture_set_autoscroll = (on: boolean) => call<ScrollProgress>("capture_set_autoscroll", { on });
/** Takes back the rows of the last movement (⌘Z). */
export const capture_undo_scroll = () => call<ScrollProgress>("capture_undo_scroll");
/** Ends the scrolling capture: the result opens in the editor and the capture closes. */
export const capture_finish_scroll = () => call<EditorImage>("capture_finish_scroll");
/** Drops the result; the overlay goes back to its frozen selection. Answers whether one ran. */
export const capture_cancel_scroll = () => call<boolean>("capture_cancel_scroll");
/** The overlay's parts that lie over the selection (window-local points), where the pointer would
 * otherwise go through to the app below; they keep it, so their buttons can be clicked. */
export const capture_cover_scroll = (covers: LogicalRect[]) => call<void>("capture_cover_scroll", { covers });

/** A scrolling capture's result grew, moved, lost a movement or stopped scrolling by itself.
 * Payload: `ScrollProgress`. */
export const CAPTURE_SCROLL = "capture://scroll";
/** A delayed capture's countdown started, ticked or ended. Payload: `DelayState`. */
export const CAPTURE_COUNTDOWN = "capture://countdown";
/** An interval series started, took or skipped a frame, or ended. Payload: `IntervalState`. */
export const CAPTURE_INTERVAL = "capture://interval";
