// Windows: open the app's windows, the toast, the popover, the pins, the pickers, links and
// quitting. Mirrors src-tauri/src/commands/windows.rs, the toast payload of
// src-tauri/src/windows.rs and the pin types of src-tauri/src/pins.rs.

import type { CaptureId, LogicalPoint, LogicalRect, Selection } from "./capture";
import type { AgentToast } from "./agents";
import type { ErrorCode } from "./core";
import { call } from "./core";
import type { EntryId } from "./history";
import type { SentToast } from "./send";

export type WindowKind = "editor" | "recorder" | "popover" | "history" | "settings" | "firstrun";

/** `diskFull`: a recording stopped because the disk was almost full, and it is saved at `path`.
 * `subjectCopied`: the subjects of a selection reached the clipboard as a transparent PNG; `subjects` says how many.
 * `colorCopied`: the colour picker copied `text`, a colour in `format` or a contrast line, and `swatch` is the colour.
 * `agentSent`: a timed permission sent a capture to an agent; `agent` names it and the minutes left.
 * `agentTimedOut`: nobody answered an agent's request within 60 seconds, so it was denied.
 * `agentLocked`: requests that came while the screen was locked were refused; told once after the unlock. */
export type ToastKind =
  | "copied"
  | "saved"
  | "failed"
  | "textCopied"
  | "diskFull"
  | "pathCopied"
  | "sent"
  | "subjectCopied"
  | "colorCopied"
  | "agentSent"
  | "agentTimedOut"
  | "agentLocked";

/** The payload of `toast://show`. The toast page words it in the user's language. */
export type Toast = {
  id: number;
  kind: ToastKind;
  /** A small PNG of the capture as a data URL. */
  thumbnail: string | null;
  width: number | null;
  height: number | null;
  bytes: number | null;
  /** The saved file, for Show in Finder. */
  path: string | null;
  /** The folder's own name, for "Saved to …". */
  folder: string | null;
  errorCode: ErrorCode | null;
  message: string | null;
  /** The copied text of a `textCopied` toast. */
  text: string | null;
  /** The file format of a `copied` or `saved` toast: an image's, or a recording's or export's; the
   * colour format of a `colorCopied` toast, none for a contrast line. */
  format: "png" | "jpeg" | "webp" | "mp4" | "gif" | "HEX" | "RGB" | "HSL" | "P3" | null;
  /** The save folder that could not take the file, so it went to Pictures › Glimpsfy instead. */
  fallbackFrom: string | null;
  /** The capture a `copied` or `saved` toast reports, which its Send button sends; `null` for the
   * editor's images. */
  captureId?: CaptureId | null;
  /** The send a `sent` toast follows; `send://state` moves it on. */
  send?: SentToast | null;
  /** A `pathCopied` toast's path points into the inbox, deleted after a day (history off). */
  temporary?: boolean;
  /** How many subjects a `subjectCopied` toast's image holds. */
  subjects?: number | null;
  /** The CSS colour a `colorCopied` toast shows in place of a thumbnail. */
  swatch?: string | null;
  /** What an agent's toast (`agentSent`, `agentTimedOut`, `agentLocked`) tells. */
  agent?: AgentToast | null;
};

export const window_open = (kind: WindowKind) => call<void>("window_open", { kind });

export type SettingsSection = "general" | "shortcuts" | "output" | "beautify" | "recording" | "ocr" | "upload" | "agents" | "about";
/** Opens Settings at `section` (Upload, when no upload host is chosen yet) instead of the section
 * shown last. */
export const window_open_settings = (section: SettingsSection) => call<void>("window_open_settings", { section });
/** The section `window_open_settings` asked for, once; the settings page calls it. */
export const window_take_settings_section = () => call<SettingsSection | null>("window_take_settings_section");
/** To the settings window: a section was asked for; read it with `window_take_settings_section`. */
export const SETTINGS_SECTION = "settings://section-requested";
/** Where one toast is drawn at this frame, from the toast window's bottom-right corner, and how
 * opaque it is, for its native blur. */
export type ToastGlass = { key: string; right: number; bottom: number; width: number; height: number; radius: number; alpha: number };
/** The toast window: `width` points wide, its bottom-right corner `pastRight` points right of and
 * `pastBelow` points under the visible area's bottom-right corner, so the toasts' shadows have
 * room past it; it is as tall as the visible area, and clicks go through it but on a toast. It
 * stays on screen, invisible while `shown` is false, so its page keeps drawing at full speed.
 * `agent` is the request the approval card shows whole at this frame (opaque and at rest), or
 * `null`; the core counts the card's 0.6 s before its answers from the first frame that names it. */
export type ToastFrame = { width: number; pastRight: number; pastBelow: number; shown: boolean; agent: number | null };
/** Puts the toast window at `frame` with a native blur under each toast in `glass`. */
export const toast_fit = (frame: ToastFrame, glass: ToastGlass[]) => call<void>("toast_fit", { frame, glass });
/** The pointer in the toast window (points from its top-left corner), or `null` while the window
 * is hidden; the toast's panel never becomes key, so its page cannot follow the pointer itself. */
export const toast_pointer = () => call<LogicalPoint | null>("toast_pointer");
export const app_quit = () => call<void>("app_quit");
/** Writes a page's error or warning to the core's log (dev builds print it). */
export const app_log = (level: "error" | "warn", message: string) => call<void>("app_log", { level, message });

/** Puts text on the clipboard (a link, Markdown, recognised text). */
export const app_copy_text = (text: string) => call<void>("app_copy_text", { text });
/** Opens an http or https page in the browser, a `mailto:` address in the mail app or a `tel:` number in
 * the phone app. Any other scheme, a `mailto:` with a query and a `tel:` with letters fail with
 * `invalid_argument`. */
export const app_open_url = (url: string) => call<void>("app_open_url", { url });
/** Whether an app on this computer opens `url` (only the schemes `app_open_url` takes). */
export const app_can_open_url = (url: string) => call<boolean>("app_can_open_url", { url });

/** An event for `app_add_to_calendar`: local times without a zone ("2026-11-27T14:30:00"). */
export type CalendarEvent = {
  title: string;
  start: string;
  /** The end of a timed event, or the last day of an all-day event over several days. */
  end: string | null;
  allDay: boolean;
};
/** Writes the event as an `.ics` file and opens it with the system's calendar app; no permission is asked. */
export const app_add_to_calendar = (event: CalendarEvent) => call<void>("app_add_to_calendar", { event });

export type SystemPane = "keyboardShortcuts" | "loginItems";
export const app_open_system_settings = (pane: SystemPane) => call<void>("app_open_system_settings", { pane });
/** Quits and starts Glimpsfy again (after the screen recording permission changed). */
export const app_restart = () => call<void>("app_restart");

export type FileFilter = { name: string; extensions: string[] };
/** The folder the user picked, or `null` when they cancelled. */
export const app_choose_folder = (start: string | null) => call<string | null>("app_choose_folder", { start });
/** The file the user picked, or `null` when they cancelled. */
export const app_choose_file = (filters: FileFilter[]) => call<string | null>("app_choose_file", { filters });

// ---------------------------------------------------------------------------------- popover

/** To the popover page each time it opens, so it reloads its captures and the permission. */
export const POPOVER_OPENED = "popover://opened";

export const popover_hide = () => call<void>("popover_hide");
/** The popover's page measured itself; the window takes that height under the icon. */
export const popover_fit = (height: number) => call<void>("popover_fit", { height });
/** Starts dragging a history entry out as a file; call it while the button is still down. */
export const popover_drag_out = (entryId: EntryId) => call<void>("popover_drag_out", { entryId });

// ------------------------------------------------------------------------------------- pins

export type PinSource =
  | { kind: "capture"; selection: Selection }
  | { kind: "history"; entryId: EntryId }
  /** `scale` is the image's pixels per point when the caller knows it (the editor); without it
   * the pin takes the scale of the screen under the pointer. */
  | { kind: "file"; path: string; scale?: number }
  /** A capture with drawings staged by `output_stage_rendered`: the pin opens where `selection`
   * was captured, at its size. Open it before the capture closes; after, it opens centred. */
  | { kind: "rendered"; path: string; selection: Selection };

export type PinInfo = {
  id: number;
  name: string;
  pixelWidth: number;
  pixelHeight: number;
  /** Pixels per point: at zoom 1 the image shows at its own size. */
  scale: number;
  /** The image's place when the pin opened, in global points from the primary display's top-left. */
  image: LogicalRect;
  /** The transparent margin around the image that holds its shadow. */
  margin: number;
  imageType: "png" | "jpeg";
  /** The history entry the pin shows, which the editor can open; null for a capture or a file. */
  entryId: EntryId | null;
  /** Whether the pin shows on every Space (it does when it opens). */
  allSpaces: boolean;
};

/** To a locked pin: ⌥ went down or up, so the pin takes the pointer while it is held. */
export const PIN_OPTION_KEY = "pin://option-key";
export type PinOptionKey = { held: boolean };

/** Opens a pin; the overlay closes its capture itself afterwards. */
export const pin_open = (source: PinSource) => call<PinInfo>("pin_open", { source });
/** The pin this window shows. */
export const pin_info = () => call<PinInfo>("pin_info");
/** This pin's image as bytes, for a blob URL. */
export const pin_image = () => call<ArrayBuffer>("pin_image");
/** This pin window's frame in global points. */
export const pin_frame = () => call<LogicalRect>("pin_frame");
export const pin_set_frame = (frame: LogicalRect) => call<void>("pin_set_frame", { frame });
export const pin_set_locked = (locked: boolean) => call<void>("pin_set_locked", { locked });
export const pin_set_all_spaces = (all: boolean) => call<void>("pin_set_all_spaces", { all });
export const pin_copy = () => call<void>("pin_copy");
/** Saves the image into the save folder and returns the new file's path. */
export const pin_save = () => call<string>("pin_save");
/** Opens the pin's history entry in the editor; a pin without `entryId` answers `not_implemented`. */
export const pin_edit = () => call<void>("pin_edit");
export const pin_close = () => call<void>("pin_close");
export const pin_close_all = () => call<void>("pin_close_all");
