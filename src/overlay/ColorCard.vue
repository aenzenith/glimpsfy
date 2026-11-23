<script setup lang="ts">
// The colour picker over the frozen frame (design/canvas/index.html, row 29, Y1 "everything in
// the loupe"): a 17 px cross where the system pointer was, and beside it a card with the lens, the
// colour in the chosen format, the format tabs, the recent colours and, after ⇧ click, the
// contrast of the held colour against the one under the cross. A click copies the colour and
// closes; ⌘C and 1–8 copy and stay. The page (`App.vue`) hands the pointer and the keys here while
// its mode is "pick"; the maths is `lib/color.ts`, the memory and the clipboard the core's.
import { computed, onMounted, onUnmounted, ref, shallowRef } from "vue";

import GIcon from "../components/GIcon.vue";
import type { OverlayFrame } from "../lib/api/capture";
import { color_copy, color_set_format, color_state } from "../lib/api/color";
import {
  FORMAT_LABEL,
  apcaLc,
  formatsFor,
  formatted,
  hex,
  parseSwatch,
  read,
  wcagPasses,
  wcagRatio,
  type Format,
  type Reading,
} from "../lib/color";
import { language, t } from "../lib/i18n";
import { drawLens, ensureContext, pixelAt } from "./frame";
import { placeLoupe } from "./layout";

type Point = { x: number; y: number };

const props = defineProps<{
  frame: OverlayFrame;
  /** The display in points. */
  view: { w: number; h: number };
  /** Frame pixels per point. */
  ratio: { x: number; y: number };
  /** The frozen frame's canvas and, when it came raw, its pixels. */
  canvas: () => CanvasRenderingContext2D | null;
  pixels: () => Uint8ClampedArray | null;
  displayId: number;
}>();

const emit = defineEmits<{
  /** Ends the capture as cancelled, after a click copied or with Esc in the picker's own mode. */
  close: [];
  /** Esc in a picker that I opened over the area selection, with nothing left to drop. */
  back: [];
  /** A short line for the page's notice: what ⌘C or 1–8 copied while the picker stays open. */
  say: [text: string];
}>();

const LENS_CELLS = 17;
const LENS_SIZE = 136;

/** The picker is open on this display. */
const active = ref(false);
/** The pointer is on this display, so the cross and the card show. */
const shown = ref(false);
/** The frame pixel under the cross. */
const at = shallowRef<Point | null>(null);
/** The colour under the cross; `null` over a window closed to capture. */
const live = shallowRef<Reading | null>(null);
const blocked = ref(false);
/** The colour ⇧ click took for the comparison. */
const held = shallowRef<Reading | null>(null);
/** X swapped the roles: the held colour is the ground and the live one the text. */
const swapped = ref(false);
const format = ref<Format>("hex");
const recent = ref<string[]>([]);

const crossEl = ref<HTMLElement | null>(null);
const cardEl = ref<HTMLElement | null>(null);
const lensHost = ref<HTMLElement | null>(null);

let lens: CanvasRenderingContext2D | null = null;
let busy = false;
/** The card's measured size, for its placement beside the cross. */
let size = { w: 148, h: 196 };
let observer: ResizeObserver | null = null;

const formats = computed(() => formatsFor(props.frame.colorSpace));
/** The remembered format where this frame offers it; P3 reads as HEX on an sRGB frame. */
const fmt = computed<Format>(() => (formats.value.includes(format.value) ? format.value : "hex"));
const double = computed(() => Math.abs(props.ratio.x - 2) < 0.01);
const compare = computed(() => held.value !== null && live.value !== null);

const roles = computed(() => {
  const h = held.value;
  const l = live.value;
  if (!h || !l) return null;
  return swapped.value ? { text: l, ground: h } : { text: h, ground: l };
});

const decimals = (digits: number) =>
  new Intl.NumberFormat(language.value, { minimumFractionDigits: digits, maximumFractionDigits: digits });
/** A number as the user's language writes it, with a true minus sign. */
const num = (v: number, digits: number) => decimals(digits).format(v).replace("-", "−");

const verdict = computed(() => {
  const r = roles.value;
  if (!r) return null;
  // The thresholds read the unrounded ratio, in the frame's own colour space.
  const ratio = wcagRatio(r.text.sample, r.ground.sample);
  const lc = apcaLc(r.text.sample, r.ground.sample);
  return { ratio, lc, passes: wcagPasses(ratio), ratioText: `${num(ratio, 2)}:1`, lcText: `Lc ${num(lc, 1)}` };
});

/** The colour as the card writes it: P3's three numbers alone, since the tab names the space. */
function shownValue(reading: Reading, f: Format): string {
  if (f === "p3") return reading.p3.map((v) => String(Number(v.toFixed(3)))).join(" ");
  return formatted(reading, f);
}

const value = computed(() => (live.value ? shownValue(live.value, fmt.value) : "—"));
const approx = computed(() => !!live.value?.out && fmt.value !== "p3");
/** P3's three numbers and a saturated HSL run past 15 characters, which the 148 px card fits only a
 * size smaller. */
const long = computed(() => value.value.length + (approx.value ? 2 : 0) > 15);
/** Four tabs leave no room for the position. */
const xyShown = computed(() => formats.value.length === 3);
const xy = computed(() => (at.value ? `${at.value.x}, ${at.value.y}` : ""));
/** At 2x a point is 2 × 2 pixels: the stronger grid lines fall on the points' edges. */
const gridStyle = computed(() => {
  const p = at.value;
  if (!double.value || !p) return undefined;
  return { "--o-grid-x": `${p.x % 2 ? 7.5 : -0.5}px`, "--o-grid-y": `${p.y % 2 ? 7.5 : -0.5}px` };
});

// ------------------------------------------------------------------------------ the pixel

/** The window under `p` (points) keeps itself out of captures, so the frame holds what lies behind it. */
function blockedAt(p: Point): boolean {
  const w = props.frame.windows.find((w) => p.x > w.rect.x && p.y > w.rect.y && p.x < w.rect.x + w.rect.width && p.y < w.rect.y + w.rect.height);
  return !!w?.captureBlocked;
}

/** The point at the middle of frame pixel `p`. */
const pointOf = (p: Point): Point => ({ x: (p.x + 0.5) / props.ratio.x, y: (p.y + 0.5) / props.ratio.y });

/** Moves the cross to frame pixel (`x`, `y`) and reads it. */
function aimAt(x: number, y: number): void {
  const ctx = props.canvas();
  if (!ctx) return;
  const px = Math.max(0, Math.min(ctx.canvas.width - 1, x));
  const py = Math.max(0, Math.min(ctx.canvas.height - 1, y));
  const p = pointOf({ x: px, y: py });
  if (px !== at.value?.x || py !== at.value?.y) {
    at.value = { x: px, y: py };
    blocked.value = blockedAt(p);
    live.value = blocked.value ? null : read({ space: props.frame.colorSpace, bytes: pixelAt(ctx, props.pixels(), px, py) });
    if (lens && !blocked.value) drawLens(lens, ctx.canvas, px, py, LENS_CELLS);
  }
  shown.value = true;
  place(p);
}

let placedAt: Point | null = null;

function place(p: Point): void {
  placedAt = p;
  const cross = crossEl.value;
  const card = cardEl.value;
  if (cross) cross.style.transform = `translate(${p.x}px, ${p.y}px)`;
  if (card) {
    const to = placeLoupe(p.x, p.y, props.view, size);
    card.style.transform = `translate(${to.x}px, ${to.y}px)`;
  }
}

// ---------------------------------------------------------------------------- copying

/** Copies `text` (one colour in `f`, or a contrast line without `f`). A click closes after it;
 * ⌘C and 1–8 stay, and the page's notice says what was copied, since the toast waits under the
 * overlay. */
async function copy(text: string, swatch: string, f: Format | null, then: "close" | "stay"): Promise<void> {
  if (busy) return;
  busy = true;
  try {
    const state = await color_copy(text, swatch, f, props.displayId);
    recent.value = state.recent;
    if (then === "close") emit("close");
    else emit("say", `${t("toast.colorCopied")} · ${text}`);
  } catch (error) {
    console.warn("color_copy", error);
  } finally {
    busy = false;
  }
}

function copyLive(then: "close" | "stay"): void {
  const r = live.value;
  if (r) void copy(formatted(r, fmt.value), r.css, fmt.value, then);
}

/** ⌘C in the comparison: both colours, the ratio and the APCA value on one line. */
function copyPair(): void {
  const r = roles.value;
  const v = verdict.value;
  if (!r || !v) return;
  const line = t("picker.pairLine", {
    text: formatted(r.text, fmt.value),
    ground: formatted(r.ground, fmt.value),
    ratio: num(v.ratio, 2),
    lc: num(v.lc, 1),
  });
  void copy(line, r.ground.css, null, "stay");
}

function copyRecent(n: number): void {
  const css = recent.value[n - 1];
  const sample = css ? parseSwatch(css) : null;
  if (!sample) return;
  const r = read(sample);
  void copy(formatted(r, fmt.value), r.css, fmt.value, "stay");
}

/** ⌥: the next format of this frame, remembered for the next time. */
function cycle(): void {
  const list = formats.value;
  const next = list[(list.indexOf(fmt.value) + 1) % list.length];
  format.value = next;
  void color_set_format(next).catch((error) => console.warn("color_set_format", error));
}

// --------------------------------------------------------------------- what the page calls

/** Opens the picker with the cross at `p` (points), or hidden until the pointer comes. */
function open(p: Point | null): void {
  active.value = true;
  held.value = null;
  swapped.value = false;
  at.value = null;
  live.value = null;
  shown.value = false;
  const host = lensHost.value;
  if (host) {
    lens = ensureContext(host, props.frame.colorSpace, "lens");
    const pixels = LENS_SIZE * (window.devicePixelRatio || 1);
    if (lens.canvas.width !== pixels) lens.canvas.width = pixels;
    if (lens.canvas.height !== pixels) lens.canvas.height = pixels;
  }
  void color_state()
    .then((state) => {
      format.value = state.format;
      recent.value = state.recent;
    })
    .catch((error) => console.warn("color_state", error));
  if (p) move(p);
}

/** The pointer moved to `p` (points). */
function move(p: Point): void {
  if (!active.value) return;
  aimAt(Math.floor(p.x * props.ratio.x), Math.floor(p.y * props.ratio.y));
}

/** A press: a click copies the colour and closes, ⇧ click holds it for the comparison. */
function press(shift: boolean): void {
  if (!active.value || !live.value) return;
  if (shift) {
    held.value = live.value;
    swapped.value = false;
    return;
  }
  copyLive("close");
}

function leave(): void {
  shown.value = false;
}

/** The picker's keys; `true` when the key was the picker's. */
function key(e: KeyboardEvent): boolean {
  if (!active.value) return false;
  if (e.key === "Escape") {
    // Esc first lets the comparison go, then leaves the picker.
    if (held.value) {
      held.value = null;
      swapped.value = false;
      return true;
    }
    if (props.frame.mode === "pickColor") {
      emit("close");
    } else {
      active.value = false;
      shown.value = false;
      emit("back");
    }
    return true;
  }
  if (e.key === "Alt") {
    if (!e.repeat) cycle();
    return true;
  }
  // The card has nothing to focus, so Tab would only move WebKit's focus off the overlay's page.
  if (e.key === "Tab") return true;
  // ⌘C on macOS, Ctrl+C elsewhere; the other modifier copies too, since nothing else takes C here.
  const command = e.metaKey || e.ctrlKey;
  if (command) {
    if (e.key.toLowerCase() !== "c" && e.code !== "KeyC") return false;
    if (compare.value) copyPair();
    else copyLive("stay");
    return true;
  }
  if (e.altKey) return false;
  if (e.key.toLowerCase() === "x") {
    if (held.value) swapped.value = !swapped.value;
    return true;
  }
  const arrows: Record<string, Point> = { ArrowLeft: { x: -1, y: 0 }, ArrowRight: { x: 1, y: 0 }, ArrowUp: { x: 0, y: -1 }, ArrowDown: { x: 0, y: 1 } };
  const step = arrows[e.key];
  if (step && at.value) {
    // One frame pixel, so a 2x display moves by half a point.
    const by = e.shiftKey ? 10 : 1;
    aimAt(at.value.x + step.x * by, at.value.y + step.y * by);
    return true;
  }
  const digit = /^[1-8]$/.test(e.key) ? Number(e.key) : /^Digit[1-8]$/.test(e.code) ? Number(e.code.slice(5)) : 0;
  if (digit) {
    copyRecent(digit);
    return true;
  }
  return false;
}

defineExpose({ open, move, press, leave, key });

onMounted(() => {
  observer = new ResizeObserver(() => {
    const card = cardEl.value;
    if (!card || !card.offsetWidth) return;
    size = { w: card.offsetWidth, h: card.offsetHeight };
    if (placedAt) place(placedAt);
  });
  if (cardEl.value) observer.observe(cardEl.value);
});
onUnmounted(() => observer?.disconnect());

const mark = (ok: boolean) => (ok ? "check" : "close");
</script>

<template>
  <span ref="crossEl" v-show="active && shown" class="o-cross o-moving" aria-hidden="true" />
  <div ref="cardEl" v-show="active && shown" class="g-glass o-loupe o-moving pc-card" :data-compare="compare ? '' : undefined" aria-hidden="true">
    <div class="pc-left">
      <div class="o-lens pc-lens" :data-scale="double ? '2' : undefined" :style="gridStyle">
        <div ref="lensHost" class="pc-lens-host" />
        <span v-if="blocked" class="pc-hatch"><GIcon name="lock" :size="24" /></span>
        <span v-else class="o-lens-mid" />
      </div>
      <div v-if="!compare" class="pc-val">
        <span v-if="blocked" class="pc-muted">—</span>
        <template v-else-if="live">
          <i class="pc-sw" :style="{ background: live.css }" />
          <span class="pc-text" :data-long="long ? '' : undefined"><span v-if="approx" class="pc-warn">≈ </span>{{ value }}</span>
        </template>
      </div>
      <div v-if="!blocked" class="pc-row">
        <span v-for="f in formats" :key="f" class="pc-tab" :data-on="f === fmt ? '' : undefined">{{ FORMAT_LABEL[f] }}</span>
        <span v-if="xyShown" class="pc-xy">{{ xy }}</span>
      </div>
      <template v-if="!compare">
        <div v-if="approx" class="pc-note">
          <GIcon name="warning" :size="12" />
          <span class="pc-warn">{{ t("picker.outOfGamut") }}</span>
        </div>
        <div v-if="blocked" class="pc-note">
          <GIcon name="warning" :size="12" />
          <span>{{ t("picker.blocked") }}</span>
        </div>
        <div v-if="recent.length && !blocked" class="pc-recent">
          <i v-for="c in recent" :key="c" :style="{ background: c }" />
        </div>
      </template>
    </div>

    <div v-if="compare && roles && verdict" class="pc-side">
      <div class="pc-pair">
        <i class="pc-sw" :style="{ background: roles.text.css }" />
        <span class="pc-muted">{{ t("picker.text") }}</span>
        <b><span v-if="roles.text.out" class="pc-warn">≈ </span>{{ hex(roles.text.srgb) }}</b>
      </div>
      <div class="pc-pair">
        <i class="pc-sw" :style="{ background: roles.ground.css }" />
        <span class="pc-muted">{{ t("picker.ground") }}</span>
        <b><span v-if="roles.ground.out" class="pc-warn">≈ </span>{{ hex(roles.ground.srgb) }}</b>
      </div>
      <div class="pc-sample" :style="{ background: roles.ground.css, color: roles.text.css }"><b>Aa</b><span>Aa Bb Cc</span></div>
      <div class="pc-ratio">{{ verdict.ratioText }}</div>
      <div class="pc-table">
        <span />
        <span class="pc-th">AA</span>
        <span class="pc-th">AAA</span>
        <span>{{ t("picker.normal") }}</span>
        <span class="pc-mark" :data-ok="verdict.passes.normalAA"><GIcon :name="mark(verdict.passes.normalAA)" :size="12" /></span>
        <span class="pc-mark" :data-ok="verdict.passes.normalAAA"><GIcon :name="mark(verdict.passes.normalAAA)" :size="12" /></span>
        <span>{{ t("picker.large") }}</span>
        <span class="pc-mark" :data-ok="verdict.passes.largeAA"><GIcon :name="mark(verdict.passes.largeAA)" :size="12" /></span>
        <span class="pc-mark" :data-ok="verdict.passes.largeAAA"><GIcon :name="mark(verdict.passes.largeAAA)" :size="12" /></span>
      </div>
      <div class="pc-apca">APCA <b>{{ verdict.lcText }}</b></div>
    </div>
  </div>
</template>

<style scoped>
/* The card grows from today's loupe (overlay.css .o-loupe, .o-lens): 148 × 196 with one colour,
   148 × 226 with the recent colours, 306 × 216 for a comparison. */
.pc-card[data-compare] {
  display: grid;
  grid-template-columns: 136px 146px;
  column-gap: 12px;
  width: 306px;
}
.pc-lens-host {
  grid-column: 1 / -1;
  grid-row: 1 / -1;
}
.pc-lens-host :deep(canvas) {
  display: block;
  width: 136px;
  height: 136px;
  image-rendering: pixelated;
}
.pc-lens::after {
  z-index: 2;
}
/* A window closed to capture: hatched, because the frame holds what lies under it. */
.pc-hatch {
  position: absolute;
  inset: 0;
  z-index: 1;
  display: grid;
  place-items: center;
  color: var(--warn);
  background: repeating-linear-gradient(135deg, var(--divider) 0 1px, transparent 1px 7px), var(--glass-solid);
}
.pc-val {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 26px;
  margin-top: 4px;
  padding: 0 3px 0 4px;
  font: 600 11.5px/16px var(--font-mono);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  overflow: hidden;
}
.pc-text {
  overflow: hidden;
  text-overflow: ellipsis;
}
.pc-text[data-long] {
  font-size: 10.5px;
  letter-spacing: -0.02em;
}
.pc-sw {
  flex: none;
  display: inline-block;
  width: 11px;
  height: 11px;
  border-radius: 3px;
  box-shadow: inset 0 0 0 var(--hairline) rgba(255, 255, 255, 0.32), 0 0 0 var(--hairline) rgba(0, 0, 0, 0.35);
}
.pc-row {
  display: flex;
  align-items: center;
  gap: 7px;
  height: 18px;
  padding: 0 4px 0 5px;
  font: 600 10px/14px var(--font);
  letter-spacing: 0.03em;
  white-space: nowrap;
}
.pc-tab {
  padding-bottom: 1px;
  color: var(--text-muted);
}
.pc-tab[data-on] {
  color: var(--text);
  box-shadow: inset 0 -1.5px 0 var(--iris-text);
}
.pc-xy {
  margin-left: auto;
  font: var(--type-num-small);
  letter-spacing: 0;
  color: var(--text-muted);
}
.pc-note {
  display: flex;
  align-items: flex-start;
  gap: 5px;
  padding: 6px 4px 2px 5px;
  font: var(--type-body);
}
.pc-note > :first-child {
  flex: none;
  margin-top: 2px;
  color: var(--warn);
}
.pc-recent {
  display: flex;
  gap: 4px;
  margin-top: 8px;
  padding: 8px 4px 2px 5px;
  box-shadow: inset 0 var(--hairline) 0 var(--divider);
}
.pc-recent > i {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  box-shadow: inset 0 0 0 var(--hairline) rgba(255, 255, 255, 0.32), 0 0 0 var(--hairline) rgba(0, 0, 0, 0.35);
}
.pc-muted {
  color: var(--text-muted);
}
.pc-warn {
  color: var(--warn);
}
.pc-side {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
  padding: 2px 2px 2px 0;
}
.pc-pair {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 18px;
  font: var(--type-body);
  white-space: nowrap;
}
.pc-pair > .pc-sw {
  width: 12px;
  height: 12px;
}
.pc-pair > b {
  margin-left: auto;
  font: var(--type-num-small);
  color: var(--text);
}
.pc-sample {
  display: flex;
  align-items: baseline;
  gap: 8px;
  height: 40px;
  margin-top: 2px;
  padding: 0 10px;
  border-radius: var(--radius-thumb);
  box-shadow: inset 0 0 0 var(--hairline) rgba(255, 255, 255, 0.18);
  overflow: hidden;
  white-space: nowrap;
}
.pc-sample > b {
  font: 700 20px/40px var(--font);
}
.pc-sample > span {
  font: 400 13px/40px var(--font);
}
.pc-ratio {
  font: 600 20px/24px var(--font);
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.01em;
}
.pc-table {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 30px 30px;
  align-items: center;
  row-gap: 4px;
  font: var(--type-body);
}
.pc-table > span {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.pc-th {
  font: var(--type-caption-strong);
  color: var(--text-muted);
  text-align: center;
}
/* Verdict marks: ok green, failure amber (red belongs to recording on glass). */
.pc-table > .pc-mark {
  display: inline-grid;
  place-items: center;
  justify-self: center;
  width: 16px;
  height: 16px;
  border-radius: 8px;
  color: var(--on-status);
}
.pc-mark[data-ok="true"] {
  background: var(--ok);
}
.pc-mark[data-ok="false"] {
  background: var(--warn);
}
.pc-mark > g-icon {
  width: 10px;
  height: 10px;
}
.pc-apca {
  font: var(--type-num-small);
  color: var(--text-muted);
  white-space: nowrap;
}
.pc-apca > b {
  font-weight: 500;
  color: var(--text);
}
</style>
