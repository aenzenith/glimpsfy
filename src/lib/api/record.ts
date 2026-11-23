// Recording: the setup surface, the take (start, pause, resume, stop, discard, mute), the
// devices and their meters, the webcam bubble, the trim window and export. Mirrors
// src-tauri/src/commands/record.rs, src-tauri/src/recording.rs and crates/record/src/lib.rs.
//
// Coordinates: every rectangle named `global` is in global logical points (origin at the primary
// display's top-left corner, y down); a setup's `area`, `window.rect` and `windows[].rect` are
// local to the setup's display, as an overlay selection is.

import type { DisplayId, LogicalPoint, LogicalRect, PhysicalSize } from "./capture";
import { call, on, onWindow } from "./core";
import type { ErrorCode } from "./core";
import type { PermissionState } from "./permissions";

export type RecordingId = string;
export type WindowId = number;

/** What a recording captures; an area is display-local and logical, as a selection is. */
export type StreamTarget =
  | { kind: "display"; display: DisplayId }
  | { kind: "window"; window: WindowId }
  | { kind: "area"; display: DisplayId; rect: LogicalRect };

/** A microphone; `deviceId: null` is the system's default input. */
export type MicrophoneSource = { deviceId: string | null };

export type VideoCodec = "h264" | "hevc";
export type Resolution = "native" | "half";
export type RingColor = "yellow" | "red" | "blue" | "white";
export type RingSize = "small" | "large";
export type OverlaySize = "small" | "medium" | "large";
export type KeystrokePosition = "bottomCenter" | "bottomLeft" | "bottomRight" | "topCenter";
export type WebcamShape = "circle" | "roundedSquare";
export type Corner = "bottomRight" | "bottomLeft" | "topRight" | "topLeft";

export type AudioConfig = {
  system: boolean;
  microphone: MicrophoneSource | null;
  /** Each source gets its own track, plus a mixed track so the file plays everywhere. */
  separateTracks: boolean;
};

export type ClickHighlight = { enabled: boolean; color: RingColor; size: RingSize };

export type KeystrokeOverlay = {
  enabled: boolean;
  position: KeystrokePosition;
  size: OverlaySize;
  /** Only keys pressed with ⌘, ⌥ or ⌃ appear, so typed text stays out of the video. */
  onlyShortcuts: boolean;
};

export type WebcamOverlay = {
  enabled: boolean;
  deviceId: string | null;
  shape: WebcamShape;
  size: OverlaySize;
  corner: Corner;
};

export type RecordingConfig = {
  target: StreamTarget;
  /** The frame rate is 30 or 60. */
  fps: number;
  resolution: Resolution;
  codec: VideoCodec;
  showsCursor: boolean;
  audio: AudioConfig;
  clicks: ClickHighlight;
  keystrokes: KeystrokeOverlay;
  webcam: WebcamOverlay;
  /** 0 starts at once. */
  countdownSeconds: number;
  outputFolder: string;
};

/** Where the recorder is (`record://state`). `finishing` is the stopping step: the file is being
 * completed. A paused take whose `discardAsked` is true shows the discard question in the bar. */
export type RecordingState =
  | { kind: "idle" }
  | { kind: "countdown"; id: RecordingId; remainingSeconds: number; totalSeconds: number }
  /** `microphone` says whether the take records a microphone at all. */
  | { kind: "recording"; id: RecordingId; elapsedMs: number; microphone: boolean; microphoneMuted: boolean }
  | {
      kind: "paused";
      id: RecordingId;
      elapsedMs: number;
      microphone: boolean;
      microphoneMuted: boolean;
      discardAsked: boolean;
    }
  | { kind: "finishing"; id: RecordingId };

export type TrackKind = "video" | "systemAudio" | "microphone" | "mixedAudio";
export type TrackInfo = { kind: TrackKind; codec: string };

export type RecordingResult = {
  id: RecordingId;
  path: string;
  /** The duration leaves paused time out. */
  durationMs: number;
  size: PhysicalSize;
  fileBytes: number;
  tracks: TrackInfo[];
  codec: VideoCodec;
  fps: number;
  createdAtMs: number;
  /** Why the take stopped on its own: the disk is almost full. `null` for an ordinary stop. */
  stoppedBecause: "diskFull" | null;
};

/** How a take ended (`record://finished`), sent once after the state returns to `idle`. */
export type RecordingOutcome =
  | { kind: "saved"; result: RecordingResult }
  /** The file went to the Trash (`trashed`), or nothing had been written yet. */
  | { kind: "discarded"; id: RecordingId; trashed: boolean }
  /** The take failed; what was written stays on disk as `partial` when there is any. */
  | { kind: "failed"; id: RecordingId; code: ErrorCode; message: string; partial: RecordingResult | null }
  /** The countdown was cancelled before anything was recorded. */
  | { kind: "cancelled"; id: RecordingId };

/** `endMs` is exclusive; both count from the start of the video. */
export type TrimRange = { startMs: number; endMs: number };

export type Mp4Quality = "high" | "medium" | "low";

export type ExportFormat =
  | { kind: "mp4"; resolution: Resolution; quality: Mp4Quality }
  | { kind: "gif"; fps: number; width: number | null; loopForever: boolean };

/** What the export does with the file: `save` writes it into the recordings folder (or
 * `destination`), `copy` writes it to the cache and puts the file itself on the clipboard, and
 * `upload` writes it to the cache for `upload_file`. */
export type ExportPurpose = "save" | "copy" | "upload";

export type ExportOptions = {
  format: ExportFormat;
  trim: TrimRange | null;
  /** `null` writes into the recordings folder; an existing file is never overwritten. */
  destination: string | null;
  /** Which audio sources the MP4 keeps; a GIF has no sound. */
  audio: { microphone: boolean; system: boolean };
  purpose: ExportPurpose;
  /** The parts cut from the text (source milliseconds, sorted and apart, inside `trim`), which
   * the file skips; the recording itself never changes. A file with cuts is named "… (trimmed)". */
  cuts: TrimRange[];
};

export type ExportResult = {
  recordingId: RecordingId;
  path: string;
  durationMs: number;
  size: PhysicalSize;
  fileBytes: number;
  /** False when the MP4 kept the original video stream (a pass-through trim). */
  reencoded: boolean;
  /** The part that was really exported: a pass-through trim starts at a keyframe. */
  trim: TrimRange;
  /** The source ranges the file holds, in order, once the cuts were skipped. */
  kept: TrimRange[];
};

export type ExportProgress = {
  recordingId: RecordingId;
  format: "mp4" | "gif";
  /** From 0 to 1. */
  fraction: number;
  /** The time left, once the export has run long enough to tell. */
  remainingMs: number | null;
};

export type DeviceTransport = "builtIn" | "usb" | "bluetooth" | "virtual" | "other";

export type Device = { id: string; name: string; isDefault: boolean; transport: DeviceTransport };

export type RecordingDevices = {
  microphones: Device[];
  cameras: Device[];
  /** Whether the system can record its own audio output here. */
  systemAudio: boolean;
  microphonePermission: PermissionState;
  cameraPermission: PermissionState;
};

/** Live input levels from 0 to 1 (`record://levels`): one per metered microphone id, and the
 * system audio's when it is metered. */
export type MeterLevels = { microphones: Record<string, number>; system: number | null };

export type RecordTick = {
  id: RecordingId;
  elapsedMs: number;
  /** The microphone's peak since the last tick, from 0 to 1, when it is recorded and not muted. */
  microphoneLevel: number | null;
  /** The system audio's peak since the last tick, when it is recorded. */
  systemLevel: number | null;
  fileBytes: number;
};

export type TargetMode = "area" | "window" | "display";

/** A window the setup can record, local to the setup's display. */
/** A window the setup can record; `cornerRadius` is the window's own, when the system reports it. */
export type SetupWindow = { id: WindowId; title: string | null; appName: string; rect: LogicalRect; cornerRadius: number | null };

/** The setup surface's state (`record://setup`): what the take will record. */
export type RecordSetup = {
  mode: TargetMode;
  display: {
    id: DisplayId;
    name: string;
    /** Global. */
    frame: LogicalRect;
    /** The part without the menu bar and the Dock, global. */
    visible: LogicalRect;
    scale: number;
    pixelSize: PhysicalSize;
  };
  /** The area in area mode, local to the display. */
  area: LogicalRect | null;
  /** The chosen window in window mode, local to the display. */
  window: SetupWindow | null;
  /** The windows of the display in window mode, frontmost first, local to the display. */
  windows: SetupWindow[];
  /** The pixel size of the file at the chosen resolution. */
  outputSize: PhysicalSize;
  /** Whether the display chip still shows (display mode, until the first change). */
  chip: boolean;
  camera: { on: boolean; deviceId: string | null; size: "small" | "large"; corner: Corner };
  /** Whether this take records the microphone; it starts from Settings › Recording and a change
   * here holds for this take only. */
  microphone: boolean;
  /** What Record will record; `null` until a window is chosen in window mode. */
  target: StreamTarget | null;
};

export type SetupUpdate = {
  mode?: TargetMode;
  /** A new area, local to the display (area mode). */
  rect?: LogicalRect;
  windowId?: WindowId;
  displayId?: DisplayId;
  camera?: boolean;
  cameraDevice?: string | null;
  /** Records the microphone in this take or not, without changing the settings. */
  microphone?: boolean;
  /** The chip was seen and goes away. */
  chipSeen?: boolean;
};

/** Which side of the bar its popover hangs on; the page draws the bar at the other edge of its
 * window, which is a stage of `width` × `height` points: the bar and, on the popover's side, the
 * room for whatever hangs from it, so a popover opens without the window moving
 * (`record://bar-layout` too). */
export type BarLayout = { popoverSide: "below" | "above"; width: number; height: number };

/** How the bar page draws its glass, so the core lays the native blur and shadow under it: the
 * side it lays the bar out for, the bar's corner radius and, for what hangs from it, its corner
 * radius, its distance from the window's right edge and whether it casts the kit's small shadow
 * instead of the floating one. */
export type BarGlass = {
  side: "below" | "above";
  barRadius: number;
  popoverRadius: number;
  popoverRight: number;
  popoverSmall: boolean;
};

/** The webcam bubble (`record://bubble`), in global points. */
export type BubbleState = {
  visible: boolean;
  size: "small" | "large";
  corner: Corner;
  rect: LogicalRect;
  dragging: boolean;
  /** The corner the bubble snaps to when the drag ends. */
  near: Corner | null;
  /** The four corner slots of the recorded area. */
  slots: { corner: Corner; rect: LogicalRect }[];
};

/** What the canvas window (click rings and keystrokes, recorded) draws (`record://canvas`). Its
 * window covers `rect` exactly. */
export type CanvasConfig = {
  mode: TargetMode;
  /** Global. */
  rect: LogicalRect;
  clicks: ClickHighlight;
  keystrokes: KeystrokeOverlay;
  /** How far the keystroke overlay's bottom place rises (above the Dock in a display recording). */
  liftBottom: number;
  /** How far its top place drops (under the menu bar in a display recording). */
  dropTop: number;
};

export type InkTool = "pen" | "highlighter" | "arrow" | "rect";
export type InkColor = "red" | "yellow" | "blue";

/** Drawing and the spotlight on the take that runs (`record://present`); `null` without one. */
export type PresentState = {
  drawing: boolean;
  spotlight: boolean;
  /** Drawings fade 3 s after the last stroke. */
  fade: boolean;
  tool: InkTool;
  ink: InkColor;
  /** The spotlight's radius in points, 40 to 240. */
  radius: number;
};

/** A change from the card, the rail or the canvas; what is left out stays. */
export type PresentChange = Partial<Pick<PresentState, "drawing" | "spotlight" | "fade" | "tool" | "ink" | "radius">> & {
  /** The last drawing goes. */
  undo?: boolean;
  /** Every drawing goes. */
  clear?: boolean;
};

/** What the canvas does with its drawings (`record://ink`). */
export type InkCommand = { kind: "undo" } | { kind: "clear" };

/** The trim window's recording (`record://trim`). */
export type RecordingInfo = {
  result: RecordingResult;
  /** Plays in a `<video>` element, served with ranges. */
  url: string;
  /** The recordings folder's own name, for "Save writes the file to …". */
  folderName: string;
  /** Which audio tracks the file holds. */
  audio: { microphone: boolean; system: boolean; mixed: boolean };
  /** The history entry of the take, when the history keeps it; an upload passes it on. */
  entryId: string | null;
  /** The format the export panel starts with ("Export as GIF" in the history asks for `gif`). */
  startFormat: "mp4" | "gif";
};

export const RECORD_TICK = "record://tick";
export const RECORD_STATE = "record://state";
export const RECORD_FINISHED = "record://finished";
export const RECORD_EXPORT_PROGRESS = "record://export-progress";
export const RECORD_SETUP = "record://setup";
export const RECORD_LEVELS = "record://levels";
export const RECORD_BUBBLE = "record://bubble";
export const RECORD_CANVAS = "record://canvas";
export const RECORD_CLICK = "record://click";
export const RECORD_KEYS = "record://keys";
export const RECORD_POINTER = "record://pointer";
export const RECORD_TRIM = "record://trim";
export const RECORD_BAR_LAYOUT = "record://bar-layout";
export const RECORD_PRESENT = "record://present";
export const RECORD_INK = "record://ink";

/** The URL scheme that serves recordings, their frames and their waveforms
 * (`glimpsfy-recording://file/<id>`; `http://glimpsfy-recording.localhost/file/<id>` on Windows). */
const SCHEME =
  typeof navigator !== "undefined" && navigator.userAgent.includes("Windows")
    ? "http://glimpsfy-recording.localhost/"
    : "glimpsfy-recording://";

/** A still of the recording at `ms`, `height` pixels tall (PNG). */
export function recordingFrameUrl(id: RecordingId, ms: number, height: number): string {
  return `${SCHEME}frame/${encodeURIComponent(id)}?t=${Math.max(0, Math.round(ms))}&h=${Math.max(1, Math.round(height))}`;
}

/** The waveform of the recording as JSON `{ peaks: number[] }`, `bins` values from 0 to 1. */
export function recordingWaveUrl(id: RecordingId, bins: number): string {
  return `${SCHEME}wave/${encodeURIComponent(id)}?n=${Math.max(1, Math.round(bins))}`;
}

// ------------------------------------------------------------------------------ the setup

/** Opens the setup surface (the target and the setup bar), or retargets it when it is open. */
export function record_setup(request: { mode: TargetMode; displayId?: DisplayId; rect?: LogicalRect; windowId?: WindowId }): Promise<RecordSetup> {
  return call("record_setup", { request });
}

/** The recorder's state now, for a window that opens or reloads in the middle of a take. */
export function record_state(): Promise<RecordingState> {
  return call("record_state");
}

export function record_setup_state(): Promise<RecordSetup | null> {
  return call("record_setup_state");
}

export function record_setup_update(update: SetupUpdate): Promise<RecordSetup> {
  return call("record_setup_update", { update });
}

/** Close (Esc) on the setup bar. */
export function record_setup_close(): Promise<void> {
  return call("record_setup_close");
}

/** The bar page measured its bar and its open popover; the core places the window and the
 * glass under it. */
export function record_bar_fit(
  bar: { width: number; height: number },
  popover: { width: number; height: number } | null,
  glass: BarGlass,
): Promise<BarLayout> {
  return call("record_bar_fit", { bar, popover, glass });
}

// ------------------------------------------------------------------------------- the take

/** Starts the countdown and then the recording. `null` records the open setup with
 * Settings › Recording. */
export function record_start(config: RecordingConfig | null = null): Promise<RecordingId> {
  return call("record_start", { config });
}

/** Return during the countdown: the take starts now. */
export function record_countdown_skip(): Promise<void> {
  return call("record_countdown_skip");
}

/** Cancel (Esc) during the countdown: back to the setup with every choice intact. */
export function record_countdown_cancel(): Promise<void> {
  return call("record_countdown_cancel");
}

export function record_pause(): Promise<void> {
  return call("record_pause");
}

export function record_resume(): Promise<void> {
  return call("record_resume");
}

/** Stops the recording and resolves when the file is complete. */
export function record_stop(): Promise<RecordingResult> {
  return call("record_stop");
}

/** Without `confirmed`, pauses the take and asks in the control bar (`asked`). With it, after the
 * question, moves the take to the Trash (`discarded`). During the countdown it cancels. */
export function record_discard(confirmed = false): Promise<"asked" | "discarded"> {
  return call("record_discard", { confirmed });
}

/** Keep recording: the discard question goes away and the take goes on as it was. */
export function record_keep(): Promise<void> {
  return call("record_keep");
}

export function record_set_microphone_muted(muted: boolean): Promise<void> {
  return call("record_set_microphone_muted", { muted });
}

// ---------------------------------------------------------------------- devices and meters

export function record_devices(): Promise<RecordingDevices> {
  return call("record_devices");
}

/** Meters these microphones (and the system audio when `system`) with `record://levels`;
 * `null` stops metering. */
export function record_meter(microphones: string[] | null, system = false): Promise<void> {
  return call("record_meter", { microphones, system });
}

// ------------------------------------------------------------------- bubble and canvas

export function record_bubble_state(): Promise<BubbleState | null> {
  return call("record_bubble_state");
}

/** The bubble's hover controls: the other size, or the camera off. */
export function record_bubble_set(change: { size?: "small" | "large"; off?: boolean }): Promise<void> {
  return call("record_bubble_set", { change });
}

export function record_canvas_state(): Promise<CanvasConfig | null> {
  return call("record_canvas_state");
}

// ------------------------------------------------------------------- drawing and spotlight

/** Turns drawing or the spotlight on or off, or changes the tool, the ink, the fade or the
 * spot's radius; fails with `invalid_argument` when no take records or pauses. */
export function record_present(change: PresentChange): Promise<PresentState> {
  return call("record_present", { change });
}

export function record_present_state(): Promise<PresentState | null> {
  return call("record_present_state");
}

/** The rail page measured its glass; the core fits the window and the glass under it. */
export function record_rail_fit(size: { width: number; height: number; radius: number }): Promise<void> {
  return call("record_rail_fit", { size });
}

// ------------------------------------------------------------------- trim and export

/** Opens the trim window for a finished take or a history entry (the history's "Trim and export"
 * and, with `gif`, "Export as GIF"). */
export function record_open_trim(source: { recordingId: RecordingId } | { entryId: string }, format: "mp4" | "gif" = "mp4"): Promise<RecordingInfo> {
  return call("record_open_trim", { source, format });
}

export function record_trim_current(): Promise<RecordingInfo | null> {
  return call("record_trim_current");
}

export function record_info(recordingId: RecordingId): Promise<RecordingInfo> {
  return call("record_info", { recordingId });
}

/** Writes an MP4 or a GIF; progress arrives as `record://export-progress`, and the export goes
 * on when the window closes. */
export function record_export(recordingId: RecordingId, options: ExportOptions): Promise<ExportResult> {
  return call("record_export", { recordingId, options });
}

/** Cancels the running export of a recording; `record_export` then fails with `cancelled`. */
export function record_export_cancel(recordingId: RecordingId): Promise<void> {
  return call("record_export_cancel", { recordingId });
}

/** Moves a finished take to the Trash (the trim window's Discard, after its own question). */
export function record_trash(recordingId: RecordingId): Promise<void> {
  return call("record_trash", { recordingId });
}

// ---------------------------------------------------------------------------- events

export function onRecordTick(handler: (tick: RecordTick) => void) {
  return on<RecordTick>(RECORD_TICK, handler);
}

export function onRecordState(handler: (state: RecordingState) => void) {
  return on<RecordingState>(RECORD_STATE, handler);
}

export function onRecordFinished(handler: (outcome: RecordingOutcome) => void) {
  return on<RecordingOutcome>(RECORD_FINISHED, handler);
}

export function onRecordExportProgress(handler: (progress: ExportProgress) => void) {
  return on<ExportProgress>(RECORD_EXPORT_PROGRESS, handler);
}

export function onRecordSetup(handler: (setup: RecordSetup | null) => void) {
  return on<RecordSetup | null>(RECORD_SETUP, handler);
}

/** To the bar window only: the bar belongs on the other side of its stage, and the window waits
 * until the page has laid it out there and fitted it again. */
export function onRecordBarLayout(handler: (layout: BarLayout) => void) {
  return on<BarLayout>(RECORD_BAR_LAYOUT, handler);
}

export function onRecordLevels(handler: (levels: MeterLevels) => void) {
  return on<MeterLevels>(RECORD_LEVELS, handler);
}

export function onRecordBubble(handler: (bubble: BubbleState | null) => void) {
  return on<BubbleState | null>(RECORD_BUBBLE, handler);
}

export function onRecordCanvas(handler: (canvas: CanvasConfig | null) => void) {
  return onWindow<CanvasConfig | null>(RECORD_CANVAS, handler);
}

/** A click while a take with click rings runs, in global points (to the canvas window). */
export function onRecordClick(handler: (click: LogicalPoint) => void) {
  return onWindow<LogicalPoint>(RECORD_CLICK, handler);
}

/** Key caps to show, already filtered by "only shortcuts" (to the canvas window). Nothing typed
 * is kept or logged anywhere. */
export function onRecordKeys(handler: (keys: { caps: string[] }) => void) {
  return onWindow<{ caps: string[] }>(RECORD_KEYS, handler);
}

/** The pointer in global points while the keystroke overlay, drawing or the spotlight needs it,
 * `null` when it left the recorded rectangle. */
export function onRecordPointer(handler: (pointer: LogicalPoint | null) => void) {
  return onWindow<LogicalPoint | null>(RECORD_POINTER, handler);
}

/** Drawing and the spotlight changed, or the take that has them began or ended (to the `rec-*`
 * panels). */
export function onRecordPresent(handler: (state: PresentState | null) => void) {
  return onWindow<PresentState | null>(RECORD_PRESENT, handler);
}

/** Take back the last stroke or wipe them all (to the canvas window). */
export function onRecordInk(handler: (command: InkCommand) => void) {
  return onWindow<InkCommand>(RECORD_INK, handler);
}

export function onRecordTrim(handler: (info: RecordingInfo) => void) {
  return onWindow<RecordingInfo>(RECORD_TRIM, handler);
}
