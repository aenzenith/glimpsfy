// Subject lifting: the editor's Remove background and the overlay's Copy subject
// (src-tauri/src/commands/subject.rs). Owner: annotation. macOS only: elsewhere
// `capabilities().subjectLift` is false and the pages show neither action.
//
// Pixels cross the bridge as the raw body of the call, never as base64: `subject_find` sends the
// editor image's own pixels (header `x-glimpsfy-subject`) and answers one byte per pixel, the
// number of the subject that covers it from 1, or 0 for the background. `subject_cut` answers the
// cut image's RGBA rows. An empty answer means "no subject" or "the original again".

import { invoke } from "@tauri-apps/api/core";

import type { ColorSpace, Selection } from "./capture";
import { ApiError, call, type ErrorCode } from "./core";
import type { RenderedImage } from "./render";

/** The editor image's pixels as the page shows them, before any drawing. */
export type SubjectPixels = {
  /** The `EditorImage.token` the pixels are. */
  token: string;
  width: number;
  height: number;
  colorSpace: ColorSpace;
  data: Uint8Array | Uint8ClampedArray;
};

/** What Copy subject did: 0 means the selection has no subject, nothing was copied and the
 * capture is still open. */
export type SubjectCopy = { subjects: number };

function wrap(error: unknown): ApiError {
  if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
    const e = error as { code: ErrorCode; message: string };
    return new ApiError(e.code, e.message);
  }
  return new ApiError("internal", String(error));
}

/** A body that is a view of a larger buffer would send the whole buffer. */
function bodyOf(view: Uint8Array | Uint8ClampedArray): Uint8Array {
  return view.byteOffset === 0 && view.byteLength === view.buffer.byteLength ? new Uint8Array(view.buffer as ArrayBuffer) : new Uint8Array(view);
}

/** Finds the subjects of the editor image and answers their labels, one byte per pixel; an empty
 * buffer when it has none. A second call for the same image answers at once. */
export async function subject_find(pixels: SubjectPixels, job: string | null = null): Promise<ArrayBuffer> {
  const header = { token: pixels.token, width: pixels.width, height: pixels.height, colorSpace: pixels.colorSpace, job };
  try {
    return await invoke<ArrayBuffer>("subject_find", bodyOf(pixels.data), {
      headers: { "x-glimpsfy-subject": encodeURIComponent(JSON.stringify(header)) },
    });
  } catch (error) {
    throw wrap(error);
  }
}

/** Makes the editor image without its background its base, keeping the subjects of `keep`, and
 * answers the cut image's RGBA rows; `null` makes the original the base again (empty answer). */
export const subject_cut = (token: string, keep: number[] | null) => call<ArrayBuffer>("subject_cut", { token, keep });

/** Copies every subject of a selection without drawings as a transparent PNG; the capture closes
 * unless the selection has no subject. */
export const subject_copy = (selection: Selection, job: string | null = null) => call<SubjectCopy>("subject_copy", { selection, job });

/** Copy subject for a selection with drawings: the masks come from the frozen frame under the
 * drawings and are laid over the rendered pixels. `image.area` must be set. */
export async function subject_copy_rendered(image: RenderedImage, job: string | null = null): Promise<SubjectCopy> {
  const header = {
    source: image.source,
    width: image.width,
    height: image.height,
    colorSpace: image.colorSpace,
    providerId: null,
    area: image.area ?? null,
  };
  const headers: Record<string, string> = { "x-glimpsfy-render": encodeURIComponent(JSON.stringify(header)) };
  if (job) headers["x-glimpsfy-job"] = job;
  try {
    return await invoke<SubjectCopy>("subject_copy_rendered", bodyOf(image.data), { headers });
  } catch (error) {
    throw wrap(error);
  }
}

/** Stops the search started under `job`. */
export const subject_cancel = (job: string) => call<void>("subject_cancel", { job });
