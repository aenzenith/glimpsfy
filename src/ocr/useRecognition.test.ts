// A redaction must reach the text layer: once a blurred password is copied as text it cannot be
// taken back. The layer keeps the last recognition and skips a run over the same pixels, so these
// tests check that the cache tells pixels apart down to one changed pixel, and that the text of
// the old pixels is gone the moment a run over new ones starts, before the engine answers.
//
// The engine is a stand-in that "reads" the password only while its pixel is still black.

import { effectScope } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { OcrPixels, OcrResult } from "../lib/api/ocr";
import { useRecognition } from "./useRecognition";

const W = 640;
const H = 400;
/** The green byte of one pixel of the password's text, between the bytes a sampling hash read. */
const SPOT = (217 * W + 333) * 4 + 1;

const reads = vi.hoisted(() => ({ count: 0 }));

vi.mock("../lib/api/ocr", () => ({
  ocr_recognize_pixels: async (pixels: OcrPixels): Promise<OcrResult> => {
    reads.count += 1;
    const text = pixels.data[SPOT] === 0 ? "hunter2" : "";
    return { engine: "vision", text, lines: [], language: null, imageSize: { width: pixels.width, height: pixels.height } };
  },
  ocr_detect_codes_pixels: async () => [],
  ocr_cancel: async () => undefined,
}));

function image(): OcrPixels {
  const data = new Uint8ClampedArray(W * H * 4).fill(255);
  data[SPOT - 1] = data[SPOT] = data[SPOT + 1] = 0;
  return { width: W, height: H, data, colorSpace: "srgb" };
}

/** The same image with the password's pixel blurred to grey, and nothing else changed. */
function blurred(): OcrPixels {
  const pixels = image();
  pixels.data[SPOT - 1] = pixels.data[SPOT] = pixels.data[SPOT + 1] = 190;
  return pixels;
}

describe("the text layer's recognition", () => {
  afterEach(() => {
    reads.count = 0;
  });

  it("reads again after a redaction and never serves the text from before it", async () => {
    const scope = effectScope();
    const recognition = scope.run(useRecognition)!;

    await recognition.run(image());
    expect(recognition.result.value?.text).toBe("hunter2");
    // The same pixels are not read twice.
    await recognition.run(image());
    expect(reads.count).toBe(1);

    const again = recognition.run(blurred());
    expect(recognition.result.value).toBeNull();
    await again;
    expect(reads.count).toBe(2);
    expect(recognition.status.value).toBe("done");
    expect(recognition.result.value?.text).toBe("");

    scope.stop();
  });
});
