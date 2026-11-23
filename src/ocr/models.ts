// Whether text recognition failed because the PP-OCR models it reads with are not downloaded yet.
// The core answers a missing model with `not_found`, as it answers other missing things, so a page
// that saw that failure asks which engine reads and which models are on disk before it says so
// and offers Settings › Text recognition instead of the bare error.

import { ApiError } from "../lib/api/core";
import { ocr_engines, ocr_models } from "../lib/api/ocr";
import { settings_get } from "../lib/api/settings";

/** Whether `error`, from a recognition, means the PP-OCR models are missing. */
export async function modelsMissing(error: unknown): Promise<boolean> {
  const code = error instanceof ApiError ? error.code : null;
  if (code !== "not_found" && code !== "not_implemented") return false;
  return ppOcrWithoutModels();
}

/** Whether PP-OCR reads the text here (chosen, or the only engine, as on Linux) and a model it
 * needs is not on disk. */
export async function ppOcrWithoutModels(): Promise<boolean> {
  try {
    const [settings, engines, models] = await Promise.all([settings_get(), ocr_engines(), ocr_models()]);
    const system = engines.available.some((id) => id !== "ppOcr");
    const ppOcr = settings.ocr.engine === "ppOcr" || !system;
    return ppOcr && models.some((model) => !model.installed);
  } catch {
    return false;
  }
}
