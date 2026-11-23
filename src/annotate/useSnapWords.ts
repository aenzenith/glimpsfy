// Reads the text of the area quietly while a tool that snaps to words is chosen, and hands the
// words to the engine (`snap.ts`). It reads through the page's shared recognition, so the text
// layer (⇧⌘C) shows the same reading without a second one, and it tells the strip when the text
// could not be read, so the tool's free drawing has a reason.
//
// The reading starts the moment a snapping tool is chosen and again once the area has rested for
// 120 ms. The same area is not read again: a blur drawn over a word leaves the boxes where they
// were, and the text layer reads the flattened pixels itself when it shows.

import { computed, onScopeDispose, ref, watch, type Ref } from "vue";

import type { AnnotationEngine } from "./engine";
import type { PixelRect } from "./redact";
import { snaps, snapWords } from "./snap";
import type { TextLayerContext } from "./textLayer";

const areaKey = (r: PixelRect | null | undefined) => (r ? `${r.x},${r.y},${r.width},${r.height}` : "");

/** `ready` is false while the area is being drawn or moved (the overlay's drag). */
export function useSnapWords(engine: AnnotationEngine, context: TextLayerContext, ready: Readonly<Ref<boolean>>) {
  const { recognition } = context;
  /** The text could not be read (no engine, an error, no PP-OCR models): the tools draw freely. */
  const problem = ref(false);
  /** The area the current or last reading took its pixels from; its boxes are placed in it. */
  let readArea: PixelRect | null = null;
  let pending = 0;

  const snapping = computed(() => (engine.rev.value, snaps(engine.tool)));

  // A reading started, here or in the text layer; it takes the area's pixels in the same tick.
  watch(
    recognition.status,
    (status) => {
      if (status === "reading") readArea = context.area.value ? { ...context.area.value } : null;
    },
    { flush: "sync" },
  );

  // A new reading leaves the words of the last one in place until it ends: they still sit where
  // the image shows them.
  watch([recognition.status, recognition.result], ([status, result]) => {
    if (status === "done" && result && readArea) {
      engine.setWords(snapWords(result, readArea));
      problem.value = false;
    } else if (status === "failed") {
      engine.setWords(null);
      problem.value = true;
    }
  });

  function read(): void {
    const area = context.area.value;
    if (!area) return;
    const status = recognition.status.value;
    if ((status === "reading" || status === "done") && areaKey(readArea) === areaKey(area)) return;
    const pixels = context.pixels();
    if (!pixels) return;
    void recognition.run({ ...pixels, colorSpace: context.colorSpace.value === "display-p3" ? "display-p3" : "srgb" });
  }

  watch(
    [snapping, () => areaKey(context.area.value), ready],
    ([on, , settled], before) => {
      clearTimeout(pending);
      if (!on || !settled) return;
      // A tool just chosen reads at once; an area that changed settles first.
      if (!before?.[0]) read();
      else pending = window.setTimeout(read, 120);
    },
    { immediate: true },
  );

  /** A new capture or image: its words are not read yet. */
  function reset(): void {
    clearTimeout(pending);
    recognition.reset();
    readArea = null;
    problem.value = false;
    engine.setWords(null);
  }

  onScopeDispose(() => clearTimeout(pending));

  return { problem, reset };
}
