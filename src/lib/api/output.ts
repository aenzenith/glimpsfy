// Output: copy, save and reveal a capture. Mirrors src-tauri/src/commands/output.rs.

import type { CaptureId, CaptureMode, ColorSpace, Selection } from "./capture";
import { call, type ErrorCode } from "./core";
import type { ImageFormat } from "../settings";

/** With `output://copied` and `output://saved`, and as the commands' result. */
export type OutputDone = {
  captureId: CaptureId;
  /** The image's size in pixels. */
  width: number;
  height: number;
  /** The encoded file's size. */
  bytes: number;
  /** The saved file; `null` for a copy. */
  path: string | null;
  /** The file format; a copy is always PNG. */
  format: "png" | "jpeg" | "webp";
  /** The save folder of the settings when it could not be reached or written, so the file went
   * to the default folder (Pictures › Glimpsfy) instead. */
  fallbackFrom: string | null;
};

/** The payload of `output://handoff`: one delivered image, for the history. */
export type CaptureHandoff = {
  kind: "screenshot";
  /** 0 for an image from the editor. */
  captureId: CaptureId;
  mode: CaptureMode;
  copied: boolean;
  savedPath: string | null;
  /** A PNG of a clipboard-only capture in the app data folder, while the history is on. */
  imagePath: string | null;
  format: "png" | "jpeg" | "webp";
  width: number;
  height: number;
  scale: number;
  bytes: number;
  colorSpace: ColorSpace;
  appName: string | null;
  bundleId: string | null;
  windowTitle: string | null;
  displayId: number | null;
  displayName: string | null;
  createdAtMs: number;
};

/** One image of a capture left the app. Payload: `CaptureHandoff`. */
export const OUTPUT_HANDOFF = "output://handoff";

/** With `output://failed`. */
export type OutputFailed = { captureId: CaptureId; code: ErrorCode; message: string };

export const output_copy = (selection: Selection) => call<OutputDone>("output_copy", { selection });
export const output_save = (selection: Selection, format: ImageFormat | null = null) =>
  call<OutputDone>("output_save", { selection, format });
/** Saves under `path`, or asks with the system's save panel when it is left out; a cancelled
 * panel rejects with `cancelled` and the overlay comes back. */
export const output_save_as = (selection: Selection, path?: string) =>
  call<OutputDone>("output_save_as", { selection, path: path ?? null });
export const output_reveal = (path: string) => call<void>("output_reveal", { path });
