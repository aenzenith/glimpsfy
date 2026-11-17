<script setup lang="ts">
// The recording target (`rec-target`): a transparent panel over one whole display, in the
// display's local points. During the setup it draws what the take will record and lets the user
// change it: an area with its edge, handles and size label (drag inside to move it, a handle to
// resize it, outside to draw a new one), the windows to pick from, or the whole display with its
// chip. During the countdown it draws the red frame, the ring and Cancel, and it owns the keyboard
// (Esc cancels, Return starts at once). During the take it draws only the frame, dashed and
// neutral while paused; the core makes the window click-through then. While the webcam bubble is
// dragged it shows the four corner slots it can snap to.
import { computed, onMounted, onUnmounted, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import {
  onRecordBubble,
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

const NO_HOLE: Rect = { x: -1, y: -1, width: 0, height: 0 };

const hovered = computed<SetupWindow | null>(() => {
  const s = setup.value;
  const p = pointer.value;
  if (!s || s.mode !== "window" || !p || phase.value !== "setup") return null;
  return s.windows.find((w) => inside(w.rect, p)) ?? null;
});

/** The clear part of the dim during the setup. */
const hole = computed<Rect>(() => {
  const s = setup.value;
  if (!s) return NO_HOLE;
  if (s.mode === "window") return hovered.value?.rect ?? s.window?.rect ?? NO_HOLE;
  return area.value ?? NO_HOLE;
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

const windowRadius = computed(() => (mode.value === "window" ? `${WINDOW_RADIUS}px` : ""));

// ------------------------------------------------------------------------------- countdown

/** The ring fills smoothly between the core's whole-second ticks. */
function runRing(): void {
  cancelAnimationFrame(ringFrame);
  const c = countdown.value;
  if (!c) return;
  const total = Math.max(1, c.totalSeconds);
  const done = total - c.remainingSeconds;
  const part = reducedMotion() ? 0 : Math.min(1, (performance.now() - tick.at) / 1000);
  progress.value = clamp(((done + part) / total) * 100, 0, 100);
  ringFrame = requestAnimationFrame(runRing);
}

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
  cancelAnimationFrame(frameRequest);
  cancelAnimationFrame(ringFrame);
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div class="r-target-page" :style="{ cursor }" @mousedown="onMouseDown" @mouseleave="onMouseLeave">
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
        <div class="g-dim" :style="{ ...box(hole), borderRadius: windowRadius }" />
        <div v-if="setup.window" class="r-target" :style="{ ...box(setup.window.rect), borderRadius: windowRadius }" />
      </template>
      <template v-else>
        <div class="g-dim" :style="box(hole)" />
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
      <div v-if="mode !== 'display' && target" class="g-dim" :style="{ ...box(target), borderRadius: windowRadius }" />
      <div v-if="frameBox" class="g-rec-frame" :style="box(frameBox)" />
      <div v-if="countAt" class="r-count" :style="countAt">
        <div class="g-countdown" role="timer" :aria-label="t('record.countdown.label', { n: countdown.remainingSeconds })">
          <span>{{ countdown.remainingSeconds }}</span>
          <svg class="g-ring" viewBox="0 0 22 22" :style="{ '--progress': progress }" aria-hidden="true">
            <circle cx="11" cy="11" r="8" pathLength="100" />
            <circle cx="11" cy="11" r="8" pathLength="100" />
          </svg>
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
/* The kit's dim spreads 4000 px; 200vmax reaches every corner of any display. */
.r-target-page .g-dim { box-shadow: 0 0 0 200vmax var(--dim); }
.r-abs { position: absolute; }

/* targets: a window and a whole display */
.r-target { position: absolute; pointer-events: none; background: var(--window-tint);
  box-shadow: 0 0 0 2px var(--iris), inset 0 0 0 1px var(--selection-inner), 0 0 24px var(--iris-glow); }
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
