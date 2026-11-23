<script setup lang="ts">
// The combine sheet (g-scrim and g-dialog, design/canvas row 13 Y1): the selected screenshots
// laid onto one image, a live preview of it on the left with the numbered order under it, and
// the layout, the gap, the alignment and the background on the right. Open in Editor hands the
// layout to `editor_combine`; nothing is written until the editor's first output, so the sheet
// opens with that button focused. The preview and the request come from one `combine()` result.
import { computed, nextTick, onMounted, reactive, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import { entryName, thumbUrl, type Entry, type EntryId } from "../lib/api/history";
import type { CombineBackground, CombineRequest } from "../lib/api/render";
import { t, type Key } from "../lib/i18n";
import {
  ALIGNS,
  arrange,
  BACKGROUND_CSS,
  BACKGROUNDS,
  combine,
  GAPS,
  LAYOUTS,
  moved,
  pieceOf,
  remembered,
  scaleText,
  type Align,
  type Layout,
  type Piece,
  type Settings,
} from "./combine";

const props = defineProps<{ entries: readonly Entry[]; busy: boolean }>();
const emit = defineEmits<{ open: [request: CombineRequest]; cancel: [] }>();

/** The preview's box inside the 360 × 206 stage. */
const BOX = { w: 328, h: 178 };

const settings = reactive<Settings>({ ...remembered });
watch(settings, (s) => Object.assign(remembered, s));

const byId = computed(() => new Map(props.entries.map((e) => [e.id, e])));
// Only screenshots take part; the order starts as the selection's (⌘-click adds in order).
const order = ref<EntryId[]>(props.entries.filter((e) => pieceOf(e)).map((e) => e.id));
// An entry deleted while the sheet is open leaves the order; nothing new joins it.
watch(byId, (ids) => (order.value = order.value.filter((id) => ids.has(id))));

const pieces = computed<Piece[]>(() => order.value.map((id) => byId.value.get(id)).map((e) => (e ? pieceOf(e) : null)).filter((p): p is Piece => !!p));
const skipped = computed(() => props.entries.filter((e) => !pieceOf(e)));

/** Where each piece lies in the free layout, in points; seeded from the layout before it. */
const free = ref(new Map<EntryId, { x: number; y: number }>());
function seedFree(from: Layout): void {
  const before = arrange(pieces.value, { ...settings, layout: from === "free" ? "row" : from });
  const next = new Map(free.value);
  pieces.value.forEach((p, i) => {
    if (!next.has(p.id)) next.set(p.id, { x: before.rects[i].x, y: before.rects[i].y });
  });
  free.value = next;
}
watch(
  () => settings.layout,
  (now, before) => {
    if (now === "free") seedFree(before);
  },
);
if (settings.layout === "free") seedFree("row");

const result = computed(() => combine(pieces.value, settings, free.value));
const k = computed(() => {
  const { width, height } = result.value.points;
  return width && height ? Math.min(BOX.w / width, BOX.h / height) : 1;
});
const r1 = (v: number) => Math.round(v * k.value * 10) / 10;
const compStyle = computed(() => {
  const bg = BACKGROUND_CSS[settings.background];
  return { width: `${r1(result.value.points.width)}px`, height: `${r1(result.value.points.height)}px`, background: bg ?? undefined };
});
const shown = computed(() =>
  pieces.value.map((p, i) => {
    const r = result.value.points.rects[i];
    const entry = byId.value.get(p.id);
    return { id: p.id, url: entry ? thumbUrl(entry) : null, style: { left: `${r1(r.x)}px`, top: `${r1(r.y)}px`, width: `${r1(r.w)}px`, height: `${r1(r.h)}px` } };
  }),
);

const isFree = computed(() => settings.layout === "free");
const canOpen = computed(() => pieces.value.length >= 2 && !result.value.tooLarge && !props.busy);
const facts = computed(() => {
  const r = result.value.request;
  return `${r.width} × ${r.height} px · ${scaleText(r.scale)}`;
});

const LAYOUT_KEY: Record<Layout, Key> = {
  row: "history.combine.layouts.row",
  col: "history.combine.layouts.col",
  grid: "history.combine.layouts.grid",
  free: "history.combine.layouts.free",
};
const LAYOUT_ICON: Record<Layout, string> = { row: "layout-row", col: "layout-col", grid: "grid", free: "layout-free" };
const BACKGROUND_KEY: Record<CombineBackground, Key> = {
  clear: "history.combine.backgrounds.clear",
  white: "history.combine.backgrounds.white",
  gray: "history.combine.backgrounds.gray",
  dark: "history.combine.backgrounds.dark",
};
const SWATCH: Record<CombineBackground, string> = {
  clear: "repeating-conic-gradient(#c9c9d0 0 25%, #ffffff 0 50%) 0 0 / 8px 8px",
  white: "#ffffff",
  gray: "#e9e9ee",
  dark: "#1c1c1e",
};
/** Alignment runs across the layout: top to bottom beside each other, left to right one under
 * the other. */
const alignments = computed<{ value: Align; icon: string; label: string }[]>(() => {
  const col = settings.layout === "col";
  const names = col ? (["left", "center", "right"] as const) : (["top", "middle", "bottom"] as const);
  return ALIGNS.map((value, i) => ({ value, icon: `align-${names[i]}`, label: t(`history.combine.aligns.${names[i]}` as Key) }));
});
const alignLabel = computed(() => alignments.value.find((a) => a.value === settings.align)?.label ?? "");

/** The one sentence under the settings: why Open is off, or what happens to the scale. */
const info = computed<{ text: string; tone: "info" | "warn" } | null>(() => {
  const r = result.value;
  if (r.tooLarge) return { text: t("history.combine.tooLargeAt1x"), tone: "warn" };
  if (pieces.value.length < 2) return { text: t("history.bulk.combineOff"), tone: "warn" };
  if (r.request.scale < r.wanted) return { text: t("history.combine.tooLarge", { scale: scaleText(r.wanted) }), tone: "info" };
  if (r.upscaled.length === 1) {
    const one = r.upscaled[0];
    const entry = byId.value.get(one.id);
    return { text: t("history.combine.upscaled", { name: entry ? entryName(entry) : "", from: scaleText(one.scale), to: scaleText(r.request.scale) }), tone: "info" };
  }
  if (r.upscaled.length > 1) return { text: t("history.combine.upscaledMany", { n: r.upscaled.length, to: scaleText(r.request.scale) }), tone: "info" };
  return null;
});
const skippedText = computed(() => {
  const s = skipped.value;
  if (!s.length) return null;
  return s.length === 1 ? t("history.combine.skipped", { name: entryName(s[0]) }) : t("history.combine.skippedMany", { n: s.length });
});

// ------------------------------------------------------------------------------ the order

const orderEl = ref<HTMLElement | null>(null);
let orderDrag: { id: EntryId; x: number; moved: boolean; pointer: number } | null = null;

function orderDown(event: PointerEvent, id: EntryId): void {
  if (event.button !== 0) return;
  orderDrag = { id, x: event.clientX, moved: false, pointer: event.pointerId };
  (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
}
function orderMove(event: PointerEvent): void {
  const d = orderDrag;
  if (!d || d.pointer !== event.pointerId) return;
  if (!d.moved && Math.abs(event.clientX - d.x) < 4) return;
  d.moved = true;
  const thumbs = [...(orderEl.value?.querySelectorAll<HTMLElement>("[data-order]") ?? [])];
  const from = order.value.indexOf(d.id);
  // The slot under the pointer: the first thumb whose centre lies right of it, else the end.
  let to = thumbs.findIndex((el) => {
    const r = el.getBoundingClientRect();
    return event.clientX < r.left + r.width / 2;
  });
  if (to < 0) to = thumbs.length - 1;
  else if (to > from) to -= 1;
  if (from >= 0 && to !== from) order.value = moved(order.value, from, to);
}
function orderUp(event: PointerEvent): void {
  if (orderDrag?.pointer === event.pointerId) orderDrag = null;
}
/** ⌥← and ⌥→ move the focused thumb, for the keyboard. */
function orderKey(event: KeyboardEvent, id: EntryId): void {
  if (!event.altKey || (event.key !== "ArrowLeft" && event.key !== "ArrowRight")) return;
  event.preventDefault();
  event.stopPropagation();
  const from = order.value.indexOf(id);
  order.value = moved(order.value, from, from + (event.key === "ArrowLeft" ? -1 : 1));
  void nextTick(() => orderEl.value?.querySelector<HTMLElement>(`[data-order="${id}"]`)?.focus());
}

// ---------------------------------------------------------------- free layout, on the preview

let pieceDrag: { id: EntryId; x: number; y: number; from: { x: number; y: number }; k: number; pointer: number } | null = null;

function pieceDown(event: PointerEvent, id: EntryId): void {
  if (!isFree.value || event.button !== 0) return;
  const from = free.value.get(id);
  if (!from) return;
  event.preventDefault();
  // The scale of the drag's start, so the piece stays under the pointer while the image grows.
  pieceDrag = { id, x: event.clientX, y: event.clientY, from, k: k.value, pointer: event.pointerId };
  (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
}
function pieceMove(event: PointerEvent): void {
  const d = pieceDrag;
  if (!d || d.pointer !== event.pointerId) return;
  const next = new Map(free.value);
  next.set(d.id, { x: d.from.x + (event.clientX - d.x) / d.k, y: d.from.y + (event.clientY - d.y) / d.k });
  free.value = next;
}
function pieceUp(event: PointerEvent): void {
  if (pieceDrag?.pointer === event.pointerId) pieceDrag = null;
}

// ------------------------------------------------------------------------------- the keys

const dialog = ref<HTMLElement | null>(null);
const go = ref<HTMLButtonElement | null>(null);
const cancelEl = ref<HTMLButtonElement | null>(null);
const titleId = `hv-combine-${Math.random().toString(36).slice(2)}`;
onMounted(() => nextTick(() => go.value?.focus()));

function open(): void {
  if (canOpen.value) emit("open", result.value.request);
}

function key(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    if (!props.busy) emit("cancel");
    return;
  }
  const bare = !event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey;
  if (bare && /^[1-4]$/.test(event.key)) {
    event.preventDefault();
    settings.layout = LAYOUTS[Number(event.key) - 1];
    return;
  }
  if (event.key === "Enter" && bare) {
    // Return on Cancel cancels; everywhere else in the sheet it opens the image.
    if (document.activeElement === cancelEl.value) return;
    event.preventDefault();
    open();
    return;
  }
  if (event.key !== "Tab" || !dialog.value) return;
  event.preventDefault();
  const items = [...dialog.value.querySelectorAll<HTMLElement>("button:not(:disabled), [tabindex='0']")];
  const index = items.indexOf(document.activeElement as HTMLElement);
  items[(index + (event.shiftKey ? -1 : 1) + items.length) % items.length]?.focus();
}
</script>

<template>
  <div class="g-scrim" @keydown="key">
    <div ref="dialog" class="g-dialog hv-combine" role="dialog" aria-modal="true" :aria-labelledby="titleId">
      <div class="g-dialog-head">
        <span class="g-dialog-icon"><GIcon name="combine" :size="20" /></span>
        <div>
          <h2 :id="titleId" class="g-dialog-title">{{ t("history.combine.title", { n: pieces.length }) }}</h2>
          <p class="g-dialog-text">{{ t("history.combine.text") }}</p>
        </div>
      </div>

      <div class="hv-combine-body">
        <div>
          <div class="hv-combine-stage">
            <div class="hv-combine-comp" :class="{ 'hv-combine-checker': settings.background === 'clear' }" :style="compStyle">
              <span
                v-for="p in shown"
                :key="p.id"
                class="hv-combine-piece"
                :data-free="isFree ? '' : undefined"
                :style="p.style"
                @pointerdown="pieceDown($event, p.id)"
                @pointermove="pieceMove"
                @pointerup="pieceUp"
                @pointercancel="pieceUp"
              >
                <img v-if="p.url" :src="p.url" alt="" draggable="false" />
              </span>
            </div>
          </div>
          <div ref="orderEl" class="hv-combine-order" :data-many="order.length > 6 ? '' : undefined">
            <span
              v-for="(p, i) in pieces"
              :key="p.id"
              class="hv-combine-thumb"
              tabindex="0"
              role="button"
              :data-order="p.id"
              :data-tip="byId.get(p.id) ? entryName(byId.get(p.id)!) : ''"
              :aria-label="`${i + 1}. ${byId.get(p.id) ? entryName(byId.get(p.id)!) : ''}`"
              @pointerdown="orderDown($event, p.id)"
              @pointermove="orderMove"
              @pointerup="orderUp"
              @pointercancel="orderUp"
              @keydown="orderKey($event, p.id)"
            >
              <img v-if="byId.get(p.id) && thumbUrl(byId.get(p.id)!)" :src="thumbUrl(byId.get(p.id)!) ?? ''" alt="" draggable="false" />
              <span class="hv-combine-num">{{ i + 1 }}</span>
            </span>
            <span v-if="pieces.length < 6 && pieces.length > 1" class="hv-combine-hint">{{ t("history.combine.order") }}</span>
          </div>
        </div>

        <div class="hv-combine-opts">
          <div class="hv-combine-opt">
            <div class="hv-combine-head">
              <b>{{ t("history.combine.layout") }}</b><span>{{ t(LAYOUT_KEY[settings.layout]) }}</span>
            </div>
            <div class="g-seg" role="radiogroup" :aria-label="t('history.combine.layout')">
              <button
                v-for="(l, i) in LAYOUTS"
                :key="l"
                type="button"
                role="radio"
                :aria-checked="settings.layout === l"
                :aria-label="t(LAYOUT_KEY[l])"
                :data-tip="t(LAYOUT_KEY[l])"
                :data-key="String(i + 1)"
                @click="settings.layout = l"
              >
                <GIcon :name="LAYOUT_ICON[l]" :size="16" />
              </button>
            </div>
          </div>

          <div class="hv-combine-opt">
            <div class="hv-combine-head">
              <b>{{ t("history.combine.gap") }}</b><span v-if="!isFree">{{ settings.gap }} pt</span>
            </div>
            <div class="g-seg hv-combine-num-seg" role="radiogroup" :aria-label="t('history.combine.gap')">
              <button
                v-for="g in GAPS"
                :key="g"
                type="button"
                role="radio"
                :aria-checked="settings.gap === g"
                :aria-disabled="isFree ? 'true' : undefined"
                @click="!isFree && (settings.gap = g)"
              >
                {{ g }}
              </button>
            </div>
          </div>

          <div class="hv-combine-opt">
            <div class="hv-combine-head">
              <b>{{ t("history.combine.align") }}</b><span v-if="!isFree">{{ alignLabel }}</span>
            </div>
            <div class="g-seg" role="radiogroup" :aria-label="t('history.combine.align')">
              <button
                v-for="a in alignments"
                :key="a.value"
                type="button"
                role="radio"
                :aria-checked="settings.align === a.value"
                :aria-label="a.label"
                :aria-disabled="isFree ? 'true' : undefined"
                :data-tip="a.label"
                @click="!isFree && (settings.align = a.value)"
              >
                <GIcon :name="a.icon" :size="16" />
              </button>
            </div>
          </div>

          <div class="hv-combine-opt">
            <div class="hv-combine-head">
              <b>{{ t("history.combine.background") }}</b><span>{{ t(BACKGROUND_KEY[settings.background]) }}</span>
            </div>
            <div class="hv-combine-swatches" role="radiogroup" :aria-label="t('history.combine.background')">
              <button
                v-for="b in BACKGROUNDS"
                :key="b"
                type="button"
                class="g-swatch"
                role="radio"
                :aria-checked="settings.background === b"
                :aria-label="t(BACKGROUND_KEY[b])"
                :data-tip="t(BACKGROUND_KEY[b])"
                :style="{ '--swatch': SWATCH[b] }"
                @click="settings.background = b"
              />
            </div>
          </div>
        </div>
      </div>

      <p v-if="skippedText" class="hv-combine-note" role="status"><GIcon name="warning" :size="16" /><span>{{ skippedText }}</span></p>
      <p v-if="info" class="hv-combine-info" :data-tone="info.tone" role="status">
        <GIcon :name="info.tone === 'warn' ? 'warning' : 'info'" :size="16" /><span>{{ info.text }}</span>
      </p>

      <div class="g-dialog-foot">
        <span class="hv-combine-facts">{{ facts }}</span>
        <span class="g-spacer" />
        <button ref="cancelEl" type="button" class="g-button" :disabled="busy" @click="emit('cancel')">{{ t("history.delete.cancel") }}</button>
        <button ref="go" type="button" class="g-button" data-variant="primary" :aria-disabled="canOpen ? undefined : 'true'" @click="open">
          {{ t("history.combine.open") }}<kbd class="g-kbd">↩</kbd>
        </button>
      </div>
    </div>
  </div>
</template>
