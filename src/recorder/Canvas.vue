<script setup lang="ts">
// The recorded canvas (`rec-canvas`): a click-through window that covers the recorded rectangle
// exactly and is part of the file, so it draws nothing but what belongs to the video. A click
// draws the ring (36 points rippling to 58 over 520 ms, in the colour and size Settings › Recording
// chose). Shortcut keys show as key caps that fade 1.2 s after the last one; they sit at their
// place (bottom centre by default) and move to the opposite side while the pointer or the webcam
// bubble comes within 24 points of it, unless that side is taken too. Reduce Motion drops the
// ripple and the slide.
//
// Drawing and the spotlight (B19) are drawn here too, so they are in the file. In drawing mode the
// core lets this window take the mouse: a drag draws with the pen (4 points), the highlighter (18
// points, 45 % see-through), the arrow or the rectangle in the chosen ink, ⇧ makes a straight line,
// a 45° arrow or a square, and a ring in the ink marks the pointer's tip while the cursor stays an
// arrow. With the fade on, every drawing goes together 3 s after the last stroke, in 600 ms. The
// spotlight dims the area outside an 80-point circle around the pointer; ⌥ with the wheel resizes
// it between 40 and 240 points. The drawings go when the take ends.
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, shallowRef, watch } from "vue";

import { drawObject } from "../annotate/draw";
import type { AnnotationObject, Point } from "../lib/annotate";
import type { LogicalPoint } from "../lib/api/capture";
import {
  onRecordBubble,
  onRecordCanvas,
  onRecordClick,
  onRecordInk,
  onRecordKeys,
  onRecordPointer,
  onRecordPresent,
  record_bubble_state,
  record_canvas_state,
  record_present,
  record_present_state,
  type BubbleState,
  type CanvasConfig,
  type InkColor,
  type InkTool,
  type PresentState,
  type RingColor,
} from "../lib/api/record";
import { placeKeys, toLocal, type Rect } from "./geometry";

/** How long a ring lives: the ripple's 520 ms and a little room. */
const RING_MS = 560;
/** How long the key caps stay after the last key. */
const KEYS_MS = 1200;
/** The ring's diameter for each size. */
const RING_SIZE = { small: 36, large: 54 } as const;
const RING_COLOR: Record<RingColor, string> = {
  yellow: "var(--ink-yellow)",
  red: "var(--ink-red)",
  blue: "var(--ink-blue)",
  white: "var(--ink-white)",
};

/** Each tool's width in points (design B19 Ortak). */
const INK_WIDTH: Record<InkTool, number> = { pen: 4, highlighter: 18, arrow: 4, rect: 4 };
/** The fade waits this long after the last stroke, then takes `FADE_MS`. */
const FADE_WAIT = 3000;
const FADE_MS = 600;
const SPOT_MIN = 40;
const SPOT_MAX = 240;
/** Points of radius per point the wheel scrolls. */
const WHEEL_STEP = 0.25;

const config = shallowRef<CanvasConfig | null>(null);
const bubble = shallowRef<BubbleState | null>(null);
const pointer = ref<LogicalPoint | null>(null);
const rings = ref<{ id: number; x: number; y: number }[]>([]);
const caps = ref<string[]>([]);
const keysShown = ref(false);
const keysEl = ref<HTMLElement | null>(null);
const keysSize = reactive({ width: 0, height: 0 });

let ringId = 0;
let keysTimer = 0;
const stops: (() => void)[] = [];

// ------------------------------------------------------------------------- drawing state

const present = shallowRef<PresentState | null>(null);
const inkEl = ref<HTMLCanvasElement | null>(null);
const fading = ref(false);
/** The spot's radius as drawn: the wheel moves it at once and tells the core a moment later. */
const radius = ref(80);

/** A stroke in local points while the button is down. */
type Draft = { id: string; tool: InkTool; ink: InkColor; from: Point; to: Point; points: Point[]; straight: boolean };

/** The finished strokes in local points; drawn into the canvas, not kept reactive. */
let strokes: AnnotationObject[] = [];
let draft: Draft | null = null;
let strokeId = 0;
let paintFrame = 0;
let fadeTimer = 0;
let goneTimer = 0;
let radiusTimer = 0;

const interactive = computed(() => !!present.value && (present.value.drawing || present.value.spotlight));

const spot = computed(() => {
  const p = pointer.value;
  if (!present.value?.spotlight || !p) return null;
  const r = radius.value;
  return {
    x: p.x,
    y: p.y,
    r,
    dim: { background: `radial-gradient(circle ${r}px at ${p.x}px ${p.y}px, transparent ${r - 0.5}px, var(--dim) ${r}px)` },
  };
});

const brush = computed(() => {
  const p = pointer.value;
  if (!present.value?.drawing || !p) return null;
  return { left: `${p.x}px`, top: `${p.y}px`, "--r-ink": `var(--ink-${present.value.ink})` };
});

const origin = computed(() => config.value?.rect ?? { x: 0, y: 0, width: window.innerWidth, height: window.innerHeight });

const ringStyle = computed(() => {
  const c = config.value?.clicks;
  const size = RING_SIZE[c?.size ?? "small"];
  return { "--r-ring": RING_COLOR[c?.color ?? "yellow"], "--r-ring-size": `${size}px`, "--r-ring-reach": `${Math.round((size * 11) / 36)}px` };
});

/** What the keystroke overlay keeps away from, in local points. */
const near = computed<Rect[]>(() => {
  const list: Rect[] = [];
  const p = pointer.value;
  if (p) list.push({ x: p.x, y: p.y, width: 0, height: 0 });
  const b = bubble.value;
  if (b?.visible) list.push(toLocal(b.rect, origin.value));
  return list;
});

const keysAt = computed(() => {
  const c = config.value;
  if (!c) return { left: "0px", top: "0px" };
  const place = placeKeys(origin.value, keysSize, c.keystrokes.position, near.value, c.liftBottom, c.dropTop);
  return { left: `${Math.round(place.x)}px`, top: `${Math.round(place.y)}px` };
});

function measureKeys(): void {
  const el = keysEl.value;
  if (!el) return;
  keysSize.width = el.offsetWidth;
  keysSize.height = el.offsetHeight;
}

function click(p: LogicalPoint): void {
  const c = config.value;
  if (!c?.clicks.enabled) return;
  const id = ++ringId;
  rings.value.push({ id, x: p.x - origin.value.x, y: p.y - origin.value.y });
  window.setTimeout(() => {
    rings.value = rings.value.filter((ring) => ring.id !== id);
  }, RING_MS);
}

async function keys(next: { caps: string[] }): Promise<void> {
  if (!config.value?.keystrokes.enabled || !next.caps.length) return;
  caps.value = next.caps;
  keysShown.value = true;
  window.clearTimeout(keysTimer);
  keysTimer = window.setTimeout(() => (keysShown.value = false), KEYS_MS);
  await nextTick();
  measureKeys();
}

// ---------------------------------------------------------------------------------- the ink

function shapeOf(d: Draft): AnnotationObject {
  const color = { kind: "ink", name: d.ink } as const;
  const width = INK_WIDTH[d.tool];
  const base = { id: d.id, hidden: false, locked: false };
  const to = d.straight ? straighten(d) : d.to;
  switch (d.tool) {
    case "pen":
      return { ...base, kind: "pen", points: d.straight ? [d.from, to] : d.points, stroke: { color, width } };
    case "highlighter":
      return { ...base, kind: "highlighter", points: d.straight ? [d.from, to] : d.points, color, width };
    case "arrow":
      return { ...base, kind: "arrow", from: d.from, to, stroke: { color, width } };
    case "rect":
      return {
        ...base,
        kind: "rectangle",
        rect: { x: Math.min(d.from.x, to.x), y: Math.min(d.from.y, to.y), width: Math.abs(to.x - d.from.x), height: Math.abs(to.y - d.from.y) },
        stroke: { color, width },
        fill: null,
      };
  }
}

/** ⇧: the rectangle becomes a square, a line and an arrow keep to 45° steps. */
function straighten(d: Draft): Point {
  const dx = d.to.x - d.from.x;
  const dy = d.to.y - d.from.y;
  if (d.tool === "rect") {
    const side = Math.max(Math.abs(dx), Math.abs(dy));
    return { x: d.from.x + Math.sign(dx || 1) * side, y: d.from.y + Math.sign(dy || 1) * side };
  }
  const step = Math.PI / 4;
  const angle = Math.round(Math.atan2(dy, dx) / step) * step;
  const length = Math.hypot(dx, dy);
  return { x: d.from.x + Math.cos(angle) * length, y: d.from.y + Math.sin(angle) * length };
}

/** A stroke in the canvas's pixels: the kit's drawing works in pixels with the scale beside them,
 * so shadows, which a transform does not scale, land as they do in the editor. */
function inPixels(o: AnnotationObject, k: number): AnnotationObject {
  const at = (q: Point) => ({ x: q.x * k, y: q.y * k });
  switch (o.kind) {
    case "pen":
      return { ...o, points: o.points.map(at), stroke: { ...o.stroke, width: o.stroke.width * k } };
    case "highlighter":
      return { ...o, points: o.points.map(at), width: o.width * k };
    case "arrow":
      return { ...o, from: at(o.from), to: at(o.to), stroke: { ...o.stroke, width: o.stroke.width * k } };
    case "rectangle":
      return { ...o, rect: { x: o.rect.x * k, y: o.rect.y * k, width: o.rect.width * k, height: o.rect.height * k }, stroke: { ...o.stroke, width: o.stroke.width * k } };
    default:
      return o;
  }
}

function paint(): void {
  paintFrame = 0;
  const canvas = inkEl.value;
  const ctx = canvas?.getContext("2d");
  if (!canvas || !ctx) return;
  const k = window.devicePixelRatio || 1;
  const width = Math.round(window.innerWidth * k);
  const height = Math.round(window.innerHeight * k);
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  ctx.clearRect(0, 0, width, height);
  for (const stroke of strokes) drawObject(ctx, inPixels(stroke, k), k);
  if (draft) drawObject(ctx, inPixels(shapeOf(draft), k), k);
}

function schedulePaint(): void {
  if (!paintFrame) paintFrame = window.requestAnimationFrame(paint);
}

function stopFade(): void {
  window.clearTimeout(fadeTimer);
  window.clearTimeout(goneTimer);
  fading.value = false;
}

/** With the fade on, every drawing goes together 3 s after the last stroke. */
function scheduleFade(): void {
  stopFade();
  if (!present.value?.fade || draft || !strokes.length) return;
  fadeTimer = window.setTimeout(() => {
    fading.value = true;
    goneTimer = window.setTimeout(() => {
      strokes = [];
      fading.value = false;
      schedulePaint();
    }, FADE_MS);
  }, FADE_WAIT);
}

function wipe(): void {
  stopFade();
  strokes = [];
  draft = null;
  schedulePaint();
}

function undo(): void {
  if (draft) draft = null;
  else strokes.pop();
  if (!strokes.length) stopFade();
  schedulePaint();
}

const local = (event: PointerEvent): Point => ({ x: event.clientX, y: event.clientY });

function onPointerDown(event: PointerEvent): void {
  const p = present.value;
  if (!p?.drawing || event.button !== 0) return;
  event.preventDefault();
  (event.currentTarget as Element).setPointerCapture(event.pointerId);
  stopFade();
  const at = local(event);
  draft = { id: `ink-${++strokeId}`, tool: p.tool, ink: p.ink, from: at, to: at, points: [at], straight: event.shiftKey };
  pointer.value = at;
  schedulePaint();
}

function onPointerMove(event: PointerEvent): void {
  if (present.value?.drawing) pointer.value = local(event);
  if (!draft) return;
  const events = typeof event.getCoalescedEvents === "function" ? event.getCoalescedEvents() : [];
  for (const e of events.length ? events : [event]) {
    const at = local(e);
    const last = draft.points[draft.points.length - 1];
    if (Math.abs(at.x - last.x) + Math.abs(at.y - last.y) >= 0.5) draft.points.push(at);
  }
  draft.to = local(event);
  draft.straight = event.shiftKey;
  schedulePaint();
}

function onPointerUp(event: PointerEvent): void {
  if (!draft) return;
  draft.to = local(event);
  draft.straight = event.shiftKey;
  const shape = shapeOf(draft);
  const size = Math.hypot(draft.to.x - draft.from.x, draft.to.y - draft.from.y);
  // A click leaves a dot with the pen and the highlighter; an arrow or a box needs a drag.
  if (draft.tool === "pen" || draft.tool === "highlighter" || size >= 4) strokes.push(shape);
  draft = null;
  schedulePaint();
  scheduleFade();
}

function onWheel(event: WheelEvent): void {
  if (!present.value?.spotlight || !event.altKey) return;
  event.preventDefault();
  const delta = Math.abs(event.deltaY) >= Math.abs(event.deltaX) ? event.deltaY : event.deltaX;
  radius.value = Math.min(SPOT_MAX, Math.max(SPOT_MIN, radius.value - delta * WHEEL_STEP));
  window.clearTimeout(radiusTimer);
  radiusTimer = window.setTimeout(() => {
    radiusTimer = 0;
    void record_present({ radius: radius.value }).catch((error) => console.error("The spotlight's size was refused.", error));
  }, 150);
}

function applyPresent(next: PresentState | null): void {
  const fadeWas = present.value?.fade ?? false;
  present.value = next;
  if (!next) {
    wipe();
    return;
  }
  if (!radiusTimer) radius.value = next.radius;
  if (!next.drawing && draft) {
    draft = null;
    schedulePaint();
  }
  if (next.fade !== fadeWas) scheduleFade();
}

// The drawings belong to the take, so they go with its canvas configuration.
watch(config, (next) => {
  if (!next) wipe();
});

const observer = new ResizeObserver(measureKeys);

onMounted(async () => {
  let heardConfig = false;
  let heardBubble = false;
  try {
    stops.push(
      await onRecordCanvas((next) => {
        heardConfig = true;
        config.value = next;
      }),
    );
    stops.push(await onRecordClick(click));
    stops.push(await onRecordKeys((next) => void keys(next)));
    stops.push(
      await onRecordPointer((p) => {
        pointer.value = p ? { x: p.x - origin.value.x, y: p.y - origin.value.y } : null;
      }),
    );
    stops.push(
      await onRecordBubble((next) => {
        heardBubble = true;
        bubble.value = next;
      }),
    );
    let heardPresent = false;
    stops.push(
      await onRecordPresent((next) => {
        heardPresent = true;
        applyPresent(next);
      }),
    );
    stops.push(await onRecordInk((command) => (command.kind === "undo" ? undo() : wipe())));
    const [first, firstBubble, firstPresent] = await Promise.all([record_canvas_state(), record_bubble_state(), record_present_state()]);
    if (!heardConfig) config.value = first;
    if (!heardBubble) bubble.value = firstBubble;
    if (!heardPresent) applyPresent(firstPresent);
  } catch (error) {
    console.error("The recording canvas could not load.", error);
  }
  if (keysEl.value) observer.observe(keysEl.value);
  window.addEventListener("resize", schedulePaint);
});

onUnmounted(() => {
  window.clearTimeout(keysTimer);
  window.clearTimeout(radiusTimer);
  window.cancelAnimationFrame(paintFrame);
  window.removeEventListener("resize", schedulePaint);
  stopFade();
  observer.disconnect();
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div
    class="r-canvas"
    :style="ringStyle"
    :data-input="interactive ? 'true' : undefined"
    aria-hidden="true"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
    @wheel="onWheel"
  >
    <canvas ref="inkEl" class="r-ink" :data-fading="fading ? 'true' : undefined" />
    <template v-if="spot">
      <div class="r-spot" :style="spot.dim" />
      <svg class="r-spot-ring"><circle :cx="spot.x" :cy="spot.y" :r="spot.r" /></svg>
    </template>
    <div v-for="ring in rings" :key="ring.id" class="r-click" :style="{ left: `${ring.x}px`, top: `${ring.y}px` }" />
    <div
      ref="keysEl"
      class="g-glass r-keys"
      :data-size="config?.keystrokes.size ?? 'medium'"
      :data-shown="keysShown ? 'true' : 'false'"
      :style="keysAt"
    >
      <kbd v-for="(cap, i) in caps" :key="i" class="r-key">{{ cap }}</kbd>
    </div>
    <span v-if="brush" class="r-brush" :style="brush" />
  </div>
</template>

<style>
.r-canvas { position: fixed; inset: 0; overflow: hidden; pointer-events: none; }
/* Drawing, or the spotlight with ⌥ held: the core lets the window take the mouse, and the cursor
   stays the arrow. */
.r-canvas[data-input] { pointer-events: auto; cursor: default; touch-action: none; }

/* the drawings, in device pixels; the fade takes them all at once */
.r-ink { position: absolute; inset: 0; width: 100%; height: 100%; }
.r-ink[data-fading] { opacity: 0; transition: opacity 600ms var(--ease-out); }

/* the spotlight: the area outside the circle dims, and a white line marks its edge */
.r-spot { position: absolute; inset: 0; pointer-events: none; }
.r-spot-ring { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; pointer-events: none; }
.r-spot-ring > circle { fill: none; stroke: #fff; stroke-opacity: 0.85; stroke-width: 1.5; }

/* the brush ring at the pointer's tip, in the ink, with a dot at its centre */
.r-brush {
  position: absolute; width: 16px; height: 16px; margin: -8px 0 0 -8px; border-radius: 50%; pointer-events: none;
  box-shadow: inset 0 0 0 2px var(--r-ink), 0 0 0 1px rgba(255, 255, 255, 0.9), 0 1px 2px rgba(0, 0, 0, 0.35);
}
.r-brush::after {
  content: ""; position: absolute; left: 50%; top: 50%; width: 3px; height: 3px; margin: -1.5px 0 0 -1.5px;
  border-radius: 50%; background: var(--r-ink);
}

/* the click ring: the colour is the user's ink, with a white line and a soft glow around it */
.r-click {
  position: absolute; width: var(--r-ring-size); height: var(--r-ring-size);
  margin: calc(var(--r-ring-size) / -2) 0 0 calc(var(--r-ring-size) / -2); border-radius: 50%;
  background: color-mix(in srgb, var(--r-ring) 26%, transparent);
  box-shadow: 0 0 0 2px var(--r-ring), 0 0 0 3.5px var(--selection-inner);
  animation: r-click 520ms var(--ease-out) both;
}
.r-click::after { content: ""; position: absolute; inset: calc(-1 * var(--r-ring-reach)); border-radius: 50%;
  box-shadow: 0 0 0 1.5px color-mix(in srgb, var(--r-ring) 45%, transparent); }
@keyframes r-click { from { transform: scale(0.6); opacity: 1; } 70% { opacity: 1; } to { transform: scale(1.15); opacity: 0; } }
@keyframes r-click-still { 0%, 70% { opacity: 1; } to { opacity: 0; } }

/* the keystroke overlay: 36, 44 or 56 px key caps on glass. Faded out, it is also hidden once the
   fade ends (as the kit's tips are): its glass is a composited backdrop layer, which opacity 0
   alone leaves in the recorded window's layers for the whole take. */
.r-keys {
  position: absolute; display: inline-flex; align-items: center; gap: var(--space-6); padding: var(--space-6);
  border-radius: var(--radius-card); white-space: nowrap;
  transition: opacity var(--dur-base) var(--ease-out), top var(--dur-move) var(--ease-out), visibility 0s;
}
.r-keys[data-shown="false"] { opacity: 0; visibility: hidden; transition-delay: 0s, 0s, var(--dur-base); }
.r-key {
  display: inline-grid; place-items: center; min-width: var(--r-cap); height: var(--r-cap); padding: 0 var(--r-cap-pad);
  border-radius: var(--r-cap-radius); background: var(--control-fill); box-shadow: var(--surface-kbd-shadow);
  color: var(--text); font: 600 var(--r-cap-font) / 1 var(--font); white-space: nowrap;
}
.r-keys[data-size="small"] { --r-cap: 36px; --r-cap-pad: 10px; --r-cap-radius: 10px; --r-cap-font: 16px; }
.r-keys[data-size="medium"] { --r-cap: 44px; --r-cap-pad: 12px; --r-cap-radius: 12px; --r-cap-font: 20px; }
.r-keys[data-size="large"] { --r-cap: 56px; --r-cap-pad: 15px; --r-cap-radius: 14px; --r-cap-font: 26px; }

@media (prefers-reduced-motion: reduce) {
  .r-click { animation-name: r-click-still; }
  .r-ink[data-fading] { transition: none; }
  .r-keys { transition: opacity var(--dur-base) var(--ease-out), visibility 0s; }
  .r-keys[data-shown="false"] { transition-delay: 0s, var(--dur-base); }
}
</style>
