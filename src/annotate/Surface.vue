<script setup lang="ts">
// The annotation layer over an image, shared by the overlay and the editor: the composed canvas
// (the image with its redactions and drawings, from compose.ts), the selected object's outline and
// handles, and the field the user types a text into. The page around it owns the pointer and
// converts it with the same `view`; this component only shows the engine's state.
//
// The canvas holds the composed area at the image's own resolution and is scaled by CSS, so what
// the user sees is pixel for pixel what the export holds.
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";

import type { AnnotationObject } from "../lib/annotate";
import { Composer, type ImageSource } from "./compose";
import { layoutText } from "./draw";
import type { AnnotationEngine } from "./engine";
import type { PixelRect } from "./redact";
import { TEXT_PAD, TEXT_RADIUS, cssOf, textFont } from "./style";
import type { View } from "./textLayer";

type Text = Extract<AnnotationObject, { kind: "text" }>;

const props = withDefaults(
  defineProps<{
    engine: AnnotationEngine;
    source: ImageSource | null;
    view: View;
    /** The part of the image the canvas shows, in image pixels. */
    area: PixelRect | null;
    /** Draws the composed canvas; the overlay leaves it out while nothing is drawn, because its
     * frame already shows the same pixels. */
    composed?: boolean;
    /** Shows the selected object's outline and handles. */
    chrome?: boolean;
  }>(),
  { composed: true, chrome: true },
);

const canvas = ref<HTMLCanvasElement | null>(null);
const field = ref<HTMLElement | null>(null);
let composer: Composer | null = null;
let frame = 0;

const rev = computed(() => props.engine.rev.value);
const px = (v: number) => `${v}px`;

/** A rectangle in image pixels on the screen. */
function screen(r: { x: number; y: number; width: number; height: number }) {
  const { k, ox, oy } = props.view;
  return { left: px(ox + r.x * k), top: px(oy + r.y * k), width: px(r.width * k), height: px(r.height * k) };
}

const canvasStyle = computed(() => {
  const a = props.area;
  if (!a) return { display: "none" };
  // From two screen pixels per image pixel on, the pixels show as squares, as in any image editor.
  const rendering: "pixelated" | "auto" = props.view.k * (window.devicePixelRatio || 1) >= 2 ? "pixelated" : "auto";
  return { ...screen(a), imageRendering: rendering };
});

function draw(): void {
  frame = 0;
  const a = props.area;
  const el = canvas.value;
  const source = props.source;
  if (!a || !el || !source || !props.composed) return;
  if (!composer || composer.source !== source) composer = new Composer(source);
  if (el.width !== a.width) el.width = a.width;
  if (el.height !== a.height) el.height = a.height;
  const ctx = el.getContext("2d", { colorSpace: source.colorSpace });
  if (!ctx) return;
  composer.draw(ctx, props.engine.doc, a, props.engine.editingId);
}

function schedule(): void {
  if (!frame) frame = requestAnimationFrame(draw);
}

watch(
  () => [rev.value, props.area?.x, props.area?.y, props.area?.width, props.area?.height, props.source, props.composed, canvas.value],
  schedule,
  { immediate: true },
);
onBeforeUnmount(() => cancelAnimationFrame(frame));

// ------------------------------------------------------------------------ outline, handles

const selection = computed(() => {
  void rev.value;
  const e = props.engine;
  if (!props.chrome || !e.selectedId || e.busy() || e.editingId) return null;
  // An arrow or a line is held by its two ends; a box around a slanted stroke reads as a crop.
  const kind = e.selected()?.kind;
  if (kind === "arrow" || kind === "line") return null;
  const r = e.frame(e.selectedId);
  return r ? screen(r) : null;
});

const handles = computed(() => {
  void rev.value;
  if (!props.chrome || props.engine.editingId) return [];
  const { k, ox, oy } = props.view;
  return props.engine.handles().map((h) => ({
    key: h.id,
    kind: h.kind === "x" || h.kind === "y" ? h.kind : undefined,
    style: { left: px(ox + h.x * k), top: px(oy + h.y * k) },
  }));
});

// ------------------------------------------------------------------------------- the text

const editing = computed<Text | null>(() => {
  void rev.value;
  const o = props.engine.get(props.engine.editingId);
  return o && o.kind === "text" ? o : null;
});

const fieldStyle = computed(() => {
  const o = editing.value;
  if (!o) return {};
  const { k, ox, oy } = props.view;
  const size = o.fontSize * k;
  const layout = layoutText(o);
  return {
    left: px(ox + o.origin.x * k),
    top: px(oy + o.origin.y * k),
    minWidth: px(layout.width * k),
    maxWidth: o.maxWidth === null ? undefined : px(o.maxWidth * k),
    whiteSpace: o.maxWidth === null ? "pre" : "pre-wrap",
    font: textFont(size),
    lineHeight: px(layout.lineHeight * k),
    padding: `${px(TEXT_PAD.top * size)} ${px(TEXT_PAD.x * size)} ${px(TEXT_PAD.bottom * size)}`,
    borderRadius: px(TEXT_RADIUS * size),
    color: cssOf(o.color),
    background: o.background ? cssOf(o.background) : "transparent",
  };
});

// A new field for every text that starts being typed: it takes the text once and the focus, and
// from then on the field is the source and the engine follows it.
watch(
  // The engine is not reactive itself; `rev` tells when to look again.
  () => (rev.value, props.engine.editingId),
  async (id) => {
    if (!id) return;
    await nextTick();
    const el = field.value;
    const o = editing.value;
    if (!el || !o) return;
    el.textContent = o.text;
    el.focus();
    const range = document.createRange();
    range.selectNodeContents(el);
    range.collapse(false);
    const chosen = window.getSelection();
    chosen?.removeAllRanges();
    chosen?.addRange(range);
  },
  { immediate: true },
);

function onInput(): void {
  // `innerText` keeps the line breaks a contenteditable makes for Return.
  props.engine.typeText((field.value?.innerText ?? "").replace(/\n$/, ""));
}

function onKeyDown(e: KeyboardEvent): void {
  // The overlay and the editor listen on the window; a key typed into the text is the text's.
  e.stopPropagation();
  if (e.key === "Escape" || (e.key === "Enter" && (e.metaKey || e.ctrlKey))) {
    e.preventDefault();
    props.engine.finishText();
  }
}

function onBlur(): void {
  props.engine.finishText();
}
</script>

<template>
  <canvas v-if="composed && source" :key="source.colorSpace" ref="canvas" class="a-canvas" :style="canvasStyle" aria-hidden="true" />
  <div v-if="selection" class="a-objsel" :style="selection" aria-hidden="true" />
  <span v-for="h in handles" :key="h.key" class="g-handle a-handle" :data-handle="h.kind" :style="h.style" aria-hidden="true" />
  <div
    v-if="editing"
    :key="editing.id"
    ref="field"
    class="a-textfield"
    :style="fieldStyle"
    contenteditable="plaintext-only"
    role="textbox"
    aria-multiline="true"
    spellcheck="false"
    @input="onInput"
    @keydown="onKeyDown"
    @blur="onBlur"
    @mousedown.stop
  />
</template>

<style scoped>
/* No z-index: the canvas stacks in document order, under the page's own selection edge. */
.a-canvas {
  position: absolute;
  pointer-events: none;
}
/* The selected object: the overlay's outline at screen size. */
.a-objsel {
  position: absolute;
  z-index: 6;
  pointer-events: none;
}
.a-objsel::before {
  content: "";
  position: absolute;
  inset: -1.5px;
  border: 1.5px solid var(--iris);
  border-radius: 2px;
}
.a-objsel::after {
  content: "";
  position: absolute;
  inset: 0;
  border-radius: 1px;
  box-shadow: inset 0 0 0 1px var(--selection-inner);
}
.a-handle {
  z-index: 7;
  pointer-events: none;
}
/* The text being typed: the label itself, with a caret and an iris ring. */
.a-textfield {
  position: absolute;
  z-index: 8;
  box-sizing: border-box;
  outline: none;
  cursor: text;
  -webkit-user-select: text;
  user-select: text;
  caret-color: currentColor;
  box-shadow:
    0 0 0 2px var(--ink-white),
    0 0 0 3.5px var(--iris);
}
</style>
