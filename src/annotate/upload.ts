// Upload (⌘U) from the overlay and the editor, in the order src/upload/flow.ts asks for: check
// the provider, ask the first-upload question when the destination is not confirmed yet
// (`FirstUploadDialog.vue`, which the page shows while `asking` is set), then start. Without a
// usable provider, Settings opens at its Upload section. The page decides what is sent: the selection as it is, or the
// rendered pixels with the drawings.

import { shallowRef } from "vue";

import { ApiError } from "../lib/api/core";
import type { ConsentInfo, ProviderId } from "../lib/api/upload";
import { window_open_settings } from "../lib/api/windows";
import { t } from "../lib/i18n";
import type { DialogFile } from "../upload/FirstUploadDialog.vue";
import { prepareUpload } from "../upload/flow";
import { issueText } from "../upload/text";
import { context2d, scratchCanvas } from "./compose";
import type { PixelRect } from "./redact";

/** The longest side of the dialog's thumbnail, in pixels. */
const THUMB = 160;

/** A small PNG data URL of `area` of `source`, for the dialog's file row. It stays in the page. */
export function thumbnail(source: CanvasImageSource, area: PixelRect): string | null {
  if (area.width <= 0 || area.height <= 0 || typeof document === "undefined") return null;
  const k = Math.min(1, THUMB / Math.max(area.width, area.height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(area.width * k));
  canvas.height = Math.max(1, Math.round(area.height * k));
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(source, area.x, area.y, area.width, area.height, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL("image/png");
}

/** The same for rendered RGBA rows. */
export function thumbnailOfPixels(image: { width: number; height: number; colorSpace: PredefinedColorSpace; data: Uint8Array | Uint8ClampedArray }): string | null {
  if (image.width <= 0 || image.height <= 0) return null;
  const canvas = scratchCanvas(image.width, image.height);
  // Rendered rows never live in shared memory.
  const data = new Uint8ClampedArray(image.data.buffer as ArrayBuffer, image.data.byteOffset, image.data.byteLength);
  context2d(canvas, image.colorSpace).putImageData(new ImageData(data, image.width, image.height, { colorSpace: image.colorSpace }), 0, 0);
  return thumbnail(canvas, { x: 0, y: 0, width: image.width, height: image.height });
}

export type UploadFlow = ReturnType<typeof uploadFlow>;

/**
 * One page's Upload. `send` starts the upload to the checked provider, `file` describes what goes
 * for the dialog, and `say` shows a sentence to the user.
 */
export function uploadFlow(options: {
  send: (provider: ProviderId) => Promise<unknown>;
  file: () => DialogFile | null;
  say: (text: string) => void;
}) {
  /** The first-upload question while it is open. */
  const asking = shallowRef<{ consent: ConsentInfo; file: DialogFile | null } | null>(null);
  let running = false;

  const fail = (error: unknown) => {
    const code = error instanceof ApiError ? error.code : "internal";
    const known = ["permission_denied", "not_found", "io", "not_implemented", "not_confirmed", "keychain"];
    options.say(t(known.includes(code) ? (`error.${code}` as "error.other") : "error.other"));
  };

  async function send(provider: ProviderId): Promise<void> {
    try {
      await options.send(provider);
    } catch (error) {
      fail(error);
    }
  }

  /** ⌘U: checks, asks when it must, and starts. */
  async function start(): Promise<void> {
    if (running || asking.value) return;
    running = true;
    try {
      const ready = await prepareUpload(null);
      switch (ready.kind) {
        case "noProvider":
          options.say(t("annotate.upload.noProvider"));
          await window_open_settings("upload").catch(() => undefined);
          break;
        case "blocked":
          options.say(t("annotate.upload.fix", { problem: issueText(ready.issues[0]) }));
          await window_open_settings("upload").catch(() => undefined);
          break;
        case "needsConsent":
          asking.value = { consent: ready.preflight.consent, file: options.file() };
          break;
        case "ready":
          await send(ready.preflight.provider);
          break;
      }
    } catch (error) {
      fail(error);
    } finally {
      running = false;
    }
  }

  return {
    asking,
    start,
    /** The dialog recorded the confirmation: the upload starts. */
    confirmed(consent: ConsentInfo): void {
      asking.value = null;
      void send(consent.provider);
    },
    cancel(): void {
      asking.value = null;
    },
    /** The settings point elsewhere by now: the check runs again. */
    stale(): void {
      asking.value = null;
      void start();
    },
    anotherHost(): void {
      asking.value = null;
      void window_open_settings("upload").catch(() => undefined);
    },
  };
}
