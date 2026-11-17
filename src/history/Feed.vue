<script setup lang="ts">
// The grid of captures, virtualised: the layout (model.gridLayout) places every row, and only
// the rows near the view are in the page, so ten thousand entries scroll like fifty. Day headers
// stay at the top while their day scrolls under them, and the next one pushes the last away.
import { computed, inject, nextTick, onBeforeUnmount, onMounted, ref } from "vue";

import type { EntryId, HistoryItem } from "../lib/api/history";
import { t } from "../lib/i18n";
import Card from "./Card.vue";
import { HISTORY, type HistoryContext } from "./context";
import { dayLabel, GRID, gridLayout, rowAt, type GridLayout } from "./model";

const props = defineProps<{
  items: readonly HistoryItem[];
  total: number;
  zoom: number;
  grouped: boolean;
  /** The search, when the grid shows its results. */
  results: string | null;
  /** Room under the last row for the bulk bar. */
  extraBottom: number;
}>();

const ctx = inject(HISTORY) as HistoryContext;
const scroller = ref<HTMLElement | null>(null);
const width = ref(0);
const viewH = ref(0);
const top = ref(0);
let observer: ResizeObserver | null = null;

onMounted(() => {
  if (!scroller.value) return;
  observer = new ResizeObserver(([entry]) => {
    width.value = entry.contentRect.width;
    viewH.value = entry.contentRect.height;
  });
  observer.observe(scroller.value);
});
onBeforeUnmount(() => {
  observer?.disconnect();
});

// Vue renders once per tick however many scroll events arrive, so the position is read at once.
function scrolled(): void {
  top.value = scroller.value?.scrollTop ?? 0;
}

const layout = computed<GridLayout>(() =>
  gridLayout(props.items, { width: width.value, zoom: props.zoom, grouped: props.grouped, results: !!props.results, extraBottom: props.extraBottom }),
);

const OVERSCAN = 600;
const visible = computed(() => {
  const rows = layout.value.rows;
  if (!rows.length) return [];
  const first = rowAt(rows, Math.max(0, top.value - OVERSCAN));
  const out = [];
  for (let i = first; i < rows.length && rows[i].y < top.value + viewH.value + OVERSCAN; i++) out.push(rows[i]);
  return out;
});

/** The header of the day at the top of the view, pushed up by the next one. */
const stuck = computed(() => {
  const rows = layout.value.rows;
  if (!props.grouped || !rows.length) return null;
  let current = null;
  let next = null;
  for (let i = rowAt(rows, top.value); i >= 0; i--) {
    if (rows[i].kind === "head" && rows[i].y <= top.value) {
      current = rows[i];
      break;
    }
  }
  if (!current || current.kind !== "head") return null;
  for (let i = rows.indexOf(current) + 1; i < rows.length; i++) {
    if (rows[i].kind === "head") {
      next = rows[i];
      break;
    }
  }
  const offset = next ? Math.min(0, next.y - top.value - GRID.headH) : 0;
  return { row: current, offset };
});

const words = computed(() => ({ today: t("history.today"), yesterday: t("history.yesterday") }));
function day(ms: number) {
  return dayLabel(ms, ctx.now.value, ctx.lang.value, words.value);
}

/** Scrolls an entry into view and focuses its card once it is in the page. */
async function reveal(id: EntryId, focus = true): Promise<void> {
  const at = layout.value.where.get(id);
  const el = scroller.value;
  if (!at || !el) return;
  const row = layout.value.rows[at.row];
  const headroom = props.grouped ? GRID.headH : 0;
  if (row.y - headroom < el.scrollTop) el.scrollTop = Math.max(0, row.y - headroom);
  else if (row.y + row.h > el.scrollTop + el.clientHeight) el.scrollTop = row.y + row.h - el.clientHeight;
  top.value = el.scrollTop;
  if (!focus) return;
  await nextTick();
  el.querySelector<HTMLElement>(`[data-id="${CSS.escape(id)}"]`)?.focus({ preventScroll: true });
}

function toTop(): void {
  if (scroller.value) scroller.value.scrollTop = 0;
  top.value = 0;
}

defineExpose({ layout, reveal, toTop });
</script>

<template>
  <div ref="scroller" class="hv-scroll" role="listbox" aria-multiselectable="true" :aria-label="t('history.feed.label')" @scroll.passive="scrolled">
    <div v-if="stuck" class="hv-stick" aria-hidden="true">
      <div class="hv-day" :style="{ top: `${stuck.offset}px` }">
        <span class="hv-day-title">{{ day(stuck.row.kind === "head" ? stuck.row.dayMs : 0).title }}</span>
        <span class="hv-day-meta">{{ day(stuck.row.kind === "head" ? stuck.row.dayMs : 0).meta }}</span>
        <span class="hv-day-count">{{ stuck.row.kind === "head" ? stuck.row.count : "" }}</span>
      </div>
    </div>
    <div class="hv-sizer" :style="{ height: `${layout.height}px` }">
      <template v-for="row in visible" :key="row.key">
        <div v-if="row.kind === 'head'" class="hv-row hv-day" role="presentation" :style="{ top: `${row.y}px` }">
          <span class="hv-day-title">{{ day(row.dayMs).title }}</span>
          <span class="hv-day-meta">{{ day(row.dayMs).meta }}</span>
          <span class="hv-day-count">{{ row.count }}</span>
        </div>
        <div v-else-if="row.kind === 'results'" class="hv-row hv-results" role="status" :style="{ top: `${row.y}px` }">
          <b>{{ total === 1 ? t("history.results.one") : t("history.results.other", { n: total }) }}</b><span>{{ t("history.results.where", { query: results ?? "" }) }}</span>
        </div>
        <div v-else class="hv-row hv-cards" role="presentation" :style="{ top: `${row.y}px`, gridTemplateColumns: `repeat(${layout.cols}, minmax(0, 1fr))` }">
          <Card
            v-for="(item, i) in row.items"
            :key="item.entry.id"
            :item="item"
            :box="layout.media"
            :height="layout.cardH"
            :index="row.start + i"
            :count="total"
          />
        </div>
      </template>
    </div>
  </div>
</template>
