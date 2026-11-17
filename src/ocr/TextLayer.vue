<script setup lang="ts">
// The text recognition layer over a capture (overlay ab7) or an image (editor ab9): a veil over
// the image, an outline around every recognised line, the recognised words as real, invisible,
// selectable text placed over the pixels, a chip that copies all of it, and a chip under each QR
// code. Selecting with the mouse and pressing ⌘C copies exactly the selected text with its
// indentation and table tabs; ⇧⌘C (or the chip) copies everything; ⌘A selects everything; Esc
// is left to the page, which hides the layer.
//
// Props, all optional:
// - `result`: a recognition to show. Without it, the layer reads on its own: it takes the pixels
//   of the surrounding text layer context (`TEXT_LAYER` in src/annotate/textLayer.ts), with every
//   blur and pixelation already flattened in, whenever it becomes visible or its area changes.
// - `codes`: the QR codes and barcodes of `result`, for their chips.
// - `imageSize`: the size in pixels of the image the boxes refer to; `result.imageSize` by default.
// - `rect`: where that image shows, in CSS pixels of the layer's box; by default the context's
//   area mapped through its view.
// - `visible`: whether the layer shows; the context's `visible`, or true without a context.
// - `displayId`: the display whose toast stack says "Copied"; the last capture's by default.
//
// It emits `hide` when its hide button is pressed (and sets the context's `visible` to false),
// and exposes `copySelection()`, `copyAll()` and `hasSelection()` to the page.

import { computed, inject, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";

import { TEXT_LAYER } from "../annotate/textLayer";
import GIcon from "../components/GIcon.vue";
import { ocr_copy, type CodeResult, type OcrResult } from "../lib/api/ocr";
import { app_open_url } from "../lib/api/windows";
import { language, t } from "../lib/i18n";
import { languageName, lineCount, placer, textOf, wordOffsets, type Rect, type WordRange } from "./text";
import { useRecognition } from "./useRecognition";

const props = withDefaults(
  defineProps<{
    result?: OcrResult | null;
    codes?: CodeResult[] | null;
    imageSize?: { width: number; height: number } | null;
    rect?: Rect | null;
    visible?: boolean | null;
    displayId?: number | null;
  }>(),
  { result: null, codes: null, imageSize: null, rect: null, visible: null, displayId: null },
);
const emit = defineEmits<{ hide: [] }>();

const context = inject(TEXT_LAYER, null);
const recognition = useRecognition();

/** The invisible words are drawn in this font, and measured in it to fit their boxes. */
const FONT = '-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif';
/** Line outlines get this much room around the text: 3 px across and 2 px up and down. */
const PAD = { x: 3, y: 2 };

const shown = computed(() => props.visible ?? context?.visible.value ?? true);
const own = computed(() => props.result === null && context !== null);
const current = computed<OcrResult | null>(() => props.result ?? (own.value ? recognition.result.value : null));
const codes = computed<CodeResult[]>(() => props.codes ?? (own.value ? recognition.codes.value : []));
const status = computed(() => (own.value ? recognition.status.value : current.value ? "done" : "idle"));

const rect = computed<Rect | null>(() => {
  if (props.rect) return props.rect;
  const area = context?.area.value;
  if (!area) return null;
  const { k, ox, oy } = context!.view.value;
  return { x: ox + area.x * k, y: oy + area.y * k, width: area.width * k, height: area.height * k };
});
const size = computed(() => props.imageSize ?? current.value?.imageSize ?? null);
const place = computed(() => (rect.value && size.value ? placer(rect.value, size.value) : null));

// ------------------------------------------------------------------------- reading on its own

let pending = 0;
watch(
  [shown, () => context?.area.value, own],
  ([visible]) => {
    clearTimeout(pending);
    if (!own.value) return;
    if (!visible) return recognition.cancel();
    // A selection that is still being adjusted settles first.
    pending = window.setTimeout(() => {
      const pixels = context?.pixels();
      if (!pixels || !context) return;
      void recognition.run({ ...pixels, colorSpace: context.colorSpace.value === "display-p3" ? "display-p3" : "srgb" });
    }, 120);
  },
  { immediate: true, deep: true },
);

// -------------------------------------------------------------------------------- geometry

const css = (r: Rect) => ({ left: `${r.x}px`, top: `${r.y}px`, width: `${r.width}px`, height: `${r.height}px` });

/** The words of a line in runs: a gap wider than the line is tall (a table's cell gap, a
 * sidebar's value) starts a new run, and each run gets its own outline. */
const runs = computed(() => {
  const p = place.value;
  if (!p || !current.value) return [];
  return current.value.lines.map((line) => {
    const lineBox = p(line.bounds);
    const boxes = line.words.map((word) => p(word.bounds));
    const groups: number[][] = [];
    boxes.forEach((box, i) => {
      const last = groups[groups.length - 1];
      const before = last ? boxes[last[last.length - 1]] : null;
      if (before && box.x - (before.x + before.width) <= lineBox.height * 1.2) last.push(i);
      else groups.push([i]);
    });
    return { lineBox, boxes, groups };
  });
});

const outlines = computed(() =>
  runs.value.flatMap(({ lineBox, boxes, groups }) => {
    if (!boxes.length) return [{ x: lineBox.x - PAD.x, y: lineBox.y - PAD.y, width: lineBox.width + 2 * PAD.x, height: lineBox.height + 2 * PAD.y }];
    return groups.map((group) => {
      const left = boxes[group[0]].x;
      const right = Math.max(...group.map((i) => boxes[i].x + boxes[i].width));
      return { x: left - PAD.x, y: lineBox.y - PAD.y, width: right - left + 2 * PAD.x, height: lineBox.height + 2 * PAD.y };
    });
  }),
);

let measure: CanvasRenderingContext2D | null = null;
function textWidth(text: string, px: number): number {
  measure ??= document.createElement("canvas").getContext("2d");
  if (!measure) return text.length * px * 0.5;
  measure.font = `${px}px ${FONT}`;
  return measure.measureText(text).width;
}

/** Every word as an absolutely placed span inside the box of the image, stretched to its box.
 * A word followed by another of its run also carries the space after it, stretched to reach the
 * next word, so a selection highlights the run without holes. */
const words = computed(() => {
  const r = rect.value;
  if (!r || !current.value) return [];
  return runs.value.flatMap(({ lineBox, boxes, groups }, l) => {
    const line = current.value!.lines[l];
    // One font size per line, from the line's height, so the selection highlight is even.
    const px = Math.max(4, lineBox.height * 0.8);
    const nextInRun = new Map<number, number>();
    for (const group of groups) group.slice(0, -1).forEach((i, n) => nextInRun.set(i, group[n + 1]));
    return line.words.map((word, w) => {
      const b = boxes[w];
      const next = nextInRun.get(w);
      const text = next === undefined ? word.text : `${word.text} `;
      const width = next === undefined ? b.width : Math.max(b.width, boxes[next].x - b.x);
      const natural = textWidth(text, px);
      const scale = natural > 0 ? width / natural : 1;
      return {
        key: `${l}:${w}`,
        l,
        w,
        text,
        style: {
          left: `${b.x - r.x}px`,
          top: `${lineBox.y - r.y}px`,
          height: `${lineBox.height}px`,
          lineHeight: `${lineBox.height}px`,
          fontSize: `${px}px`,
          transform: `scaleX(${Number.isFinite(scale) ? scale : 1})`,
        },
      };
    });
  });
});

const offsets = computed(() => (current.value ? wordOffsets(current.value) : []));

// ----------------------------------------------------------------------- the user's selection

const root = ref<HTMLElement | null>(null);
const textEl = ref<HTMLElement | null>(null);
const picked = shallowRef<WordRange | null>(null);
const selRects = shallowRef<Rect[]>([]);

function clearSelection(): void {
  picked.value = null;
  selRects.value = [];
}

/** Follows the page's selection: which words it covers, and the highlight and carets to draw. */
function onSelectionChange(): void {
  const host = textEl.value;
  const base = root.value?.getBoundingClientRect();
  const selection = document.getSelection();
  if (!host || !base || !selection || selection.rangeCount === 0 || selection.isCollapsed) return clearSelection();
  const range = selection.getRangeAt(0);
  if (!range.intersectsNode(host)) return clearSelection();
  const spans = [...host.querySelectorAll<HTMLElement>(".ocr-word")].filter((span) => range.intersectsNode(span));
  if (!spans.length) return clearSelection();
  const first = spans[0];
  const last = spans[spans.length - 1];
  const at = (span: HTMLElement): [number, number] => [Number(span.dataset.l), Number(span.dataset.w)];
  const startChar = range.startContainer.parentElement === first ? range.startOffset : 0;
  const endChar = range.endContainer.parentElement === last ? range.endOffset : (last.textContent ?? "").length;
  picked.value = { start: at(first), startChar, end: at(last), endChar };
  selRects.value = [...range.getClientRects()]
    .filter((q) => q.width >= 1 && q.height >= 1 && q.height < base.height)
    .map((q) => ({ x: q.left - base.left, y: q.top - base.top, width: q.width, height: q.height }));
}

const carets = computed(() => {
  const rs = selRects.value;
  if (!rs.length) return null;
  const f = rs[0];
  const l = rs[rs.length - 1];
  return {
    start: { x: f.x - 1, y: f.y, width: 2, height: f.height },
    end: { x: l.x + l.width - 1, y: l.y, width: 2, height: l.height },
  };
});

function selectionText(): string {
  return current.value && picked.value ? textOf(current.value, offsets.value, picked.value) : "";
}

function selectAll(): void {
  const host = textEl.value;
  const selection = document.getSelection();
  if (!host || !selection) return;
  const range = document.createRange();
  range.selectNodeContents(host);
  selection.removeAllRanges();
  selection.addRange(range);
}

// ---------------------------------------------------------------------------------- copying

async function copy(text: string): Promise<void> {
  if (!text.trim()) return;
  try {
    await ocr_copy(text, current.value?.language ?? null, props.displayId);
  } catch (error) {
    console.warn("[glimpsfy] ocr_copy", error);
  }
}

function copySelection(): Promise<void> {
  return copy(selectionText());
}

function copyAll(): Promise<void> {
  return copy(current.value?.text ?? "");
}

function hasSelection(): boolean {
  return selectionText().length > 0;
}

/** The system's own copy (the Edit menu, a context menu) gets the same text as ⌘C. */
function onCopy(e: ClipboardEvent): void {
  const text = selectionText();
  if (!text || !e.clipboardData) return;
  e.preventDefault();
  e.clipboardData.setData("text/plain", text);
}

/** ⌘C, ⇧⌘C and ⌘A belong to the layer while it shows, before the page's own shortcuts see them. */
function onKeyDown(e: KeyboardEvent): void {
  if (!shown.value || !current.value || e.altKey || !(e.metaKey || e.ctrlKey)) return;
  const key = e.key.toLowerCase();
  const take = () => {
    e.preventDefault();
    e.stopImmediatePropagation();
  };
  if (key === "c" && !e.shiftKey) {
    const text = selectionText();
    if (!text) return;
    take();
    void copy(text);
  } else if (key === "c" && e.shiftKey) {
    if (!current.value.text.trim()) return;
    take();
    void copyAll();
  } else if (key === "a" && !e.shiftKey && current.value.lines.length) {
    take();
    selectAll();
  }
}

function hide(): void {
  if (context) context.visible.value = false;
  emit("hide");
}

onMounted(() => {
  document.addEventListener("selectionchange", onSelectionChange);
  window.addEventListener("keydown", onKeyDown, true);
});
onBeforeUnmount(() => {
  clearTimeout(pending);
  document.removeEventListener("selectionchange", onSelectionChange);
  window.removeEventListener("keydown", onKeyDown, true);
});
watch(current, clearSelection);

defineExpose({ copySelection, copyAll, hasSelection });

// ------------------------------------------------------------------------------- the chips

/** The glass chip sits 16 px above the bottom of the image, centred, and inside the window. */
const chipAt = computed(() => {
  const r = rect.value;
  if (!r) return null;
  const room = root.value?.clientWidth ?? window.innerWidth;
  const center = Math.min(Math.max(r.x + r.width / 2, 180), Math.max(180, room - 180));
  const inside = r.height >= 96;
  return { left: `${center}px`, top: `${inside ? r.y + r.height - 16 - 40 : r.y + r.height + 12}px` };
});

const summary = computed(() => {
  const result = current.value;
  if (!result) return "";
  const n = result.lines.length ? lineCount(result.text) : 0;
  const lines = n === 1 ? t("ocr.chip.lines.one") : t("ocr.chip.lines.many", { n });
  const name = languageName(result.language, language.value);
  return name ? `${lines} · ${name}` : lines;
});

const message = computed(() => {
  if (status.value === "reading") return t("ocr.reading");
  if (status.value === "failed") return recognition.problem.value === "unavailable" ? t("ocr.unavailable") : t("ocr.failed");
  if (status.value === "done" && current.value && !current.value.text.trim()) return t("ocr.empty");
  return null;
});

const codeChips = computed(() => {
  const p = place.value;
  if (!p) return [];
  return codes.value.map((code, i) => {
    const r = p(code.bounds);
    return {
      key: i,
      code,
      outline: { x: r.x - 5, y: r.y - 5, width: r.width + 10, height: r.height + 10 },
      at: { left: `${r.x - 5}px`, top: `${r.y + r.height + 12}px` },
      label: code.symbology === "qr" || code.symbology === "microQr" ? t("ocr.code.qr") : t("ocr.code.barcode"),
      shown: code.url ?? code.payload,
    };
  });
});

/** How far each chip moves down so that no two overlap: the summary chip first, then the code
 * chips. A chip's width is known only once it is drawn, so this runs after each render. */
const shifts = ref<number[]>([]);
const summaryEl = ref<HTMLElement | null>(null);
const codeEls = ref<HTMLElement[]>([]);

async function layoutChips(): Promise<void> {
  await nextTick();
  const els = [summaryEl.value, ...codeEls.value];
  const placed: { left: number; right: number; top: number; bottom: number }[] = [];
  const next = els.map((el, i) => {
    if (!el) return 0;
    const r = el.getBoundingClientRect();
    const box = { left: r.left, right: r.right, top: r.top - (shifts.value[i] ?? 0), bottom: 0 };
    const start = box.top;
    box.bottom = box.top + r.height;
    for (let moved = true; moved; ) {
      moved = false;
      for (const p of placed) {
        if (box.left < p.right && box.right > p.left && box.top < p.bottom + 8 && box.bottom + 8 > p.top) {
          const by = p.bottom + 8 - box.top;
          box.top += by;
          box.bottom += by;
          moved = true;
        }
      }
    }
    placed.push(box);
    return box.top - start;
  });
  if (next.some((v, i) => Math.abs(v - (shifts.value[i] ?? 0)) > 0.5)) shifts.value = next;
}
watch([codeChips, chipAt, message, summary, shown], layoutChips, { flush: "post" });

const shifted = (at: { left: string; top: string }, i: number) => ({ ...at, top: `${parseFloat(at.top) + (shifts.value[i] ?? 0)}px` });

async function openLink(url: string): Promise<void> {
  try {
    await app_open_url(url);
  } catch (error) {
    console.warn("[glimpsfy] app_open_url", error);
  }
}
</script>

<template>
  <div v-if="shown && rect" ref="root" class="ocr-layer" :data-status="status">
    <div class="ocr-veil" :style="css(rect)" />
    <span v-for="(box, i) in outlines" :key="`o${i}`" class="ocr-line" :style="css(box)" />
    <span v-for="chip in codeChips" :key="`q${chip.key}`" class="ocr-line" data-kind="code" :style="css(chip.outline)" />
    <span v-for="(box, i) in selRects" :key="`s${i}`" class="ocr-sel" :style="css(box)" />
    <template v-if="carets">
      <span class="ocr-caret" data-end="start" :style="css(carets.start)" />
      <span class="ocr-caret" data-end="end" :style="css(carets.end)" />
    </template>

    <div
      ref="textEl"
      class="ocr-text"
      role="document"
      :aria-label="t('ocr.layer')"
      :style="css(rect)"
      @pointerdown.stop
      @mousedown.stop
      @click.stop
      @dblclick.stop
      @copy="onCopy"
    >
      <span v-for="word in words" :key="word.key" class="ocr-word" :data-l="word.l" :data-w="word.w" :style="word.style">{{ word.text }}</span>
    </div>

    <div v-if="chipAt && (message || (current && current.text.trim()))" ref="summaryEl" class="g-chip ocr-chip" :style="shifted(chipAt, 0)" @pointerdown.stop @mousedown.stop>
      <GIcon name="ocr" :size="16" />
      <span class="g-chip-text" role="status">
        <small>{{ t("ocr.chip.title") }}</small>{{ message ?? summary }}
      </span>
      <button
        v-if="!message && current"
        type="button"
        class="g-button"
        data-variant="primary"
        aria-keyshortcuts="Shift+Meta+C"
        @click="copyAll"
      >
        <GIcon name="copy" :size="16" />{{ t("ocr.copyAll") }}
      </button>
      <button type="button" class="g-btn" :aria-label="t('ocr.hide')" :title="t('ocr.hide')" aria-keyshortcuts="Escape" @click="hide">
        <GIcon name="close" :size="16" />
      </button>
    </div>

    <div
      v-for="(chip, i) in codeChips"
      :key="`c${chip.key}`"
      ref="codeEls"
      class="g-chip ocr-code"
      :style="shifted(chip.at, i + 1)"
      @pointerdown.stop
      @mousedown.stop
    >
      <GIcon name="qr" :size="16" />
      <span class="g-chip-text"><small>{{ chip.label }}</small>{{ chip.shown }}</span>
      <button v-if="chip.code.url" type="button" class="g-button" data-variant="primary" @click="openLink(chip.code.url)">
        <GIcon name="external-link" :size="16" />{{ t("ocr.code.open") }}
      </button>
      <button type="button" class="g-button" @click="copy(chip.code.payload)">
        <GIcon name="copy" :size="16" />{{ t("ocr.code.copy") }}
      </button>
    </div>
  </div>
</template>

<style scoped>
.ocr-layer {
  position: absolute;
  inset: 0;
  pointer-events: none;
}
.ocr-veil {
  position: absolute;
  background: var(--ocr-veil);
}
.ocr-line {
  position: absolute;
  border-radius: var(--radius-key);
  box-shadow:
    0 0 0 1.25px var(--ocr-line),
    0 0 0 3px var(--selection-inner);
}
.ocr-line[data-kind="code"] {
  border-radius: var(--radius-thumb);
}
.ocr-sel {
  position: absolute;
  border-radius: 2px;
  background: var(--iris-soft);
  mix-blend-mode: multiply;
}
.ocr-caret {
  position: absolute;
  z-index: 2;
  border-radius: 1px;
  background: var(--iris);
}
.ocr-caret::before {
  content: "";
  position: absolute;
  left: -3px;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--iris);
  box-shadow: 0 0 0 1.5px var(--handle);
}
.ocr-caret[data-end="start"]::before {
  top: -7px;
}
.ocr-caret[data-end="end"]::before {
  bottom: -7px;
}
.ocr-text {
  position: absolute;
  z-index: 1;
  pointer-events: auto;
  cursor: text;
  -webkit-user-select: text;
  user-select: text;
}
/* The words stay invisible over the pixels; the highlight is drawn by .ocr-sel instead. */
.ocr-word {
  position: absolute;
  white-space: pre;
  color: transparent;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  transform-origin: 0 0;
}
.ocr-word::selection {
  background: transparent;
  color: transparent;
}
.ocr-chip {
  position: absolute;
  z-index: 3;
  transform: translateX(-50%);
  pointer-events: auto;
}
.ocr-code {
  position: absolute;
  z-index: 3;
  pointer-events: auto;
}
.ocr-layer[data-status="reading"] .ocr-veil {
  animation: ocr-breathe 1.6s var(--ease-out, ease-out) infinite alternate;
}
@keyframes ocr-breathe {
  from {
    opacity: 1;
  }
  to {
    opacity: 0.55;
  }
}
@media (prefers-reduced-motion: reduce) {
  .ocr-layer[data-status="reading"] .ocr-veil {
    animation: none;
  }
}
</style>
