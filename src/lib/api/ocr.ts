// Text recognition: "Copy text", the selectable text layer, QR codes and the PP-OCR models.
// Mirrors src-tauri/src/commands/ocr.rs and crates/ocr/src/lib.rs.
//
// Every box is in pixels of the image that was read (`OcrResult.imageSize`): the selection's crop
// of the frozen frame, the file, or the pixels the page sent. To place one over the overlay,
// divide it by `imageSize.width / selection.rect.width` (and the same for the height) and add the
// selection's origin. Pixels cross the bridge as the raw body of the call, never as base64.

import { invoke } from "@tauri-apps/api/core";

import type { CaptureId, ColorSpace, DisplayId, Selection } from "./capture";
import { ApiError, call, type ErrorCode } from "./core";

/** A model download made progress. Payload: `ModelProgress`. */
export const OCR_MODEL_PROGRESS = "ocr://model-progress";

/** A BCP 47 tag such as "tr-TR"; name it for the user with `Intl.DisplayNames`. */
export type LanguageTag = string;

export type EngineId = "vision" | "windowsMediaOcr" | "ppOcr";

export type Bounds = { x: number; y: number; width: number; height: number };

export type OcrWord = { text: string; bounds: Bounds; confidence: number };

/** What kind of text a block holds: running text, monospaced code or a terminal, or table rows. */
export type BlockKind = "prose" | "code" | "table";

export type OcrLine = {
  /** The line as it appears in `OcrResult.text`, indentation included; table cells are separated by tabs. */
  text: string;
  bounds: Bounds;
  /** The confidence runs from 0 to 1. */
  confidence: number;
  words: OcrWord[];
  /** The paragraph; lines come in reading order, and a new number starts a new paragraph. */
  block: number;
  kind: BlockKind;
};

export type OcrResult = {
  engine: EngineId;
  /** The whole text in reading order, as "Copy text" puts it on the clipboard. */
  text: string;
  lines: OcrLine[];
  language: LanguageTag | null;
  imageSize: { width: number; height: number };
};

export type LanguageInfo = {
  tag: LanguageTag;
  available: boolean;
  /** The size of the download that makes the language available, when one is needed. */
  downloadBytes: number | null;
};

export type Symbology =
  | "qr"
  | "microQr"
  | "aztec"
  | "dataMatrix"
  | "pdf417"
  | "ean8"
  | "ean13"
  | "upcE"
  | "code39"
  | "code93"
  | "code128"
  | "itf"
  | "codabar"
  | "other";

export type CodeResult = {
  symbology: Symbology;
  payload: string;
  bounds: Bounds;
  /** The link is set only for http and https URLs, which the chip under the code offers to open. */
  url: string | null;
};

/** What a piece of recognised text is. The last five are smart redaction's (D28). */
export type EntityKind = "phone" | "email" | "link" | "date" | "card" | "iban" | "apiKey" | "token" | "urlPassword";

/** The part of one line a piece covers, in pixels of the recognised image. */
export type EntityBox = { line: number; bounds: Bounds };

/** When a found date takes place, in local time without a zone ("2026-11-27T14:30:00"). */
export type EntityDate = {
  /** Midnight of the day for an all-day date. */
  start: string;
  /** The end of a timed date (an hour after the start when the text names none), or the last day of
   * an all-day date over several days; `null` for an all-day date of one day. */
  end: string | null;
  allDay: boolean;
  /** The text names no end, so the event was given one hour. */
  endAssumed: boolean;
};

/** A piece of recognised text the layer underlines, in reading order. */
export type Entity = {
  kind: EntityKind;
  /** The piece as the text spells it. */
  text: string;
  /** What an action uses: a number as + and digits, an address, a link with its scheme, a date's text. */
  value: string;
  boxes: EntityBox[];
  date: EntityDate | null;
};

export type CopyTextResult = {
  /** An empty text means nothing was recognised, and the clipboard was left as it was. */
  text: string;
  lineCount: number;
  language: LanguageTag | null;
};

/** Where the pixels to read come from: a selection of an open capture, or an image file. */
export type OcrSource = { kind: "selection"; selection: Selection } | { kind: "file"; path: string };

/** RGBA pixels in tight rows, as the text layer's context gives them. */
export type OcrPixels = {
  width: number;
  height: number;
  data: Uint8Array | Uint8ClampedArray;
  colorSpace: ColorSpace;
  /** The capture the pixels belong to, so that closing it cancels the recognition. */
  captureId?: CaptureId | null;
};

/** A PP-OCR model of the reviewed table. */
export type ModelInfo = {
  id: string;
  /** A product name, shown as it is. */
  title: string;
  languages: LanguageTag[];
  /** The download's size in bytes. */
  bytes: number;
  installed: boolean;
};

export type ModelProgress = { model: string; received: number; total: number };

/** A selection on its own reads as `{ kind: "selection", selection }`. */
function sourceOf(source: OcrSource | Selection): OcrSource {
  return "kind" in source ? source : { kind: "selection", selection: source };
}

async function withPixels<T>(command: string, pixels: OcrPixels, job: string | null): Promise<T> {
  const header = { width: pixels.width, height: pixels.height, colorSpace: pixels.colorSpace, captureId: pixels.captureId ?? null, job };
  // A body that is a view of a larger buffer would send the whole buffer.
  const view = pixels.data;
  const body =
    view.byteOffset === 0 && view.byteLength === view.buffer.byteLength ? new Uint8Array(view.buffer as ArrayBuffer) : new Uint8Array(view);
  try {
    return await invoke<T>(command, body, { headers: { "x-glimpsfy-ocr": encodeURIComponent(JSON.stringify(header)) } });
  } catch (error) {
    if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
      const e = error as { code: ErrorCode; message: string };
      throw new ApiError(e.code, e.message);
    }
    throw new ApiError("internal", String(error));
  }
}

/** Reads the text of a selection or a file with the engine and languages of the settings. */
export function ocr_recognize(source: OcrSource | Selection, job: string | null = null): Promise<OcrResult> {
  return call("ocr_recognize", { source: sourceOf(source), job });
}

/** Reads the text of pixels from the page, with every redaction already flattened in. */
export function ocr_recognize_pixels(pixels: OcrPixels, job: string | null = null): Promise<OcrResult> {
  return withPixels("ocr_recognize_pixels", pixels, job);
}

/** Reads the text of a selection or a file and copies it, with the "copied N lines" toast. The capture stays open. */
export function ocr_copy_text(source: OcrSource | Selection, job: string | null = null): Promise<CopyTextResult> {
  return call("ocr_copy_text", { source: sourceOf(source), job });
}

/** Copies text chosen in the text layer, with the "copied N lines" toast on `displayId`'s screen. */
export function ocr_copy(text: string, language: LanguageTag | null = null, displayId: DisplayId | null = null): Promise<CopyTextResult> {
  return call("ocr_copy", { text, language, displayId });
}

/** Finds QR codes and barcodes; an empty list when the settings turn code detection off. */
export function ocr_detect_codes(source: OcrSource | Selection, job: string | null = null): Promise<CodeResult[]> {
  return call("ocr_detect_codes", { source: sourceOf(source), job });
}

/** Finds QR codes and barcodes in pixels from the page. */
export function ocr_detect_codes_pixels(pixels: OcrPixels, job: string | null = null): Promise<CodeResult[]> {
  return withPixels("ocr_detect_codes_pixels", pixels, job);
}

/** Finds the phone numbers, email addresses, links and dates of `kinds` in a recognised text, on the machine. */
export function ocr_detect_entities(result: OcrResult, kinds: EntityKind[]): Promise<Entity[]> {
  return call("ocr_detect_entities", { result, kinds });
}

/** Stops the recognition, code search or model download started under `job`. */
export function ocr_cancel(job: string): Promise<void> {
  return call("ocr_cancel", { job });
}

/** The languages the chosen engine reads on this machine. */
/** The engines of this platform and build, and those that read text now. */
export type OcrEngines = { available: OcrEngineId[]; ready: OcrEngineId[] };
export type OcrEngineId = "vision" | "windowsMediaOcr" | "ppOcr";
export function ocr_engines(): Promise<OcrEngines> {
  return call("ocr_engines");
}

export function ocr_languages(): Promise<LanguageInfo[]> {
  return call("ocr_languages");
}

/** The PP-OCR models and whether each is downloaded. */
export function ocr_models(): Promise<ModelInfo[]> {
  return call("ocr_models");
}

/** Downloads a model after the user asked for it; progress arrives as `ocr://model-progress`. */
export function ocr_model_download(id: string, job: string | null = null): Promise<ModelInfo> {
  return call("ocr_model_download", { id, job });
}

/** Deletes a downloaded model. */
export function ocr_model_remove(id: string): Promise<void> {
  return call("ocr_model_remove", { id });
}
