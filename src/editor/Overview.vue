<script setup lang="ts">
// The overview of a tall image (D27, Done): the whole picture shrunk into a 56 pt glass column at
// the canvas's right edge, with the iris frame on the part the canvas shows and the image's
// height under it. A press or a drag in it brings that part into the canvas. The editor shows it
// while a tall image (a scrolling capture's result) overflows the canvas, which fit width leaves
// room for (`view.ts`).
import { computed, onBeforeUnmount, ref, watch } from "vue";

import type { ImageSource } from "../annotate/compose";
import type { PixelRect } from "../annotate/redact";
import type { View } from "../annotate/textLayer";
import { language, t } from "../lib/i18n";
import type { Room } from "./view";

const props = defineProps<{ source: ImageSource; area: PixelRect; view: View; room: Room }>();
/** The canvas's vertical offset that centres image row `y`, for the editor to apply. */
const emit = defineEmits<{ pan: [y: number] }>();

/** The picture's width inside the 56 pt column, and the column's own margins and number. */
const INNER = 44;
const REST = 12 + 12 + 6 + 6 + 22;

const canvasEl = ref<HTMLCanvasElement | null>(null);
const s = computed(() => Math.min(INNER / props.area.width, Math.max(60, props.room.height - REST) / props.area.height));
const size = computed(() => ({ w: Math.max(1, Math.round(props.area.width * s.value)), h: Math.max(1, Math.round(props.area.height * s.value)) }));
/** The rows the canvas shows, as a frame on the picture. */
const shown = computed(() => {
  const { k, oy } = props.view;
  const first = Math.max(props.area.y, -oy / k);
  const last = Math.min(props.area.y + props.area.height, (props.room.height - oy) / k);
  const top = Math.round((first - props.area.y) * s.value);
  return { top, height: Math.max(4, Math.round((last - first) * s.value)) };
});
const height = computed(() => new Intl.NumberFormat(language.value).format(props.area.height));

function draw(): void {
  const canvas = canvasEl.value;
  if (!canvas) return;
  const d = window.devicePixelRatio || 1;
  const w = Math.round(size.value.w * d);
  const h = Math.round(size.value.h * d);
  if (canvas.width !== w) canvas.width = w;
  if (canvas.height !== h) canvas.height = h;
  const g = canvas.getContext("2d", { colorSpace: props.source.colorSpace });
  if (!g) return;
  g.imageSmoothingEnabled = true;
  g.imageSmoothingQuality = "high";
  const a = props.area;
  g.clearRect(0, 0, w, h);
  g.drawImage(props.source.canvas, a.x, a.y, a.width, a.height, 0, 0, w, h);
}
watch([() => props.source, () => props.area, size, canvasEl], draw, { flush: "post" });

let dragging = false;
function seek(e: PointerEvent): void {
  const box = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const y = Math.max(0, Math.min(size.value.h, e.clientY - box.top));
  emit("pan", props.area.y + y / s.value);
}
function onDown(e: PointerEvent): void {
  dragging = true;
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  seek(e);
}
function onMove(e: PointerEvent): void {
  if (dragging) seek(e);
}
function onUp(e: PointerEvent): void {
  dragging = false;
  const target = e.currentTarget as HTMLElement;
  if (target.hasPointerCapture(e.pointerId)) target.releasePointerCapture(e.pointerId);
}
onBeforeUnmount(() => (dragging = false));
</script>

<template>
  <div class="g-glass e-overview" role="navigation" :aria-label="t('editor.overview')" @pointerdown.stop @wheel.stop>
    <div
      class="e-overview-map"
      :style="{ width: `${size.w}px`, height: `${size.h}px` }"
      @pointerdown="onDown"
      @pointermove="onMove"
      @pointerup="onUp"
      @pointercancel="onUp"
    >
      <canvas ref="canvasEl" :style="{ width: `${size.w}px`, height: `${size.h}px` }" />
      <span class="e-overview-view" :style="{ top: `${shown.top}px`, height: `${shown.height}px` }" />
    </div>
    <span class="e-overview-num">{{ height }}</span>
  </div>
</template>

<style scoped>
.e-overview {
  position: absolute;
  right: 12px;
  top: 12px;
  bottom: 12px;
  z-index: 6;
  display: flex;
  flex-direction: column;
  align-items: center;
  width: 56px;
  padding: var(--space-6);
  border-radius: var(--radius-lens);
}
.e-overview-map {
  position: relative;
  flex: none;
  overflow: hidden;
  border-radius: 4px;
  cursor: pointer;
  box-shadow: 0 0 0 var(--hairline) var(--edge-inner);
}
.e-overview-map > canvas {
  display: block;
}
.e-overview-view {
  position: absolute;
  left: 0;
  right: 0;
  border-radius: 2px;
  pointer-events: none;
  box-shadow: 0 0 0 1.5px var(--iris-text), 0 0 0 999px var(--dim);
}
.e-overview-num {
  margin-top: auto;
  padding-top: var(--space-6);
  font: var(--type-num-small);
  font-size: 10px;
  font-variant-numeric: tabular-nums;
  color: var(--text-muted);
  white-space: nowrap;
}
</style>
