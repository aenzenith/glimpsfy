// Runs text recognition and code detection on pixels and holds the state the text layer shows:
// the result, the codes, whether it is reading, and what went wrong. A new run cancels the one
// before it, the same pixels are not read twice, and leaving the component cancels a run that
// is still going.

import { onScopeDispose, ref, shallowRef } from "vue";

import { ApiError } from "../lib/api/core";
import { ocr_cancel, ocr_detect_codes_pixels, ocr_recognize_pixels, type CodeResult, type OcrPixels, type OcrResult } from "../lib/api/ocr";
import { fingerprint, jobName } from "./text";

export type RecognitionStatus = "idle" | "reading" | "done" | "failed";

/** Why a run failed: the engine is not there (`unavailable`) or it failed (`failed`). */
export type RecognitionProblem = "unavailable" | "failed";

export function useRecognition() {
  const result = shallowRef<OcrResult | null>(null);
  const codes = shallowRef<CodeResult[]>([]);
  const status = ref<RecognitionStatus>("idle");
  const problem = ref<RecognitionProblem | null>(null);
  let job: string | null = null;
  let readKey = "";

  /** Reads `pixels`, unless the same pixels were read last time. */
  async function run(pixels: OcrPixels): Promise<void> {
    const key = fingerprint(pixels.width, pixels.height, pixels.data);
    if (key === readKey && (status.value === "done" || status.value === "reading")) return;
    cancel();
    const mine = jobName();
    job = mine;
    readKey = key;
    // The old result belongs to other pixels (perhaps from before a blur): showing or copying it
    // while the new ones are read would hand out text the image no longer shows.
    result.value = null;
    codes.value = [];
    status.value = "reading";
    problem.value = null;
    try {
      // Codes are a bonus: a failure there never hides the text.
      const [text, found] = await Promise.all([
        ocr_recognize_pixels(pixels, mine),
        ocr_detect_codes_pixels(pixels, `${mine}:codes`).catch(() => [] as CodeResult[]),
      ]);
      if (job !== mine) return;
      result.value = text;
      codes.value = found;
      status.value = "done";
    } catch (error) {
      if (job !== mine) return;
      readKey = "";
      const code = error instanceof ApiError ? error.code : "internal";
      if (code === "cancelled") {
        status.value = "idle";
        return;
      }
      console.warn("[glimpsfy] text recognition failed", error);
      problem.value = code === "not_implemented" || code === "not_found" ? "unavailable" : "failed";
      status.value = "failed";
    } finally {
      if (job === mine) job = null;
    }
  }

  /** Stops the run that is going, if any; its answer is ignored. */
  function cancel(): void {
    if (!job) return;
    const stopping = job;
    job = null;
    void ocr_cancel(stopping).catch(() => undefined);
    void ocr_cancel(`${stopping}:codes`).catch(() => undefined);
    if (status.value === "reading") status.value = "idle";
  }

  /** Forgets the result, so the next run reads again. */
  function reset(): void {
    cancel();
    readKey = "";
    result.value = null;
    codes.value = [];
    status.value = "idle";
    problem.value = null;
  }

  onScopeDispose(cancel);

  return { result, codes, status, problem, run, cancel, reset };
}
