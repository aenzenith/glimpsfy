<script setup lang="ts">
// The recording target (`rec-target`): a transparent panel over one whole display, in the
// display's local points. During the setup it draws what the take will record and lets the user
// change it: an area with its edge, handles and size label (drag inside to move it, a handle to
// resize it, outside to draw a new one), the windows to pick from, or the whole display with its
// chip. Picking a window, the window under the pointer carries the outline and the chosen one is
// the clear part of the dim; the pointer comes from the core, because this page gets no mouse
// moves while the bar holds the keyboard. During the countdown it draws the red frame, the ring and Cancel, and it owns the keyboard
// (Esc cancels, Return starts at once). During the take it draws only the frame, dashed and
// neutral while paused; the core makes the window click-through then. While the webcam bubble is
// dragged it shows the four corner slots it can snap to.
import { computed, defineComponent, h, onMounted, onUnmounted, ref, watch, watchEffect } from "vue";

import GIcon from "../components/GIcon.vue";
import {
  onRecordBubble,
  onRecordPointer,
  onRecordSetup,
  onRecordState,
  record_bubble_state,
  record_countdown_cancel,
  record_countdown_skip,
  record_setup_close,
  record_setup_state,
  record_setup_update,
  record_state,
  type BubbleState,
  type RecordingState,
  type RecordSetup,
  type SetupWindow,
} from "../lib/api/record";
import { t } from "../lib/i18n";
import { box, clamp, handleAt, HANDLES, inside, RESIZE_CURSORS, toLocal, type Point, type Rect } from "./geometry";
import { reducedMotion } from "./format";

/** The smallest area the setup accepts, in points. */
const MIN_AREA = 16;
/** A handle's hit radius: half of its 28 px hit area. */
const HANDLE_HIT = 14;
/** A press that moves less than this is a click, not a new area. */
const CLICK_SLOP = 3;
/** The corner radius of a window target, as macOS draws windows. */
const WINDOW_RADIUS = 10;
/** The bar's height and its gap to the area, to guess whether the core put the bar above it. */
const BAR_ROOM = 8 + 40 + 8;

const setup = ref<RecordSetup | null>(null);
const state = ref<RecordingState>({ kind: "idle" });
const bubble = ref<BubbleState | null>(null);
/** The area while the user drags it, before the core has the final one. */
const draft = ref<Rect | null>(null);
const pointer = ref<Point | null>(null);
const cursor = ref("default");
const progress = ref(0);

type Grab =
  | { kind: "move"; offset: Point; origin: Rect }
  | { kind: "resize"; handle: [number, number]; origin: Rect }
  | { kind: "new"; start: Point; moved: boolean };
let grab: Grab | null = null;
let chipSent = false;
let pending: Rect | null = null;
let frameRequest = 0;
let ringFrame = 0;
let tick = { remaining: -1, at: 0 };
const stops: (() => void)[] = [];

// ------------------------------------------------------------------------------- what shows

const phase = computed(() => {
  switch (state.value.kind) {
    case "countdown":
      return "countdown";
    case "recording":
      return "recording";
    case "paused":
      return "paused";
    case "finishing":
      return "finishing";
    default:
      return setup.value ? "setup" : "none";
  }
});

const mode = computed(() => setup.value?.mode ?? "area");
const bounds = computed<Rect>(() => {
  const f = setup.value?.display.frame;
  return { x: 0, y: 0, width: f?.width ?? window.innerWidth, height: f?.height ?? window.innerHeight };
});
/** The menu bar's height and the Dock's top, in local points. */
const visible = computed<Rect>(() => {
  const d = setup.value?.display;
  return d ? toLocal(d.visible, d.frame) : bounds.value;
});

const area = computed<Rect | null>(() => draft.value ?? setup.value?.area ?? null);

/** What the take records, in local points. */
const target = computed<Rect | null>(() => {
  const s = setup.value;
  if (!s) return null;
  if (s.mode === "area") return area.value;
  if (s.mode === "window") return s.window?.rect ?? null;
  return bounds.value;
});

/** The red frame; on a whole display it sits 2 points inside the edge. */
const frameBox = computed<Rect | null>(() => {
  const r = target.value;
  if (!r) return null;
  if (mode.value !== "display") return r;
  return { x: 2, y: 2, width: r.width - 4, height: r.height - 4 };
});

const hovered = computed<SetupWindow | null>(() => {
  const s = setup.value;
  const p = pointer.value;
  if (!s || s.mode !== "window" || !p || phase.value !== "setup") return null;
  return s.windows.find((w) => inside(w.rect, p)) ?? null;
});

const cornerOf = (w: SetupWindow | null | undefined) => w?.cornerRadius ?? WINDOW_RADIUS;
const radiusOf = (w: SetupWindow | null | undefined) => `${cornerOf(w)}px`;

/** The clear part of the dim and the radius of its corners, or null where nothing is dimmed. The
 * setup dims around the chosen window or the area, and all of the display before there is one;
 * the countdown dims around the target unless it is the whole display. */
const dim = computed<{ hole: Rect | null; radius: number } | null>(() => {
  const s = setup.value;
  if (!s || s.mode === "display") return null;
  const radius = s.mode === "window" ? cornerOf(s.window) : 0;
  if (phase.value === "setup") return { hole: s.mode === "window" ? (s.window?.rect ?? null) : area.value, radius };
  if (phase.value === "countdown" && target.value) return { hole: target.value, radius };
  return null;
});

const handles = computed(() => {
  const r = area.value;
  if (!r || phase.value !== "setup" || mode.value !== "area") return [];
  return HANDLES.map(([fx, fy, kind]) => ({ key: `${fx},${fy}`, kind, x: r.x + fx * r.width, y: r.y + fy * r.height }));
});

/** The size label: 6 points above the area's top-left corner, or inside it when the menu bar or
 * the bar (which the core puts above the area when there is no room below) takes that place. */
const sizeLabel = computed(() => {
  const s = setup.value;
  const r = area.value;
  if (!s || !r || mode.value !== "area" || phase.value !== "setup") return null;
  const barAbove = r.y + r.height + BAR_ROOM > bounds.value.height;
  const above = r.y - 30 >= visible.value.y + 8 && !barAbove;
  return {
    width: s.outputSize.width,
    height: s.outputSize.height,
    style: { left: `${above ? r.x : r.x + 6}px`, top: `${above ? r.y - 30 : r.y + 6}px` },
  };
});

const slots = computed(() => {
  const b = bubble.value;
  const s = setup.value;
  if (!b?.dragging || !s) return [];
  return b.slots.map((slot) => ({ corner: slot.corner, rect: toLocal(slot.rect, s.display.frame), near: slot.corner === b.near }));
});

const countdown = computed(() => (state.value.kind === "countdown" ? state.value : null));
const countAt = computed(() => {
  const r = target.value;
  return r ? { left: `${r.x + r.width / 2}px`, top: `${r.y + r.height / 2 - 56}px` } : null;
});

const tagAt = computed(() => {
  const r = target.value;
  if (!r || mode.value === "display") return null;
  return { left: `${r.x}px`, top: `${Math.max(visible.value.y + 4, r.y - 26)}px` };
});

// ------------------------------------------------------------------------------------ the dim
//
// The dim is four bands around the hole and, around a window, four corner pieces, as the capture
// overlay draws it (`paintDim` in overlay/App.vue). Each is a box of one solid colour moved by its
// transform, which WebKit composites without a backing store, so dragging the area or picking
// another window repaints nothing. The kit's `.g-dim`, one spread shadow around the hole, repainted
// a layer the size of the display on every move. The pieces are written straight to the DOM after
// the render that mounts them.

const dimEl = ref<HTMLElement | null>(null);

/** The device pixels per point. A move to a display of another scale paints the dim again, or its
 * edges would sit off the new pixel grid and show seams. */
const scale = ref(window.devicePixelRatio || 1);
let scaleQuery: MediaQueryList | null = null;

function followScale(): void {
  scaleQuery?.removeEventListener("change", followScale);
  scale.value = window.devicePixelRatio || 1;
  scaleQuery = window.matchMedia(`(resolution: ${scale.value}dppx)`);
  scaleQuery.addEventListener("change", followScale);
}

const written = new WeakMap<HTMLElement, Record<string, string>>();

/** Sets one style property, and skips a write that would change nothing. */
function put(el: HTMLElement, name: string, value: string): void {
  let seen = written.get(el);
  if (!seen) written.set(el, (seen = {}));
  if (seen[name] === value) return;
  seen[name] = value;
  el.style.setProperty(name, value);
}

/** A box of `w` × `h` points at (`x`, `y`), moved by its transform. */
function setBox(el: HTMLElement, x: number, y: number, w: number, h: number): void {
  put(el, "transform", `translate(${x}px, ${y}px)`);
  put(el, "width", `${Math.max(0, w)}px`);
  put(el, "height", `${Math.max(0, h)}px`);
}

/** A coordinate in WebKit's layout units: a CSS length is cut to 1/64 point. */
const lu = (v: number) => Math.trunc(v * 64) / 64;

/** A coordinate on the device pixel grid, rounded half away from zero, the way WebKit snaps the
 * edges of a box: the kit's dim left its hole on these pixels, and the bands meet there with no
 * seam and nothing dimmed twice. */
function snap(v: number, d: number): number {
  const n = lu(v) * d;
  return (n < 0 ? -Math.round(-n) : Math.round(n)) / d;
}

/** The dim over the `width` × `height` page around `hole`, or over all of it without one: four
 * bands, then the corner pieces of a rounded hole when the page has them. */
function paintDim(host: HTMLElement, hole: Rect | null, radius: number, width: number, height: number, d: number): void {
  const [top, bottom, left, right, tl, tr, bl, br] = Array.from(host.children) as HTMLElement[];
  let l = 0;
  let t = 0;
  let r = 0;
  let b = 0;
  if (hole) {
    l = snap(hole.x, d);
    t = snap(hole.y, d);
    r = snap(lu(hole.x) + lu(hole.width), d);
    b = snap(lu(hole.y) + lu(hole.height), d);
  }
  const cl = clamp(l, 0, width);
  const cr = clamp(r, cl, width);
  const ct = clamp(t, 0, height);
  const cb = clamp(b, ct, height);
  setBox(top, 0, 0, width, ct);
  setBox(bottom, 0, cb, width, height - cb);
  setBox(left, 0, ct, cl, cb - ct);
  setBox(right, cr, ct, width - cr, cb - ct);
  if (!tl || !tr || !bl || !br) return;
  // CSS shrinks radii that do not fit the box. A piece spans whole device pixels, so its layer is
  // never resampled along the edges it shares with the bands; its circle stays where the hole's
  // corner is (the `.r-dim` rules below).
  const k = radius > 0 ? Math.min(radius, (r - l) / 2, (b - t) / 2) : 0;
  const s = k > 0 ? Math.ceil(k * d) / d : 0;
  put(host, "--r-dim-r", `${k}px`);
  put(host, "--r-dim-far", `${s - 2 * k}px`);
  setBox(tl, l, t, s, s);
  setBox(tr, r - s, t, s, s);
  setBox(bl, l, b - s, s, s);
  setBox(br, r - s, b - s, s, s);
}

// Post-flush: it runs after the render that mounts the dim or its corners, in the same task as the
// change, so the dim never shows in a stale place.
watchEffect(
  () => {
    const host = dimEl.value;
    const v = dim.value;
    if (host && v) paintDim(host, v.hole, v.radius, bounds.value.width, bounds.value.height, scale.value);
  },
  { flush: "post" },
);

// ------------------------------------------------------------------------------- countdown

/** The ring fills smoothly between the core's whole-second ticks. With Reduce Motion it steps
 * with the ticks, and once a second's share is full it waits for the next tick: neither needs a
 * frame, and the watch below starts the frames again on the next tick. */
function runRing(): void {
  cancelAnimationFrame(ringFrame);
  ringFrame = 0;
  const c = countdown.value;
  if (!c) return;
  const total = Math.max(1, c.totalSeconds);
  const done = total - c.remainingSeconds;
  const still = reducedMotion();
  const part = still ? 0 : Math.min(1, (performance.now() - tick.at) / 1000);
  progress.value = clamp(((done + part) / total) * 100, 0, 100);
  if (!still && part < 1) ringFrame = requestAnimationFrame(runRing);
}

/** The countdown's ring as a component of its own, which reads `progress` itself: a new share on
 * every frame then draws the ring again, not the whole target with its dim, frame and handles. */
const CountdownRing = defineComponent(
  () => () =>
    h("svg", { class: "g-ring", viewBox: "0 0 22 22", style: { "--progress": progress.value }, "aria-hidden": "true" }, [
      h("circle", { cx: "11", cy: "11", r: "8", pathLength: "100" }),
      h("circle", { cx: "11", cy: "11", r: "8", pathLength: "100" }),
    ]),
);

watch(countdown, (c) => {
  if (!c) {
    cancelAnimationFrame(ringFrame);
    tick = { remaining: -1, at: 0 };
    return;
  }
  if (c.remainingSeconds !== tick.remaining) tick = { remaining: c.remainingSeconds, at: performance.now() };
  runRing();
});

// ---------------------------------------------------------------------------- the setup

function report(error: unknown): void {
  console.error("The recording setup could not be updated.", error);
}

function update(change: Parameters<typeof record_setup_update>[0]): Promise<void> {
  return record_setup_update(change).then(
    (next) => void (setup.value = next),
    (error) => report(error),
  );
}

/** At most one area update per animation frame while the user drags. */
function sendSoon(rect: Rect): void {
  pending = rect;
  if (frameRequest) return;
  frameRequest = requestAnimationFrame(() => {
    frameRequest = 0;
    const next = pending;
    pending = null;
    if (next) void record_setup_update({ rect: next }).catch(report);
  });
}

async function sendFinal(rect: Rect): Promise<void> {
  cancelAnimationFrame(frameRequest);
  frameRequest = 0;
  pending = null;
  try {
    setup.value = await record_setup_update({ rect });
  } catch (error) {
    report(error);
  } finally {
    if (!grab) draft.value = null;
  }
}

function point(e: MouseEvent): Point {
  return { x: clamp(e.clientX, 0, bounds.value.width), y: clamp(e.clientY, 0, bounds.value.height) };
}

const round = (r: Rect): Rect => ({ x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) });

/** A new area from the press to the pointer, at least the minimum on each side and inside the
 * display. */
function drawn(start: Point, p: Point): Rect {
  const W = bounds.value.width;
  const H = bounds.value.height;
  const w = Math.max(MIN_AREA, Math.abs(p.x - start.x));
  const h = Math.max(MIN_AREA, Math.abs(p.y - start.y));
  const x = clamp(p.x >= start.x ? start.x : start.x - w, 0, W - w);
  const y = clamp(p.y >= start.y ? start.y : start.y - h, 0, H - h);
  return round({ x, y, width: Math.min(w, W), height: Math.min(h, H) });
}

function resized(origin: Rect, [fx, fy]: [number, number], p: Point): Rect {
  let x0 = origin.x;
  let y0 = origin.y;
  let x1 = origin.x + origin.width;
  let y1 = origin.y + origin.height;
  if (fx === 0) x0 = clamp(p.x, 0, x1 - MIN_AREA);
  if (fx === 1) x1 = clamp(p.x, x0 + MIN_AREA, bounds.value.width);
  if (fy === 0) y0 = clamp(p.y, 0, y1 - MIN_AREA);
  if (fy === 1) y1 = clamp(p.y, y0 + MIN_AREA, bounds.value.height);
  return round({ x: x0, y: y0, width: x1 - x0, height: y1 - y0 });
}

function moved(origin: Rect, offset: Point, p: Point): Rect {
  return round({
    x: clamp(p.x - offset.x, 0, bounds.value.width - origin.width),
    y: clamp(p.y - offset.y, 0, bounds.value.height - origin.height),
    width: origin.width,
    height: origin.height,
  });
}

function seeChip(): void {
  if (chipSent || !setup.value?.chip || mode.value !== "display") return;
  chipSent = true;
  void update({ chipSeen: true });
}

function onMouseDown(e: MouseEvent): void {
  if (e.button !== 0 || phase.value !== "setup" || !setup.value) return;
  e.preventDefault();
  const p = point(e);
  if (mode.value === "display") return seeChip();
  if (mode.value === "window") {
    const w = setup.value.windows.find((candidate) => inside(candidate.rect, p));
    if (w && w.id !== setup.value.window?.id) void update({ windowId: w.id });
    return;
  }
  const r = area.value;
  const handle = r ? handleAt(r, p, HANDLE_HIT) : null;
  if (r && handle) grab = { kind: "resize", handle, origin: { ...r } };
  else if (r && inside(r, p)) grab = { kind: "move", offset: { x: p.x - r.x, y: p.y - r.y }, origin: { ...r } };
  else grab = { kind: "new", start: p, moved: false };
}

function hoverCursor(p: Point): string {
  if (phase.value !== "setup") return "default";
  if (mode.value === "window") return "pointer";
  if (mode.value !== "area") return "default";
  const r = area.value;
  if (!r) return "crosshair";
  const handle = handleAt(r, p, HANDLE_HIT);
  if (handle) return RESIZE_CURSORS[handle.join(",")] ?? "default";
  return inside(r, p) ? "move" : "crosshair";
}

function onMouseMove(e: MouseEvent): void {
  const p = point(e);
  pointer.value = p;
  if (phase.value !== "setup") return;
  seeChip();
  if (!grab) {
    cursor.value = hoverCursor(p);
    return;
  }
  let next: Rect;
  if (grab.kind === "move") next = moved(grab.origin, grab.offset, p);
  else if (grab.kind === "resize") next = resized(grab.origin, grab.handle, p);
  else {
    if (!grab.moved && Math.hypot(p.x - grab.start.x, p.y - grab.start.y) < CLICK_SLOP) return;
    grab.moved = true;
    next = drawn(grab.start, p);
  }
  draft.value = next;
  sendSoon(next);
}

function onMouseUp(e: MouseEvent): void {
  if (e.button !== 0 || !grab) return;
  const done = grab;
  grab = null;
  cursor.value = hoverCursor(point(e));
  // A click outside the area without a drag keeps the area as it was.
  if (done.kind === "new" && !done.moved) return;
  if (draft.value) void sendFinal(draft.value);
}

function onMouseLeave(): void {
  if (!grab) pointer.value = null;
}

function onKeyDown(e: KeyboardEvent): void {
  if (phase.value === "countdown") {
    if (e.key === "Escape") {
      e.preventDefault();
      record_countdown_cancel().catch(report);
    } else if (e.key === "Enter") {
      e.preventDefault();
      record_countdown_skip().catch(report);
    }
    return;
  }
  // The bar holds the keyboard during the setup; this panel only answers Esc if it got the focus.
  if (phase.value === "setup" && e.key === "Escape") {
    e.preventDefault();
    record_setup_close().catch(report);
  }
}

function cancelCountdown(): void {
  record_countdown_cancel().catch(report);
}

// ------------------------------------------------------------------------------- lifecycle

watch(
  () => setup.value?.chip,
  (chip) => {
    if (chip) chipSent = false;
  },
);

onMounted(async () => {
  window.addEventListener("mousemove", onMouseMove);
  window.addEventListener("mouseup", onMouseUp);
  window.addEventListener("keydown", onKeyDown);
  followScale();
  let heardSetup = false;
  let heardBubble = false;
  try {
    stops.push(
      await onRecordSetup((next) => {
        heardSetup = true;
        setup.value = next;
        if (!next) draft.value = null;
      }),
    );
    stops.push(await onRecordState((next) => void (state.value = next)));
    stops.push(
      await onRecordPointer((global) => {
        const frame = setup.value?.display.frame;
        if (mode.value !== "window" || !frame) return;
        pointer.value = global ? { x: global.x - frame.x, y: global.y - frame.y } : null;
      }),
    );
    stops.push(
      await onRecordBubble((next) => {
        heardBubble = true;
        bubble.value = next;
      }),
    );
    const [first, firstBubble, now] = await Promise.all([record_setup_state(), record_bubble_state(), record_state()]);
    if (!heardSetup) setup.value = first;
    if (state.value.kind === "idle") state.value = now;
    if (!heardBubble) bubble.value = firstBubble;
  } catch (error) {
    report(error);
  }
});

onUnmounted(() => {
  window.removeEventListener("mousemove", onMouseMove);
  window.removeEventListener("mouseup", onMouseUp);
  window.removeEventListener("keydown", onKeyDown);
  scaleQuery?.removeEventListener("change", followScale);
  cancelAnimationFrame(frameRequest);
  cancelAnimationFrame(ringFrame);
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div class="r-target-page" :style="{ cursor }" @mousedown="onMouseDown" @mouseleave="onMouseLeave">
    <!-- The dim, under everything else: bands above, below, left and right of the hole, then the
         corners of a window's rounded hole (`paintDim`). -->
    <div v-if="dim" ref="dimEl" class="r-dim" aria-hidden="true">
      <i data-band /><i data-band /><i data-band /><i data-band />
      <template v-if="dim.radius > 0"><i data-corner="tl" /><i data-corner="tr" /><i data-corner="bl" /><i data-corner="br" /></template>
    </div>

    <!-- the setup -->
    <template v-if="phase === 'setup' && setup">
      <template v-if="mode === 'display'">
        <div class="r-target-display" />
        <div v-if="setup.chip" class="g-chip r-chip">
          <GIcon name="display" />
          <span class="g-chip-text"
            ><small>{{ setup.display.name }}</small>{{ setup.display.pixelSize.width }} × {{ setup.display.pixelSize.height }}</span
          >
        </div>
      </template>
      <template v-else-if="mode === 'window'">
        <div v-if="hovered" class="r-win" :style="{ ...box(hovered.rect), '--r-radius': radiusOf(hovered) }" />
      </template>
      <template v-else>
        <template v-if="area">
          <div class="g-selection" :style="box(area)" />
          <span
            v-for="h in handles"
            :key="h.key"
            class="g-handle"
            :data-handle="h.kind ?? undefined"
            :style="{ left: `${h.x}px`, top: `${h.y}px` }"
          />
        </template>
        <div v-if="sizeLabel" class="g-size r-abs" :style="sizeLabel.style">
          {{ sizeLabel.width }}<span class="g-size-x">×</span>{{ sizeLabel.height }}
        </div>
      </template>
    </template>

    <!-- the countdown: the edge is already red -->
    <template v-else-if="phase === 'countdown' && countdown">
      <div v-if="frameBox" class="g-rec-frame" :style="box(frameBox)" />
      <div v-if="countAt" class="r-count" :style="countAt">
        <div class="g-countdown" role="timer" :aria-label="t('record.countdown.label', { n: countdown.remainingSeconds })">
          <span>{{ countdown.remainingSeconds }}</span>
          <CountdownRing />
        </div>
        <div class="g-glass r-cap">
          <button type="button" class="g-button" @mousedown.stop @click="cancelCountdown">
            {{ t("record.cancel") }}<kbd class="g-kbd">Esc</kbd>
          </button>
        </div>
      </div>
    </template>

    <!-- the take: no dim, the screen stays live -->
    <template v-else-if="phase === 'recording' && frameBox">
      <div class="g-rec-frame" :style="box(frameBox)" />
    </template>
    <template v-else-if="phase === 'paused' && frameBox">
      <div class="r-frame-paused" :style="box(frameBox)" />
      <span v-if="tagAt" class="g-tag r-abs" :style="tagAt"><GIcon name="pause" :size="12" />{{ t("record.paused") }}</span>
    </template>

    <!-- the bubble's corner slots while it is dragged -->
    <div
      v-for="slot in slots"
      :key="slot.corner"
      class="r-cam-slot"
      :data-near="slot.near ? '' : undefined"
      :style="box(slot.rect)"
    />
  </div>
</template>

<style>
.r-target-page { position: fixed; inset: 0; overflow: hidden; }
.r-abs { position: absolute; }

/* The dim as the capture overlay draws it (overlay.css): bands of one solid colour and, for a
   window's rounded hole, corner pieces that each clip the kit's own drawing, a spread shadow
   around a circle the size of the corner, so the curve is drawn exactly as the kit draws it. A
   piece may be a fraction of a pixel larger than the radius (--r-dim-far moves the circle back
   into the corner), and that margin lies inside the circle. */
.r-dim { position: absolute; inset: 0; pointer-events: none; }
.r-dim > i { position: absolute; left: 0; top: 0; width: 0; height: 0; will-change: transform; }
.r-dim > [data-band] { background: var(--dim); }
.r-dim > [data-corner] { overflow: hidden; }
.r-dim > [data-corner]::before {
  content: ""; position: absolute; left: 0; top: 0;
  width: calc(2 * var(--r-dim-r, 0px)); height: calc(2 * var(--r-dim-r, 0px)); border-radius: 50%;
  box-shadow: 0 0 0 var(--r-dim-r, 0px) var(--dim);
}
.r-dim > [data-corner="tr"]::before { left: var(--r-dim-far, 0px); }
.r-dim > [data-corner="bl"]::before { top: var(--r-dim-far, 0px); }
.r-dim > [data-corner="br"]::before { left: var(--r-dim-far, 0px); top: var(--r-dim-far, 0px); }

/* targets: the window under the pointer, as the capture overlay outlines it, and a whole display */
.r-win { position: absolute; pointer-events: none; border-radius: var(--r-radius); background: var(--window-tint); }
.r-win::after { content: ""; position: absolute; inset: -2px; border: 2px solid var(--iris); border-radius: calc(var(--r-radius) + 2px);
  box-shadow: inset 0 0 0 1px var(--selection-inner), 0 0 24px var(--iris-glow); }
.r-target-display { position: absolute; inset: 0; pointer-events: none;
  box-shadow: inset 0 0 0 3px var(--iris), inset 0 0 0 4px var(--selection-inner); }
.r-chip { position: absolute; left: 50%; top: 50%; transform: translate(-50%, calc(-50% - 20px)); pointer-events: none; }

/* paused: the frame stays, dashed and neutral */
.r-frame-paused { position: absolute; pointer-events: none; border-radius: 2px; outline: 1.5px dashed var(--text); box-shadow: 0 0 0 2.5px var(--edge-outer); }

/* the countdown and its Cancel capsule, centred on the target */
.r-count { position: absolute; display: flex; flex-direction: column; align-items: center; gap: var(--space-16); transform: translateX(-50%); }
.r-cap { display: inline-flex; gap: var(--space-4); padding: var(--space-4); border-radius: var(--radius-strip); }

/* the webcam bubble's corner slots */
.r-cam-slot { position: absolute; border-radius: 50%; pointer-events: none;
  outline: 1.5px dashed var(--text); outline-offset: -1.5px; box-shadow: 0 0 0 1px var(--edge-outer); }
.r-cam-slot[data-near] { outline-color: var(--iris-text); background: var(--iris-soft); }
</style>
