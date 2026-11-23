// Rendered output and the editor's image (src-tauri/src/commands/render.rs). Owner: annotation.
//
// A rendered image crosses the bridge as the raw body of the call, never as base64 or JSON
// numbers; the header `x-glimpsfy-render` says what the bytes are. The editor's image comes back
// the same way, as an `ArrayBuffer`.

import { invoke } from "@tauri-apps/api/core";
import { ApiError, call, type ErrorCode } from "./core";
import type { CaptureId, ColorSpace, DisplayId } from "./capture";
import type { OutputDone } from "./output";
import type { Sent } from "./send";
import type { ProviderId, UploadStarted } from "./upload";

/** To the editor window: a new image is open. Payload: `EditorImage`. */
export const EDITOR_OPENED = "editor://opened";

export type RenderSource = { kind: "capture"; captureId: CaptureId; displayId: DisplayId } | { kind: "editor"; token: string };

/** A rendered image: RGBA rows of `width × height` in `colorSpace`, which must be the source's. */
export type RenderedImage = {
  source: RenderSource;
  width: number;
  height: number;
  colorSpace: ColorSpace;
  data: Uint8Array | Uint8ClampedArray;
  /** The part of the source the pixels show, in its pixels (`renderExport` gives it). From the
   * overlay it is the selection's crop of the frame, which Repeat Last Area captures again. */
  area?: { x: number; y: number; width: number; height: number } | null;
};

export type EditorImage = {
  token: string;
  name: string;
  path: string | null;
  /** `rgba`: `editor_image` answers raw pixels; `file`: the file's own bytes, to decode. */
  encoding: "rgba" | "file";
  width: number | null;
  height: number | null;
  colorSpace: ColorSpace;
  scale: number;
  /** The annotation document (JSON) to draw over the image: the overlay's, or a reopened edit's. */
  document: string | null;
  /** The history entry the image is, when the history opened it. */
  entryId: string | null;
};

async function rendered<T>(command: string, image: RenderedImage, providerId: ProviderId | null = null): Promise<T> {
  const header = {
    source: image.source,
    width: image.width,
    height: image.height,
    colorSpace: image.colorSpace,
    providerId,
    area: image.area ?? null,
  };
  // A body that is a view of a larger buffer would send the whole buffer.
  const view = image.data;
  const body = view.byteOffset === 0 && view.byteLength === view.buffer.byteLength ? new Uint8Array(view.buffer) : new Uint8Array(view);
  try {
    return await invoke<T>(command, body, { headers: { "x-glimpsfy-render": encodeURIComponent(JSON.stringify(header)) } });
  } catch (error) {
    if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
      const e = error as { code: ErrorCode; message: string };
      throw new ApiError(e.code, e.message);
    }
    throw new ApiError("internal", String(error));
  }
}

/** Puts a rendered image on the clipboard; from the overlay it also closes the capture. */
export const output_copy_rendered = (image: RenderedImage) => rendered<OutputDone>("output_copy_rendered", image);
/** Saves a rendered image as a new file named by the settings; from the overlay it also closes the capture. */
export const output_save_rendered = (image: RenderedImage) => rendered<OutputDone>("output_save_rendered", image);
/** Asks where with the save panel and saves there; a cancelled panel throws `cancelled`. */
export const output_save_as_rendered = (image: RenderedImage) => rendered<OutputDone>("output_save_as_rendered", image);
/** Writes a rendered image to a PNG in the cache and returns its path, for `pin_open` and `upload_file`. */
export const output_stage_rendered = (image: RenderedImage) => rendered<string>("output_stage_rendered", image);
/** Uploads a rendered image as PNG to `providerId` (or the default provider) once the upload area
 * admits it; from the overlay it then closes the capture. Check first with `prepareUpload`. */
export const upload_capture_rendered = (image: RenderedImage, providerId: ProviderId | null) =>
  rendered<UploadStarted>("upload_capture_rendered", image, providerId);
/** Sends the overlay's rendering to the target whose bundle identifier is `sendTarget`, or copies
 * its file's path when it is `null`; the capture closes. */
export const send_rendered = (image: RenderedImage, sendTarget: string | null) =>
  rendered<Sent>("send_rendered", image, null, { sendTarget });

/** Opens an image file in the editor window (a history entry gives its id). */
export const editor_open = (path: string, entryId: string | null = null) => call<EditorImage>("editor_open", { path, entryId });
/** Hands the overlay's capture and its annotation document (JSON) to the editor, and closes the capture. */
export const editor_open_capture = (captureId: CaptureId, displayId: DisplayId, document: string) =>
  call<EditorImage>("editor_open_capture", { captureId, displayId, document });
/** The image the editor shows, if any. */
export const editor_current = () => call<EditorImage | null>("editor_current");
/** The editor image's bytes: RGBA rows or the file's own bytes (see `EditorImage.encoding`). */
export const editor_image = (token: string) => call<ArrayBuffer>("editor_image", { token });
/** Forgets the editor's image when its window closes. */
export const editor_close = (token: string) => call<void>("editor_close", { token });
/** Keeps the page's annotation document (JSON) with the editor's image, so the next Copy, Save
 * or Save as stores it with the history entry and the drawings stay editable. */
export const editor_set_document = (token: string, document: string | null) =>
  call<void>("editor_set_document", { token, document });
