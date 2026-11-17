<script setup lang="ts">
// The trim timeline (design `buildTimeline`/`updateTimeline`): the in, out and kept-length chips,
// a ruler, frame thumbnails over the waveform, everything outside the kept range dimmed, two iris
// handles and the playhead. The handles are sliders: drag them, or use ← and → (0.1 s, ⇧ 1 s),
// Home and End; the kept part never falls under half a second. A press on the strip moves the
// playhead inside the kept range. During an export the handles lock and the kept range fills with
// iris as the encoder passes through it.
import { computed, onMounted, onUnmounted, ref, watch, watchPostEffect } from "vue";

import { recordingFrameUrl, recordingWaveUrl } from "../lib/api/record";
import { t } from "../lib/i18n";
import { withEdge } from "./choice";
import { clock, tenths } from "./format";
import { clamp } from "./geometry";

/** Which end of the trim a handle moves. */
export type Edge = "start" | "end";

const props = defineProps<{
  recordingId: string;
  totalMs: number;
  startMs: number;
  endMs: number;
  playMs: number;
  /** The video's width over its height, for the thumbnails. */
  aspect: number;
  locked: boolean;
  /** How much of the kept range the export has encoded, from 0 to 1. */
  encoded: number;
}>();
const emit = defineEmits<{ range: [startMs: number, endMs: number]; seek: [ms: number] }>();

const THUMB_H = 52;
const WAVE_STEP = 3;
const MINOR_STEPS = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600];

const root = ref<HTMLElement | null>(null);
const track = ref<HTMLElement | null>(null);
const chipIn = ref<HTMLElement | null>(null);
const chipOut = ref<HTMLElement | null>(null);
const chipDur = ref<HTMLElement | null>(null);
const width = ref(0);
/** The width the thumbnails and the waveform were made for; it follows `width` once it settles. */
const settled = ref(0);
const peaks = ref<number[]>([]);

let settleTimer = 0;
let waveAbort: AbortController | null = null;
let drag: { edge: Edge; offset: number } | null = null;
let scrubbing = false;

const X = (ms: number) => (props.totalMs > 0 ? (ms / props.totalMs) * width.value : 0);
const xi = computed(() => X(props.startMs));
const xo = computed(() => X(props.endMs));

// ------------------------------------------------------------------------------ the strips

const ticks = computed(() => {
  const w = width.value;
  const seconds = props.totalMs / 1000;
  if (!w || seconds <= 0) return [];
  const perSecond = w / seconds;
  const minor = MINOR_STEPS.find((s) => s * perSecond >= 6) ?? 3600;
  const major = MINOR_STEPS.find((s) => s >= minor && s % minor === 0 && s * perSecond >= 56) ?? minor;
  const list: { x: number; major: boolean; label: string | null }[] = [];
  for (let s = 0; s <= seconds; s += minor) {
    const x = s * perSecond;
    const isMajor = s % major === 0;
    list.push({ x, major: isMajor, label: isMajor && x < w - 30 ? clock(s * 1000) : null });
  }
  return list;
});

const thumbs = computed(() => {
  const w = settled.value;
  if (!w || props.totalMs <= 0) return [];
  const n = Math.max(4, Math.round(w / (THUMB_H * Math.max(0.1, props.aspect))));
  const tw = w / n;
  const height = Math.round(THUMB_H * (window.devicePixelRatio || 1));
  return Array.from({ length: n }, (_, i) => ({
    key: i,
    width: tw,
    src: recordingFrameUrl(props.recordingId, ((i + 0.5) / n) * props.totalMs, height),
  }));
});

const wave = computed(() => {
  const list = peaks.value;
  return {
    viewBox: `0 0 ${Math.max(1, list.length * WAVE_STEP)} 32`,
    bars: list.map((p, i) => {
      const h = Math.max(1.5, clamp(p, 0, 1) * 26);
      return { x: i * WAVE_STEP, y: 16 - h / 2, h };
    }),
  };
});

async function loadWave(): Promise<void> {
  waveAbort?.abort();
  const bins = Math.floor(settled.value / WAVE_STEP);
  if (bins < 1) return;
  const abort = new AbortController();
  waveAbort = abort;
  try {
    const response = await fetch(recordingWaveUrl(props.recordingId, bins), { signal: abort.signal });
    if (!response.ok) throw new Error(`The waveform answered ${response.status}.`);
    const body = (await response.json()) as { peaks?: unknown };
    peaks.value = Array.isArray(body.peaks) ? body.peaks.map((p) => (typeof p === "number" && Number.isFinite(p) ? p : 0)) : [];
  } catch (error) {
    if (abort.signal.aborted) return;
    // A recording without sound, or a waveform that failed, leaves the strip empty.
    peaks.value = [];
    console.warn("The waveform could not load.", error);
  }
}

watch(width, (w) => {
  window.clearTimeout(settleTimer);
  settleTimer = window.setTimeout(() => (settled.value = w), settled.value ? 160 : 0);
});
watch([settled, () => props.recordingId], () => void loadWave());

// ------------------------------------------------------------------------------- the trim

function setEdge(edge: Edge, ms: number): void {
  if (props.locked) return;
  const next = withEdge({ startMs: props.startMs, endMs: props.endMs }, edge, ms, props.totalMs);
  emit("range", next.startMs, next.endMs);
}

function msAt(clientX: number, offset = 0): number {
  const r = track.value?.getBoundingClientRect();
  if (!r || !r.width) return 0;
  return ((clientX - r.left - offset) / r.width) * props.totalMs;
}

function grab(edge: Edge, event: PointerEvent): void {
  if (props.locked || event.button !== 0) return;
  event.preventDefault();
  event.stopPropagation();
  const handle = event.currentTarget as HTMLElement;
  handle.focus({ preventScroll: true });
  handle.setPointerCapture(event.pointerId);
  const r = track.value?.getBoundingClientRect();
  const cut = X(edge === "start" ? props.startMs : props.endMs);
  drag = { edge, offset: r ? event.clientX - r.left - cut : 0 };
}

function moveHandle(event: PointerEvent): void {
  if (drag) setEdge(drag.edge, msAt(event.clientX, drag.offset));
}

function release(): void {
  drag = null;
}

function key(edge: Edge, event: KeyboardEvent): void {
  const step = event.shiftKey ? 1000 : 100;
  const now = edge === "start" ? props.startMs : props.endMs;
  let next: number | null = null;
  if (event.key === "ArrowLeft" || event.key === "ArrowDown") next = now - step;
  else if (event.key === "ArrowRight" || event.key === "ArrowUp") next = now + step;
  else if (event.key === "Home") next = 0;
  else if (event.key === "End") next = props.totalMs;
  if (next === null) return;
  event.preventDefault();
  event.stopPropagation();
  setEdge(edge, next);
}

function seekAt(clientX: number): void {
  emit("seek", clamp(msAt(clientX), props.startMs, props.endMs));
}

function press(event: PointerEvent): void {
  if (event.button !== 0 || (event.target as Element).closest(".r-tl-handle")) return;
  scrubbing = true;
  (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  seekAt(event.clientX);
}

function scrub(event: PointerEvent): void {
  if (scrubbing) seekAt(event.clientX);
}

// --------------------------------------------------------------------------------- chips

// The in and out chips sit centred on their handles and step aside when they would meet; the
// kept length shows between them only when it fits (design `updateTimeline`).
watchPostEffect(() => {
  const w = width.value;
  const a0 = xi.value;
  const b0 = xo.value;
  void props.startMs;
  void props.endMs;
  const li = chipIn.value;
  const lo = chipOut.value;
  const ld = chipDur.value;
  if (!li || !lo || !ld || !w) return;
  const wi = li.offsetWidth;
  const wo = lo.offsetWidth;
  const wd = ld.offsetWidth;
  let a = clamp(a0 - 7 - wi / 2, -14, w + 14 - wi);
  let b = clamp(b0 + 7 - wo / 2, -14, w + 14 - wo);
  if (a + wi + 4 > b) {
    a = a0 - wi;
    b = b0;
  }
  li.style.left = `${a}px`;
  lo.style.left = `${b}px`;
  const d = (a0 + b0) / 2 - wd / 2;
  ld.style.left = `${d}px`;
  ld.style.visibility = d > a + wi + 6 && d + wd + 6 < b ? "visible" : "hidden";
});

const observer = new ResizeObserver(() => {
  if (root.value) width.value = root.value.clientWidth;
});

onMounted(() => {
  if (root.value) {
    width.value = root.value.clientWidth;
    observer.observe(root.value);
  }
});

onUnmounted(() => {
  observer.disconnect();
  waveAbort?.abort();
  window.clearTimeout(settleTimer);
});

const valueNow = (ms: number) => (ms / 1000).toFixed(1);
</script>

<template>
  <div ref="root" class="r-tl">
    <div class="r-tl-labels" aria-hidden="true">
      <span ref="chipIn" class="r-tl-time">{{ tenths(startMs) }}</span>
      <span ref="chipDur" class="r-tl-dur">{{ tenths(endMs - startMs) }}</span>
      <span ref="chipOut" class="r-tl-time">{{ tenths(endMs) }}</span>
    </div>
    <div class="r-tl-ruler" aria-hidden="true">
      <template v-for="tick in ticks" :key="tick.x">
        <i class="r-tick" :style="{ left: `${tick.x}px` }" :data-major="tick.major ? '' : undefined" />
        <span v-if="tick.label" class="r-tick-label" :style="{ left: `${tick.x}px` }">{{ tick.label }}</span>
      </template>
    </div>
    <div ref="track" class="r-tl-track" @pointerdown="press" @pointermove="scrub" @pointerup="scrubbing = false" @pointercancel="scrubbing = false">
      <div class="r-tl-strip">
        <div v-for="thumb in thumbs" :key="thumb.key" class="r-tl-thumb" :style="{ width: `${thumb.width}px` }">
          <img
            :key="thumb.src"
            :src="thumb.src"
            alt=""
            draggable="false"
            decoding="async"
            @error="($event.target as HTMLImageElement).style.visibility = 'hidden'"
          />
        </div>
        <div class="r-tl-shade" :style="{ left: '0px', width: `${xi}px` }" />
        <div class="r-tl-shade" :style="{ left: `${xo}px`, right: '0px' }" />
        <div class="r-tl-enc" :style="{ left: `${xi}px`, width: `${(xo - xi) * encoded}px` }" />
      </div>
      <div class="r-tl-wave">
        <svg :viewBox="wave.viewBox" preserveAspectRatio="none" aria-hidden="true">
          <rect v-for="(bar, i) in wave.bars" :key="i" :x="bar.x" :y="bar.y" width="2" :height="bar.h" rx="1" />
        </svg>
        <div class="r-tl-shade" :style="{ left: '0px', width: `${xi}px` }" />
        <div class="r-tl-shade" :style="{ left: `${xo}px`, right: '0px' }" />
        <div class="r-tl-enc" :style="{ left: `${xi}px`, width: `${(xo - xi) * encoded}px` }" />
      </div>
      <div class="r-tl-range" :style="{ left: `${xi}px`, width: `${xo - xi}px` }" />
      <button
        type="button"
        class="r-tl-handle"
        data-edge="start"
        role="slider"
        :aria-label="t('record.trim.start')"
        aria-valuemin="0"
        :aria-valuemax="valueNow(totalMs)"
        :aria-valuenow="valueNow(startMs)"
        :aria-valuetext="tenths(startMs)"
        :disabled="locked"
        :style="{ left: `${xi - 14}px` }"
        @pointerdown="grab('start', $event)"
        @pointermove="moveHandle"
        @pointerup="release"
        @pointercancel="release"
        @keydown="key('start', $event)"
      />
      <button
        type="button"
        class="r-tl-handle"
        data-edge="end"
        role="slider"
        :aria-label="t('record.trim.end')"
        aria-valuemin="0"
        :aria-valuemax="valueNow(totalMs)"
        :aria-valuenow="valueNow(endMs)"
        :aria-valuetext="tenths(endMs)"
        :disabled="locked"
        :style="{ left: `${xo}px` }"
        @pointerdown="grab('end', $event)"
        @pointermove="moveHandle"
        @pointerup="release"
        @pointercancel="release"
        @keydown="key('end', $event)"
      />
    </div>
    <div class="r-tl-play" :style="{ left: `${X(playMs)}px` }" aria-hidden="true" />
  </div>
</template>

<style>
.r-tl { position: relative; flex: none; height: 130px; margin: 0 var(--space-20) var(--space-20); }
.r-tl-labels { position: relative; height: 24px; }
.r-tl-time, .r-tl-dur { position: absolute; top: 0; height: 20px; padding: 0 7px; border-radius: var(--radius-chip); white-space: nowrap;
  font: var(--type-num-small); line-height: 20px; font-variant-numeric: tabular-nums; }
.r-tl-time { background: var(--iris); color: var(--text-on-iris); }
.r-tl-dur { background: var(--win-fill); color: var(--win-text); font-weight: 600; }
.r-tl-ruler { position: relative; height: 18px; }
.r-tick { position: absolute; bottom: 3px; width: 1px; height: 4px; background: var(--win-line-strong); }
.r-tick[data-major] { height: 8px; }
.r-tick-label { position: absolute; top: 0; margin-left: 4px; font: var(--type-num-small); color: var(--win-text-3); }
.r-tl-track { position: relative; height: 88px; touch-action: none; }
.r-tl-strip { position: relative; display: flex; height: 52px; overflow: hidden; border-radius: var(--radius-thumb); background: var(--win-sunken); }
.r-tl-thumb { flex: none; height: 100%; overflow: hidden; box-shadow: inset calc(-1 * var(--hairline)) 0 0 var(--win-line); }
.r-tl-thumb > img { display: block; width: 100%; height: 100%; object-fit: cover; -webkit-user-drag: none; }
.r-tl-wave { position: relative; height: 32px; margin-top: 4px; overflow: hidden; border-radius: var(--radius-thumb); background: var(--win-fill); }
.r-tl-wave > svg { position: absolute; inset: 0; width: 100%; height: 100%; }
.r-tl-wave rect { fill: var(--win-iris-text); }
.r-tl-shade { position: absolute; top: 0; bottom: 0; background: var(--dim); pointer-events: none; }
.r-tl-enc { position: absolute; top: 0; bottom: 0; background: var(--iris-soft); pointer-events: none; }
.r-tl-range { position: absolute; top: 0; height: 88px; pointer-events: none; box-shadow: inset 0 2px 0 var(--iris), inset 0 -2px 0 var(--iris); }
.r-tl-handle {
  position: absolute; top: 0; display: grid; place-items: center; width: 14px; height: 88px; margin: 0; padding: 0; border: 0;
  background: var(--iris); color: var(--text-on-iris); -webkit-appearance: none; appearance: none; cursor: ew-resize; touch-action: none;
  transition: background-color var(--dur-fast) var(--ease-out);
}
.r-tl-handle[data-edge="start"] { border-radius: var(--radius-thumb) 0 0 var(--radius-thumb); }
.r-tl-handle[data-edge="end"] { border-radius: 0 var(--radius-thumb) var(--radius-thumb) 0; }
.r-tl-handle::before { content: ""; width: 2px; height: 22px; border-radius: 1px; background: currentColor; }
.r-tl-handle::after { content: ""; position: absolute; top: 0; bottom: 0; left: -7px; right: -7px; } /* 28 px hit area */
.r-tl-handle:hover { background: var(--iris-hover); }
.r-tl-handle:active { background: var(--iris-press); }
.r-tl-handle:focus { outline: 0; }
.r-tl-handle:focus-visible { outline: 2px solid var(--win-focus); outline-offset: 2px; }
.r-tl-handle:disabled { cursor: default; background: var(--iris); }
.r-tl-play { position: absolute; top: 24px; width: 2px; height: 106px; margin-left: -1px; border-radius: 1px; background: var(--handle); box-shadow: var(--shadow-handle); pointer-events: none; }
.r-tl-play::before { content: ""; position: absolute; left: 50%; top: -2px; width: 12px; height: 12px; margin-left: -6px; border-radius: 50%; background: var(--handle); box-shadow: var(--shadow-handle); }
@media (prefers-reduced-motion: reduce) {
  .r-tl-handle { transition: none; }
}
</style>
