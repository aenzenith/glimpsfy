<script setup lang="ts">
// The scrolling capture's parts around the selection (D27, Y1): the glass strip with the whole
// result shrunk beside it, the iris frame on the part the selection shows now and amber marks
// on its gaps, and the bar with the status, Auto, Done and Cancel under it. The strip takes the
// action bar's place and the bar the tool bar's (`layout.ts`, `placeScroll`); the pops stand off
// the selection, which lets the pointer through to the app under it, and whatever still lies over
// it is reported (`cover`) so the pointer stays there. The page (`App.vue`) owns the keys and the
// calls; this draws and says what was clicked.
import { computed, nextTick, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import type { ScrollProgress } from "../lib/api/capture";
import { language, t } from "../lib/i18n";
import { placeScroll, type Rect, type Size } from "./layout";

const props = defineProps<{
  progress: ScrollProgress | null;
  sel: Rect;
  view: Size;
  /** Frame pixels per point. */
  scale: number;
  /** A wheel turned outside the selection, so nothing in it moved. */
  outside: boolean;
  /** Auto was pressed without Accessibility. */
  asking: boolean;
}>();
const emit = defineEmits<{
  auto: [];
  done: [];
  cancel: [];
  manual: [];
  settings: [];
  /** The strip, the bar and the pop where they lie over the selection, whose hole would let the
   * pointer through them to the app below. */
  cover: [rects: Rect[]];
}>();

/** The strip's width and its minimap's width, in points. */
const STRIP_W = 112;
const MAP_W = 96;
/** The strip's padding and its two lines of numbers. */
const STRIP_REST = 60;
/** The bar's buttons and a little of its status. */
const BAR_MIN = 420;
/** The result shrinks to fit the strip until a pixel of the strip shows this many of its rows
 * at its width's fit; past it the oldest part leaves the top under a fade. */
const MAX_SHRINK = 4;
/** The share of the limit after which the strip counts down to it. */
const NEAR_LIMIT = 0.8;

const statusEl = ref<HTMLElement | null>(null);
const popEl = ref<HTMLElement | null>(null);
const stripEl = ref<HTMLElement | null>(null);
const barEl = ref<HTMLElement | null>(null);
const popAt = ref({ x: -9999, y: -9999 });
const statusHover = ref(false);
const statusCut = ref(false);

const number = (n: number) => new Intl.NumberFormat(language.value).format(Math.round(n));
const px = (n: number) => t("scroll.px", { n: number(n) });

const placement = computed(() =>
  placeScroll(props.sel, props.view, { w: STRIP_W, h: mapBox.value.h + STRIP_REST }, BAR_MIN),
);

/** The box the minimap may fill: the strip's width, and the selection's height or the screen's. */
const mapBox = computed(() => {
  const room = Math.min(props.sel.h, props.view.h - 16) - STRIP_REST;
  return { w: MAP_W, h: Math.max(120, room) };
});

/** The minimap: its scale (points per result pixel), the rows it shows and where its marks go. */
const map = computed(() => {
  const p = props.progress;
  if (!p || p.width === 0 || p.height === 0) return null;
  const fit = MAP_W / p.width;
  const s = Math.max(Math.min(fit, mapBox.value.h / p.height), fit / MAX_SHRINK);
  const rows = Math.min(p.height, mapBox.value.h / s);
  // The part the selection shows stays in sight: at the bottom while the page scrolls down.
  const centre = p.view ? p.view[0] + p.view[1] / 2 : p.height;
  const top = Math.max(0, Math.min(p.height - rows, centre - rows / 2));
  const y = (row: number) => Math.round((row - top) * s);
  return {
    w: Math.round(p.width * s),
    h: Math.round(rows * s),
    image: { w: p.width * s, h: p.height * s, y: -top * s },
    view: p.view ? { y: y(p.view[0]), h: Math.max(4, Math.round(p.view[1] * s)) } : null,
    gaps: p.gaps.map((g) => y(g) - 5).filter((g) => g > -10 && g < rows * s),
    foldTop: top > 0.5,
    foldBottom: top + rows < p.height - 0.5,
  };
});

const near = computed(() => !!props.progress && props.progress.height >= props.progress.limit * NEAR_LIMIT);
const second = computed(() => {
  const p = props.progress;
  if (!p) return "";
  if (near.value) return t("scroll.mb", { n: new Intl.NumberFormat(language.value, { maximumFractionDigits: 0 }).format(p.bytes / 1_000_000) });
  return p.frames === 1 ? t("scroll.frames.one") : t("scroll.frames", { n: number(p.frames) });
});

/** The status line: the most pressing thing first, and the counts after a `·` otherwise. */
const status = computed<{ text: string; meta: string; warn: boolean }>(() => {
  const p = props.progress;
  const height = p ? px(p.height) : px(0);
  if (props.outside) return { text: t("scroll.still"), meta: "", warn: true };
  if (p?.gap) return { text: t(p.gap === "above" ? "scroll.gap.above" : "scroll.gap.below"), meta: "", warn: true };
  if (p && near.value) return { text: t("scroll.limit", { n: number(Math.max(0, p.limit - p.height)) }), meta: "", warn: true };
  if (p?.auto) return { text: t("scroll.auto.status"), meta: height, warn: false };
  if (p?.ended) return { text: t("scroll.ended"), meta: "", warn: false };
  if (p?.headerFound && p.footerFound) return { text: t("scroll.both"), meta: "", warn: false };
  if (p?.headerFound) return { text: t("scroll.header"), meta: height, warn: false };
  if (p?.footerFound) return { text: t("scroll.footer"), meta: height, warn: false };
  return { text: t("scroll.scroll"), meta: p && p.frames > 0 ? `${second.value} · ${height}` : "", warn: false };
});

/** The sticky header and bottom bar, hatched where they lie in the selection once found. */
const bands = computed(() => {
  const p = props.progress;
  if (!p) return [];
  const r = props.sel;
  const out: { key: string; style: Record<string, string>; edge: "top" | "bottom" }[] = [];
  if (p.headerFound && p.header > 0) {
    const h = Math.min(r.h, p.header / props.scale);
    out.push({ key: "header", edge: "top", style: { left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${h}px` } });
  }
  if (p.footerFound && p.footer > 0) {
    const h = Math.min(r.h, p.footer / props.scale);
    out.push({ key: "footer", edge: "bottom", style: { left: `${r.x}px`, top: `${r.y + r.h - h}px`, width: `${r.w}px`, height: `${h}px` } });
  }
  return out;
});

/** The pop that shows: the Accessibility question, the wheel outside the selection, or the whole
 * status line when the bar cuts it short. */
const pop = computed<{ kind: "ask" | "outside" | "status"; icon: string; warn: boolean; title: string; text: string; w: number } | null>(() => {
  if (props.asking) return { kind: "ask", icon: "accessibility", warn: false, title: t("scroll.ax.title"), text: t("scroll.ax.text"), w: 340 };
  if (props.outside) return { kind: "outside", icon: "bang", warn: true, title: t("scroll.outside.title"), text: t("scroll.outside.text"), w: 384 };
  if (statusHover.value && statusCut.value) {
    const s = status.value;
    return { kind: "status", icon: s.warn ? "bang" : "info", warn: s.warn, title: s.text, text: s.meta, w: 384 };
  }
  return null;
});

/** A pop goes under the bar, or above the selection, or above the bar: off the selection first. */
async function placePop(): Promise<void> {
  await nextTick();
  const el = popEl.value;
  const p = pop.value;
  if (!el || !p) return;
  const b = placement.value.bar;
  const h = el.offsetHeight;
  const w = el.offsetWidth;
  const { w: W, h: H } = props.view;
  const x = p.kind === "ask" ? b.x + b.w - w : b.x;
  let y = b.y + 40 + 8;
  if (y + h > H - 8) y = props.sel.y - 8 - h >= 8 ? props.sel.y - 8 - h : b.y - 8 - h;
  popAt.value = { x: Math.round(Math.max(8, Math.min(W - 8 - w, x))), y: Math.round(Math.max(8, y)) };
}
watch([pop, placement], () => void placePop());

/** Whether the bar cuts the status line short; then hovering it shows the whole line. */
function measureStatus(): void {
  const el = statusEl.value;
  statusCut.value = !!el && el.scrollWidth > el.clientWidth + 1;
}
watch(status, () => void nextTick(measureStatus));

/** Reports the parts that overlap the selection, each time one of them moves or a pop comes and goes. */
let covered = "";
async function reportCover(): Promise<void> {
  await nextTick();
  const s = props.sel;
  const rects = [stripEl.value, barEl.value, popEl.value]
    .filter((el): el is HTMLElement => !!el)
    .map((el) => el.getBoundingClientRect())
    .filter((b) => b.right > s.x && b.left < s.x + s.w && b.bottom > s.y && b.top < s.y + s.h)
    .map((b) => ({ x: b.left, y: b.top, w: b.width, h: b.height }));
  const key = JSON.stringify(rects);
  if (key === covered) return;
  covered = key;
  emit("cover", rects);
}
watch([placement, pop, popAt, () => map.value?.h], () => void reportCover(), { flush: "post", immediate: true });

const at = (p: { x: number; y: number }) => ({ left: `${p.x}px`, top: `${p.y}px` });
</script>

<template>
  <div
    v-for="band in bands"
    :key="band.key"
    class="o-scroll-band"
    :data-edge="band.edge"
    :style="band.style"
    aria-hidden="true"
  />

  <div ref="stripEl" class="g-glass o-bar o-scroll-strip" :style="at(placement.strip)" role="img" :aria-label="t('scroll.strip')" @mousedown.stop>
    <div v-if="map && progress?.previewUrl" class="o-scroll-map" :style="{ width: `${map.w}px`, height: `${map.h}px` }">
      <img
        class="o-scroll-image"
        :src="progress.previewUrl"
        alt=""
        draggable="false"
        :style="{ width: `${map.image.w}px`, height: `${map.image.h}px`, transform: `translateY(${map.image.y}px)` }"
      />
      <span v-if="map.view" class="o-scroll-view" :style="{ top: `${map.view.y}px`, height: `${map.view.h}px` }" />
      <span v-for="(g, i) in map.gaps" :key="i" class="o-scroll-gap" :style="{ top: `${g}px` }" />
      <span v-if="map.foldTop" class="o-scroll-fold" data-edge="top" />
      <span v-if="map.foldBottom" class="o-scroll-fold" data-edge="bottom" />
    </div>
    <div v-else class="o-scroll-map" data-empty :style="{ width: `${MAP_W}px`, height: `${Math.round((MAP_W * sel.h) / Math.max(1, sel.w))}px` }" />
    <div class="o-scroll-num" :data-tone="near ? 'warn' : undefined">{{ px(progress?.height ?? 0) }}</div>
    <div class="o-scroll-num">{{ second }}</div>
  </div>

  <div
    ref="barEl"
    class="g-bar o-bar o-scroll-bar"
    role="toolbar"
    :aria-label="t('action.scrollCapture')"
    :style="{ ...at(placement.bar), width: `${placement.bar.w}px` }"
    @mousedown.stop
  >
    <span
      ref="statusEl"
      class="o-scroll-status"
      role="status"
      :data-tone="status.warn ? 'warn' : undefined"
      @mouseenter="measureStatus(); statusHover = true"
      @mouseleave="statusHover = false"
    >
      <GIcon v-if="status.warn" name="bang" :size="14" />{{ status.text }}<span v-if="status.meta" class="g-muted"> · {{ status.meta }}</span>
    </span>
    <button type="button" class="g-button" :aria-pressed="progress?.auto ? 'true' : 'false'" @click="emit('auto')">
      <GIcon :name="progress?.auto ? 'pause' : progress?.autoAllowed === false ? 'lock' : 'play'" />
      {{ progress?.auto ? t("scroll.stopButton") : t("scroll.autoButton") }}<kbd class="g-kbd">Space</kbd>
    </button>
    <span class="o-scroll-space" />
    <button type="button" class="g-button" data-variant="primary" @click="emit('done')">
      {{ t("scroll.done") }}<kbd class="g-kbd">↩</kbd>
    </button>
    <span class="g-divider" role="separator" />
    <span class="g-tipwrap">
      <button type="button" class="g-btn" :aria-label="t('scroll.cancel')" @click="emit('cancel')"><GIcon name="close" /></button>
      <span class="g-tip" role="tooltip">{{ t("scroll.cancel") }}<kbd class="g-kbd">Esc</kbd></span>
    </span>
  </div>

  <div
    v-if="pop"
    ref="popEl"
    class="g-pop o-bar o-scroll-pop"
    role="dialog"
    :data-tone="pop.warn ? 'warn' : undefined"
    :style="{ ...at(popAt), '--pop-w': `${pop.w}px` }"
    @mousedown.stop
  >
    <div class="g-pop-title"><GIcon :name="pop.icon" :size="16" /><span>{{ pop.title }}</span></div>
    <p v-if="pop.text" class="g-pop-text">{{ pop.text }}</p>
    <div v-if="pop.kind === 'ask'" class="g-pop-actions">
      <button type="button" class="g-button" @click="emit('manual')">{{ t("scroll.ax.manual") }}</button>
      <button type="button" class="g-button" data-variant="primary" @click="emit('settings')">{{ t("scroll.ax.open") }}</button>
    </div>
  </div>
</template>

<style scoped>
.o-scroll-strip {
  display: flex;
  flex-direction: column;
  align-items: center;
  width: 112px;
  padding: var(--space-8) var(--space-8) var(--space-6);
  border-radius: var(--radius-card);
}
.o-scroll-map {
  position: relative;
  flex: none;
  overflow: hidden;
  border-radius: 4px;
  box-shadow: 0 0 0 var(--hairline) var(--edge-inner);
}
.o-scroll-map[data-empty] {
  max-height: 256px;
  background: var(--divider);
}
.o-scroll-image {
  position: absolute;
  left: 0;
  top: 0;
  display: block;
  max-width: none;
  user-select: none;
}
/* The part of the result the selection shows now; the rest of the strip sits under the dim, as
   the screen around the selection does. */
.o-scroll-view {
  position: absolute;
  left: 0;
  right: 0;
  z-index: 2;
  border-radius: 2px;
  box-shadow: 0 0 0 1.5px var(--iris-text), 0 0 0 999px var(--dim);
}
.o-scroll-gap {
  position: absolute;
  left: -2px;
  right: -2px;
  z-index: 3;
  height: 10px;
  background: repeating-linear-gradient(135deg, var(--warn) 0 3px, transparent 3px 6px);
  box-shadow: 0 0 0 1px var(--edge-outer);
}
.o-scroll-fold {
  position: absolute;
  left: 0;
  right: 0;
  z-index: 3;
  height: 22px;
  background: linear-gradient(180deg, var(--edge-outer), transparent);
}
.o-scroll-fold[data-edge="top"] {
  top: 0;
}
.o-scroll-fold[data-edge="bottom"] {
  bottom: 0;
  transform: scaleY(-1);
}
.o-scroll-num {
  margin-top: var(--space-6);
  font: var(--type-num-small);
  font-variant-numeric: tabular-nums;
  color: var(--text);
  text-align: center;
  white-space: nowrap;
}
.o-scroll-num + .o-scroll-num {
  margin-top: 0;
  color: var(--text-muted);
}
.o-scroll-num[data-tone="warn"] {
  color: var(--warn);
}
.o-scroll-bar {
  display: flex;
  align-items: center;
}
.o-scroll-bar > .g-button > .g-kbd {
  margin-left: 2px;
}
.o-scroll-space {
  flex: none;
  width: 4px;
}
.o-scroll-status {
  flex: 1;
  min-width: 0;
  padding: 0 var(--space-8) 0 10px;
  font: var(--type-label);
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.o-scroll-status .g-muted {
  font-variant-numeric: tabular-nums;
}
.o-scroll-status[data-tone="warn"] {
  color: var(--warn);
}
.o-scroll-status > g-icon {
  margin: -2px 6px 0 0;
  color: var(--warn);
}
.o-scroll-pop {
  position: absolute;
  z-index: 13;
}
.o-scroll-pop .g-pop-title {
  display: flex;
  gap: var(--space-8);
  align-items: flex-start;
}
.o-scroll-pop .g-pop-title > g-icon {
  flex: none;
  margin-top: -1px;
  color: var(--iris-text);
}
.o-scroll-pop[data-tone="warn"] .g-pop-title > g-icon {
  color: var(--warn);
}
/* The sticky header and bottom bar inside the selection, once found: they enter the result once. */
.o-scroll-band {
  position: absolute;
  z-index: 7;
  pointer-events: none;
  background: repeating-linear-gradient(135deg, var(--iris-soft) 0 6px, transparent 6px 12px);
}
.o-scroll-band[data-edge="top"] {
  box-shadow: inset 0 -1.5px 0 var(--iris);
}
.o-scroll-band[data-edge="bottom"] {
  box-shadow: inset 0 1.5px 0 var(--iris);
}
</style>
