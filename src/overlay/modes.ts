// What the overlay does once a selection is finished, by the capture's mode and Settings ›
// General › After a selection: stay with the bars for annotation, copy and close, save and close,
// copy the text in it ("Copy text from area"), hand it to an interval series as its area, hand it
// to the agent that asked the user to select it, or scroll through it (the scrolling capture). A
// clicked window is first captured on its own, so Copy and Save deliver the window with its real
// transparency instead of the crop.
// Owner: capture (milestone 1); the page wires it in `App.vue`.

import { agents_take_selection } from "../lib/api/agents";
import {
  capture_close,
  capture_interval_area,
  capture_pick_window,
  overlay_failed,
  type OverlayFrame,
  type PickedWindow,
  type Selection,
} from "../lib/api/capture";
import { ocr_copy_text } from "../lib/api/ocr";
import { output_copy, output_save } from "../lib/api/output";

/** `stay`: the bars show and the user goes on; `done`: the capture closed; `empty`: copy text
 * found nothing, so the selection stays for another try and the page says so; `scroll`: the page
 * goes straight into the scrolling capture of the selection. */
export type Finish = "stay" | "done" | "empty" | "scroll";

/**
 * Runs the mode's action on a finished selection. `windowId` is the window the user clicked, if
 * the selection is a window. A failure is thrown, and the page shows it and keeps the selection.
 */
export async function finish(
  frame: OverlayFrame,
  selection: Selection,
  windowId: number | null,
  hooks: {
    /** The beautify changes sent so far (F during the drag); a copy or save waits for them. */
    settled?: () => Promise<void>;
    /** The window's own image, whose size the beautify panel shows. */
    picked?: (window: PickedWindow) => void;
  } = {},
): Promise<Finish> {
  const settled = hooks.settled ?? (() => Promise.resolve());
  if (windowId !== null) {
    try {
      hooks.picked?.(await capture_pick_window(frame.captureId, selection.displayId, windowId));
    } catch (error) {
      // Without the window's own image, the crop of the frozen frame is delivered instead.
      console.warn("[glimpsfy] the window could not be captured on its own", error);
    }
  }
  switch (frame.afterSelection) {
    case "annotate":
      return "stay";
    case "copy":
      await settled();
      await output_copy(selection);
      return "done";
    case "save":
      await settled();
      await output_save(selection);
      return "done";
    case "copyText": {
      const copied = await ocr_copy_text(selection);
      if (!copied.text.trim()) return "empty";
      await capture_close(frame.captureId);
      return "done";
    }
    case "interval":
      // The core closes the capture and starts the series on this area.
      await capture_interval_area(selection);
      return "done";
    case "agent":
      // An agent asked for an area the user selects; the core sends what lies in it and closes
      // the capture.
      await agents_take_selection(selection);
      return "done";
    case "scroll":
      return "scroll";
  }
}

/** Tells the core this page could not show its frame, so it reloads the page or gives up at once
 * instead of after the 5-second timeout. */
export async function reportFailure(frame: OverlayFrame, displayId: number, error: unknown): Promise<void> {
  const message = error instanceof Error ? error.message : String(error);
  try {
    await overlay_failed(frame.captureId, displayId, message);
  } catch {
    await capture_close(frame.captureId);
  }
}
