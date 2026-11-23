<script setup lang="ts">
// The screen ruler over the frozen frame (design/canvas/index.html, row 30, Y1): a drag draws a
// measure line, or a box with ⌥ or the bar's Box, whose ends snap to the nearest edge within 6 pt
// (`edges.ts`), and every measure stays on the screen until it is cleared. ⇧ keeps a line to one
// axis, ⌘ turns snapping off, the arrows move the last released end by one pixel, ⌘C copies the
// last measure and ⌫ deletes it. Windows closed to capture are hatched and never snap. The page
// (`App.vue`) hands the pointer and the keys here first while the ruler is on.
import { computed, onMounted, onUnmounted, onUpdated, ref, shallowRef } from "vue";

import GIcon from "../../components/GIcon.vue";
import type { LogicalRect, OverlayFrame } from "../../lib/api/capture";
import { app_copy_text } from "../../lib/api/windows";
import { t } from "../../lib/i18n";
import { commandHeld, commandKeyName } from "../../lib/keys";
import { measure as snapMeasure, type PixelSource, type SnapOptions } from "./edges";
import {
  THRESHOLDS,
  choice,
  copyText,
  noEdge,
  nudge,
  readout,
  ringAt,
  scaled,
  straight,
  text,
  type Measure,
  type Point,
  type Reading,
  type Scale,
} from "./measure";

const props = defineProps<{
  frame: OverlayFrame;
  /** The display in points. */
  view: { w: number; h: number };
  scale: Scale;
  /** The frozen frame's pixels, once the frame is drawn. */
  source: () => PixelSource | null;
  /** This display holds the keyboard; the bar shows only here. */
  bar: boolean;
}>();
const emit = defineEmits<{ close: [] }>();

/** How far an end looks for an edge, in points. */
const REACH = 6;
/** A drag shorter than this, in points, is a click and leaves no measure. */
const MIN_DRAG = 3;
const COPIED_MS = 1600;

const measures = shallowRef<Measure[]>([]);
const live = shallowRef<Measure | null>(null);
/** The measure ⌘C copied and its text. It is held by the measure itself, so a measure that
 * moves, snaps again or goes away takes the capsule with it and a new one never shows it. */
const copied = shallowRef<{ measure: Measure; text: string } | null>(null);
const root = ref<HTMLElement | null>(null);

/** Where the drag started and where the pointer is, in frame pixels. */
let from: Point | null = null;
let last: Point = { x: 0, y: 0 };
let fromView: Point = { x: 0, y: 0 };
const mods = { shift: false, alt: false, command: false };
let copiedTimer = 0;

const isScaled = computed(() => scaled(props.scale));
/** The unit a label leads with and ⌘C writes: points only where they differ from pixels. */
const unit = computed(() => (isScaled.value ? choice.unit : "px"));

// ------------------------------------------------------------------------------ geometry

const inside = (r: LogicalRect, p: Point) => p.x >= r.x && p.y >= r.y && p.x < r.x + r.width && p.y < r.y + r.height;

/** A point (frame pixels) inside a window closed to capture, where the frame shows what lies
 * behind the window, so nothing there is an edge of it. */
function blockedAt(p: Point): boolean {
  const pt = { x: p.x / props.scale.x, y: p.y / props.scale.y };
  return props.frame.windows.find((w) => inside(w.rect, pt))?.captureBlocked ?? false;
}

function options(shape = mods.alt ? "box" : choice.shape, lock = mods.shift, free = mods.command): SnapOptions {
  return {
    shape,
    lock,
    free,
    reach: Math.round(REACH * Math.max(props.scale.x, props.scale.y)),
    band: Math.max(1, Math.round(Math.max(props.scale.x, props.scale.y))),
    threshold: choice.threshold,
    open: (p) => !blockedAt(p),
  };
}

function relive(): void {
  const src = props.source();
  if (!from || !src) return;
  live.value = snapMeasure(src, from, last, options());
}

const raw = (p: Point): Point => ({ x: p.x * props.scale.x, y: p.y * props.scale.y });

function readMods(e: MouseEvent | KeyboardEvent): void {
  mods.shift = e.shiftKey;
  mods.alt = e.altKey;
  mods.command = commandHeld(e);
}

// ------------------------------------------------------------------- pointer and keys

function press(p: Point, e: MouseEvent): void {
  readMods(e);
  from = raw(p);
  last = from;
  fromView = p;
  live.value = null;
}

function move(p: Point, e: MouseEvent): void {
  readMods(e);
  last = raw(p);
  relive();
}

function release(p: Point, e: MouseEvent): void {
  move(p, e);
  const m = live.value;
  const long = Math.hypot(p.x - fromView.x, p.y - fromView.y) >= MIN_DRAG;
  if (m && long && (m.a.x !== m.b.x || m.a.y !== m.b.y)) measures.value = [...measures.value, m];
  from = null;
  live.value = null;
}

/** Esc: ends a drag, or clears the measures; `false` when there was nothing to clear. */
function escape(): boolean {
  if (from) {
    from = null;
    live.value = null;
    return true;
  }
  if (measures.value.length === 0) return false;
  clear();
  return true;
}

function clear(): void {
  measures.value = [];
  copied.value = null;
}

/** Runs the ruler's keys; `true` when the key was the ruler's. */
function key(e: KeyboardEvent): boolean {
  readMods(e);
  if (e.key === "Shift" || e.key === "Alt" || e.key === commandKeyName()) {
    relive();
    return true;
  }
  if (commandHeld(e)) {
    if (e.key.toLowerCase() !== "c" || e.shiftKey) return false;
    copy();
    return true;
  }
  const arrows: Record<string, Point> = { ArrowLeft: { x: -1, y: 0 }, ArrowRight: { x: 1, y: 0 }, ArrowUp: { x: 0, y: -1 }, ArrowDown: { x: 0, y: 1 } };
  const step = arrows[e.key];
  if (step) {
    const list = measures.value;
    const m = list[list.length - 1];
    if (!m || from) return true;
    const by = e.shiftKey ? 10 : 1;
    const { width, height } = props.frame.frameSize;
    measures.value = [...list.slice(0, -1), nudge(m, step.x * by, step.y * by, width, height)];
    return true;
  }
  if (e.altKey) return false;
  switch (e.key.toLowerCase()) {
    case "backspace":
    case "delete":
      if (!from) measures.value = measures.value.slice(0, -1);
      return true;
    case "l":
      choice.shape = "line";
      relive();
      return true;
    case "r":
      choice.shape = "box";
      relive();
      return true;
    case "u":
      if (isScaled.value) choice.unit = choice.unit === "px" ? "pt" : "px";
      return true;
    case "[":
    case "]":
      sensitivity(e.key === "]" ? -1 : 1);
      return true;
    // Tab would move the focus onto the bar's buttons, where Space and ↩ would press them.
    case "tab":
      return true;
  }
  return false;
}

function keyUp(e: KeyboardEvent): void {
  readMods(e);
  relive();
}

/** ] snaps to fainter edges and [ only to stronger ones; the drag and the last measure snap
 * again, so the rings at their ends show the change. */
function sensitivity(direction: number): void {
  const at = THRESHOLDS.indexOf(choice.threshold);
  const next = THRESHOLDS[Math.max(0, Math.min(THRESHOLDS.length - 1, (at < 0 ? THRESHOLDS.indexOf(12) : at) + direction))];
  if (next === choice.threshold) return;
  choice.threshold = next;
  relive();
  const list = measures.value;
  const m = list[list.length - 1];
  const src = props.source();
  if (!m?.raw || !src) return;
  measures.value = [...list.slice(0, -1), snapMeasure(src, m.raw.a, m.raw.b, options(m.shape, m.raw.lock, m.raw.free))];
}

/** ⌘C: the last measure's reading in the leading unit ("120 px", "96 × 28 px"). */
function copy(): void {
  const list = measures.value;
  const m = list[list.length - 1];
  if (!m) return;
  const value = copyText(m, props.scale, unit.value);
  void app_copy_text(value).then(
    () => {
      copied.value = { measure: m, text: value };
      clearTimeout(copiedTimer);
      copiedTimer = window.setTimeout(() => (copied.value = null), COPIED_MS);
    },
    (error) => console.warn("app_copy_text", error),
  );
}

defineExpose({ press, move, release, key, keyUp, escape });

// ------------------------------------------------------------------------------- drawing

type Seg = { x1: number; y1: number; x2: number; y2: number };
type Anchor = "n" | "s" | "e" | "w" | "c";
type Label = { x: number; y: number; a: Anchor; kind: "size" | "warn" | "copied"; main: Reading; alt: Reading | null; copied: string };
type Drawn = { key: string; segs: Seg[]; crisp: boolean; rect: { x: number; y: number; w: number; h: number } | null; rings: Point[]; label: Label };

/** A label is 24 pt tall and keeps 4 pt inside the display. */
const LABEL_H = 24;
const dpr = () => window.devicePixelRatio || 1;
/** A frame coordinate in points, on the device pixel grid. */
const X = (v: number) => Math.round((v / props.scale.x) * dpr()) / dpr();
const Y = (v: number) => Math.round((v / props.scale.y) * dpr()) / dpr();

function draw(m: Measure, key: string, copiedText: string | null): Drawn {
  const { main, alt } = readout(m, props.scale, unit.value);
  const kind = copiedText ? "copied" : noEdge(m) ? "warn" : "size";
  const label = (x: number, y: number, a: Anchor): Label => ({ x, y, a, kind, main, alt, copied: copiedText ?? "" });
  const rings: Point[] = [];
  const minX = Math.min(m.a.x, m.b.x);
  const minY = Math.min(m.a.y, m.b.y);

  if (m.shape === "box") {
    const l = X(minX);
    const r = X(Math.max(m.a.x, m.b.x));
    const top = Y(minY);
    const bottom = Y(Math.max(m.a.y, m.b.y));
    // The outline sits on the box's outermost pixels, as the design draws it.
    const corner = (p: Point): Point => ({ x: p.x === minX ? l + 0.5 : r - 0.5, y: p.y === minY ? top + 0.5 : bottom - 0.5 });
    if (ringAt(m, "a")) rings.push(corner(m.a));
    if (ringAt(m, "b")) rings.push(corner(m.b));
    const cx = (l + r) / 2;
    const at =
      top - 6 - LABEL_H >= 4 ? label(cx, top - 6, "s") : bottom + 6 + LABEL_H <= props.view.h - 4 ? label(cx, bottom + 6, "n") : label(cx, top + 6, "n");
    return { key, segs: [], crisp: true, rect: { x: l + 0.5, y: top + 0.5, w: Math.max(0, r - l - 1), h: Math.max(0, bottom - top - 1) }, rings, label: at };
  }

  if (straight(m) && m.a.y === m.b.y) {
    // Along a row: the line covers the pixels just below the boundary row, as the design's
    // `hline` does, with 7 px ticks just inside both ends.
    const y = Y(m.a.y) + 0.5;
    const x1 = X(minX);
    const x2 = X(Math.max(m.a.x, m.b.x));
    const end = (p: Point): Point => ({ x: p.x === minX ? x1 + 0.5 : x2 - 0.5, y });
    if (ringAt(m, "a")) rings.push(end(m.a));
    if (ringAt(m, "b")) rings.push(end(m.b));
    const segs = [
      { x1, y1: y, x2, y2: y },
      { x1: x1 + 0.5, y1: y - 3.5, x2: x1 + 0.5, y2: y + 3.5 },
      { x1: x2 - 0.5, y1: y - 3.5, x2: x2 - 0.5, y2: y + 3.5 },
    ];
    const at = y - 6.5 - LABEL_H >= 4 ? label((x1 + x2) / 2, y - 6.5, "s") : label((x1 + x2) / 2, y + 6.5, "n");
    return { key, segs, crisp: true, rect: null, rings, label: at };
  }

  if (straight(m)) {
    const x = X(m.a.x) + 0.5;
    const y1 = Y(minY);
    const y2 = Y(Math.max(m.a.y, m.b.y));
    const end = (p: Point): Point => ({ x, y: p.y === minY ? y1 + 0.5 : y2 - 0.5 });
    if (ringAt(m, "a")) rings.push(end(m.a));
    if (ringAt(m, "b")) rings.push(end(m.b));
    const segs = [
      { x1: x, y1, x2: x, y2 },
      { x1: x - 3.5, y1: y1 + 0.5, x2: x + 3.5, y2: y1 + 0.5 },
      { x1: x - 3.5, y1: y2 - 0.5, x2: x + 3.5, y2: y2 - 0.5 },
    ];
    // Beside the line, on the side with room for the label.
    const at = x + 7.5 + 120 <= props.view.w ? label(x + 7.5, (y1 + y2) / 2, "w") : label(x - 7.5, (y1 + y2) / 2, "e");
    return { key, segs, crisp: true, rect: null, rings, label: at };
  }

  // A slanted line runs between the boundary points, with its ticks across it.
  const p1 = { x: m.a.x / props.scale.x, y: m.a.y / props.scale.y };
  const p2 = { x: m.b.x / props.scale.x, y: m.b.y / props.scale.y };
  const length = Math.hypot(p2.x - p1.x, p2.y - p1.y) || 1;
  let nx = -(p2.y - p1.y) / length;
  let ny = (p2.x - p1.x) / length;
  if (ny > 0) {
    nx = -nx;
    ny = -ny;
  }
  const tick = (p: Point): Seg => ({ x1: p.x - nx * 3.5, y1: p.y - ny * 3.5, x2: p.x + nx * 3.5, y2: p.y + ny * 3.5 });
  if (ringAt(m, "a")) rings.push(p1);
  if (ringAt(m, "b")) rings.push(p2);
  // Past the released end, where a level label never crosses the slanted line.
  const ux = (p2.x - p1.x) / length;
  const uy = (p2.y - p1.y) / length;
  const past = label(p2.x + ux * 12, p2.y + uy * 12, ux >= 0 ? "w" : "e");
  return { key, segs: [{ x1: p1.x, y1: p1.y, x2: p2.x, y2: p2.y }, tick(p1), tick(p2)], crisp: false, rect: null, rings, label: past };
}

const drawn = computed<Drawn[]>(() => {
  const list = measures.value.map((m, i) => draw(m, `m${i}`, copied.value?.measure === m ? copied.value.text : null));
  if (live.value && (live.value.a.x !== live.value.b.x || live.value.a.y !== live.value.b.y)) list.push(draw(live.value, "live", null));
  return list;
});

type Rect = { x: number; y: number; w: number; h: number };

/** `r` less the part `cut` covers, as up to four rectangles. */
function subtract(r: Rect, cut: Rect): Rect[] {
  const x0 = Math.max(r.x, cut.x);
  const y0 = Math.max(r.y, cut.y);
  const x1 = Math.min(r.x + r.w, cut.x + cut.w);
  const y1 = Math.min(r.y + r.h, cut.y + cut.h);
  if (x1 <= x0 || y1 <= y0) return [r];
  const out: Rect[] = [];
  if (y0 > r.y) out.push({ x: r.x, y: r.y, w: r.w, h: y0 - r.y });
  if (y1 < r.y + r.h) out.push({ x: r.x, y: y1, w: r.w, h: r.y + r.h - y1 });
  if (x0 > r.x) out.push({ x: r.x, y: y0, w: x0 - r.x, h: y1 - y0 });
  if (x1 < r.x + r.w) out.push({ x: x1, y: y0, w: r.x + r.w - x1, h: y1 - y0 });
  return out;
}

/** The windows closed to capture, each as the parts no window in front of it covers, with the
 * place of its name tag on the largest part. */
const closed = computed(() => {
  const windows = props.frame.windows;
  const out: { id: number; rect: Rect; pieces: Rect[]; tag: Point | null }[] = [];
  windows.forEach((w, i) => {
    if (!w.captureBlocked) return;
    const rect = { x: w.rect.x, y: w.rect.y, w: w.rect.width, h: w.rect.height };
    // The part on this display; a window may reach past its edges.
    const x0 = Math.max(0, rect.x);
    const y0 = Math.max(0, rect.y);
    const x1 = Math.min(props.view.w, rect.x + rect.w);
    const y1 = Math.min(props.view.h, rect.y + rect.h);
    if (x1 <= x0 || y1 <= y0) return;
    let pieces: Rect[] = [{ x: x0, y: y0, w: x1 - x0, h: y1 - y0 }];
    for (const front of windows.slice(0, i)) {
      const cut = { x: front.rect.x, y: front.rect.y, w: front.rect.width, h: front.rect.height };
      pieces = pieces.flatMap((p) => subtract(p, cut));
    }
    if (pieces.length === 0) return;
    const big = pieces.reduce((a, b) => (b.w * b.h > a.w * a.h ? b : a));
    out.push({ id: w.id, rect, pieces, tag: big.h >= 40 && big.w >= 80 ? { x: big.x + big.w / 2, y: big.y + 10 } : null });
  });
  return out;
});

/** Puts every anchored part on whole points, 4 pt inside the display, as the design's `settle`. */
function settle(): void {
  const host = root.value;
  if (!host) return;
  for (const el of host.querySelectorAll<HTMLElement>("[data-a]")) {
    const x = Number(el.dataset.x);
    const y = Number(el.dataset.y);
    const a = el.dataset.a ?? "c";
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let left = a === "w" ? x : a === "e" ? x - w : x - w / 2;
    let top = a === "n" ? y : a === "s" ? y - h : y - h / 2;
    left = Math.max(4, Math.min(props.view.w - w - 4, left));
    top = Math.max(4, Math.min(props.view.h - h - 4, top));
    el.style.left = `${Math.round(left)}px`;
    el.style.top = `${Math.round(top)}px`;
  }
}
onMounted(settle);
onUpdated(settle);
onUnmounted(() => clearTimeout(copiedTimer));
</script>

<template>
  <div ref="root" class="r-layer">
    <svg class="r-svg" :width="view.w" :height="view.h" :viewBox="`0 0 ${view.w} ${view.h}`" aria-hidden="true">
      <defs>
        <pattern id="ruler-hatch" patternUnits="userSpaceOnUse" width="9" height="9" patternTransform="rotate(45)">
          <rect class="r-hatch" width="4" height="9" />
        </pattern>
        <clipPath v-for="c in closed" :id="`ruler-closed-${c.id}`" :key="c.id">
          <rect v-for="(p, i) in c.pieces" :key="i" :x="p.x" :y="p.y" :width="p.w" :height="p.h" />
        </clipPath>
      </defs>
      <g v-for="c in closed" :key="c.id" :clip-path="`url(#ruler-closed-${c.id})`">
        <rect :x="c.rect.x" :y="c.rect.y" :width="c.rect.w" :height="c.rect.h" fill="url(#ruler-hatch)" />
        <rect class="r-closed-edge" :x="c.rect.x + 0.5" :y="c.rect.y + 0.5" :width="Math.max(0, c.rect.w - 1)" :height="Math.max(0, c.rect.h - 1)" />
      </g>
      <g v-for="d in drawn" :key="d.key" :class="{ 'r-crisp': d.crisp }">
        <rect v-if="d.rect" class="r-halo" :x="d.rect.x" :y="d.rect.y" :width="d.rect.w" :height="d.rect.h" />
        <line v-for="(s, i) in d.segs" :key="`h${i}`" class="r-halo" :x1="s.x1" :y1="s.y1" :x2="s.x2" :y2="s.y2" />
        <rect v-if="d.rect" class="r-ln" :x="d.rect.x" :y="d.rect.y" :width="d.rect.w" :height="d.rect.h" />
        <line v-for="(s, i) in d.segs" :key="`l${i}`" class="r-ln" :x1="s.x1" :y1="s.y1" :x2="s.x2" :y2="s.y2" />
        <circle v-for="(r, i) in d.rings" :key="`r${i}`" class="r-ring" :cx="r.x" :cy="r.y" r="3" />
      </g>
    </svg>

    <template v-for="c in closed" :key="`tag${c.id}`">
      <div v-if="c.tag" class="r-pl" :data-x="c.tag.x" :data-y="c.tag.y" data-a="n">
        <span class="g-tag"><GIcon name="lock" :size="12" />{{ t("ruler.excluded") }}</span>
      </div>
    </template>

    <div v-for="d in drawn" :key="`label-${d.key}`" class="r-pl" :data-x="d.label.x" :data-y="d.label.y" :data-a="d.label.a">
      <span v-if="d.label.kind === 'copied'" class="g-glass r-cap" data-material="strong" role="status">
        <GIcon name="check" :size="16" class="r-ok" /><span>{{ t("ruler.copied") }}</span><span class="r-mono" data-muted>{{ d.label.copied }}</span>
      </span>
      <span v-else-if="d.label.kind === 'warn'" class="g-glass r-cap" data-material="strong">
        <span class="r-mono">{{ text(d.label.main) }}</span><span class="r-warn">{{ t("ruler.noEdge") }}</span>
      </span>
      <span v-else class="g-size">
        <template v-for="(n, i) in d.label.main.nums" :key="i"><span v-if="i" class="g-size-x">×</span>{{ n }}</template>
        <span class="r-u">{{ d.label.main.unit }}</span>
        <span v-if="d.label.alt" class="r-alt">· {{ text(d.label.alt) }}</span>
      </span>
    </div>

    <div v-if="bar" class="r-pl r-top" :data-x="view.w / 2" data-y="8" data-a="n" @mousedown.stop>
      <div class="g-bar" role="toolbar" :aria-label="t('ruler.bar')">
        <div class="g-seg" role="radiogroup" :aria-label="t('ruler.shape')">
          <button type="button" role="radio" :aria-checked="choice.shape === 'line'" :aria-label="t('tool.line')" @click="choice.shape = 'line'">
            <GIcon name="line" />
          </button>
          <button type="button" role="radio" :aria-checked="choice.shape === 'box'" :aria-label="t('ruler.box')" @click="choice.shape = 'box'">
            <GIcon name="rect" />
          </button>
        </div>
        <span class="g-divider" role="separator" />
        <template v-if="isScaled">
          <div class="g-seg" role="radiogroup" :aria-label="t('ruler.unit')">
            <button type="button" role="radio" :aria-checked="choice.unit === 'px'" @click="choice.unit = 'px'">px</button>
            <button type="button" role="radio" :aria-checked="choice.unit === 'pt'" @click="choice.unit = 'pt'">pt</button>
          </div>
          <span class="g-divider" role="separator" />
        </template>
        <button type="button" class="g-button" data-variant="plain" :disabled="measures.length === 0" @click="clear">{{ t("ruler.clear") }}</button>
        <span class="g-tipwrap" data-side="below" data-align="end">
          <button type="button" class="g-btn" data-tone="muted" :aria-label="t('action.close')" @click="emit('close')"><GIcon name="close" /></button>
          <span class="g-tip" role="tooltip">{{ t("action.close") }}<kbd class="g-kbd">Esc</kbd></span>
        </span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.r-layer {
  position: absolute;
  inset: 0;
  z-index: 5;
  pointer-events: none;
}
.r-svg {
  position: absolute;
  inset: 0;
  display: block;
  overflow: visible;
}
.r-crisp {
  shape-rendering: crispEdges;
}
.r-halo {
  fill: none;
  stroke: var(--selection-inner);
  stroke-width: 3;
}
.r-ln {
  fill: none;
  stroke: var(--iris);
  stroke-width: 1;
}
.r-ring {
  fill: var(--handle);
  stroke: var(--iris);
  stroke-width: 1.5;
  shape-rendering: geometricPrecision;
}
.r-hatch {
  fill: var(--dim);
}
.r-closed-edge {
  fill: none;
  stroke: var(--edge-outer);
  stroke-width: 1;
}
.r-pl {
  position: absolute;
  left: -9999px;
  top: -9999px;
}
.r-top {
  z-index: 2;
  pointer-events: auto;
  cursor: default;
}
.r-u {
  margin-left: 0.32em;
  color: var(--text-muted);
}
.r-alt {
  margin-left: 8px;
  color: var(--text-muted);
}
/* A capsule that holds words as well as numbers; numbers alone use the kit's g-size. */
.r-cap {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 24px;
  padding: 0 10px;
  border-radius: var(--radius-chip);
  font: var(--type-label);
  white-space: nowrap;
}
.r-cap > g-icon {
  margin-left: -3px;
}
.r-mono {
  font: var(--type-num);
  font-variant-numeric: tabular-nums;
}
.r-cap .r-mono[data-muted] {
  color: var(--text-muted);
}
.r-warn {
  color: var(--warn);
}
.r-ok {
  color: var(--ok);
}
</style>
