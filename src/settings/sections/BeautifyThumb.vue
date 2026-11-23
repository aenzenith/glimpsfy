<script setup lang="ts">
// A preset's thumbnail in Settings › Beautify: the preset's style drawn around a sample screen,
// as the beautify panel's preview draws it around a capture (`annotate/beautify.ts`).
import { onBeforeUnmount, ref, watch } from "vue";

import type { BeautifyStyle } from "../../lib/annotate";
import { drawBeautified, edgeMedian, layoutOf, useBackgroundImage } from "../../annotate/beautify";

const props = defineProps<{ current: BeautifyStyle; jpeg: boolean }>();

const W = 64;
const H = 40;
/** The sample is the size of a typical area selection, so the padding reads as it would. */
const SAMPLE = { width: 434, height: 276 };

let sample: HTMLCanvasElement | null = null;
let sampleEdge: [number, number, number] = [244, 245, 247];
/** A light window: a title strip and a few lines of text, drawn once for every thumbnail. */
function sampleImage(): HTMLCanvasElement {
  if (sample) return sample;
  const c = document.createElement("canvas");
  c.width = SAMPLE.width;
  c.height = SAMPLE.height;
  const ctx = c.getContext("2d");
  if (ctx) {
    ctx.fillStyle = "#f4f5f7";
    ctx.fillRect(0, 0, c.width, c.height);
    ctx.fillStyle = "#e3e5ea";
    ctx.fillRect(0, 0, c.width, 40);
    ctx.fillStyle = "#c9ccd4";
    for (const [y, w] of [[72, 260], [104, 330], [136, 210], [168, 300], [200, 180]] as const) ctx.fillRect(32, y, w, 14);
    ctx.fillStyle = "#4b6bfb";
    ctx.fillRect(32, 228, 96, 24);
    const data = ctx.getImageData(0, 0, c.width, c.height);
    sampleEdge = edgeMedian(data);
  }
  sample = c;
  return c;
}

const canvas = ref<HTMLCanvasElement | null>(null);
const background = useBackgroundImage(() => (props.current.background.kind === "image" ? props.current.background.path : null), 256);
let frame = 0;

function draw(): void {
  frame = 0;
  const el = canvas.value;
  if (!el) return;
  const image = sampleImage();
  const from = { x: 0, y: 0, ...SAMPLE };
  const b = { image, from, scale: 1, style: props.current, window: false, space: "srgb" as const, match: sampleEdge, backgroundImage: background.image.value, jpeg: props.jpeg };
  const L = layoutOf(b);
  const k = Math.min(W / L.width, H / L.height);
  const dpr = window.devicePixelRatio || 1;
  const cssW = Math.max(1, Math.round(L.width * k));
  const cssH = Math.max(1, Math.round(L.height * k));
  el.style.width = `${cssW}px`;
  el.style.height = `${cssH}px`;
  el.width = Math.round(cssW * dpr);
  el.height = Math.round(cssH * dpr);
  const ctx = el.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(el.width / L.width, 0, 0, el.height / L.height, 0, 0);
  ctx.imageSmoothingQuality = "high";
  drawBeautified(ctx, b);
}

watch(
  () => [props.current, props.jpeg, background.image.value, canvas.value],
  () => {
    if (!frame) frame = requestAnimationFrame(draw);
  },
  { immediate: true, flush: "post" },
);
onBeforeUnmount(() => cancelAnimationFrame(frame));
</script>

<template>
  <span class="bt-thumb" aria-hidden="true"><canvas ref="canvas" /></span>
</template>

<style scoped>
.bt-thumb {
  flex: none;
  display: grid;
  place-items: center;
  width: 64px;
  height: 40px;
  overflow: hidden;
  border-radius: 6px;
  background: var(--win-sunken);
  box-shadow: 0 0 0 var(--hairline) var(--win-line);
}
.bt-thumb > canvas {
  display: block;
}
</style>
