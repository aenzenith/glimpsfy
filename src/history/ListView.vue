<script setup lang="ts">
// The list view: 32 px rows with a 36 × 24 thumbnail, virtualised with two spacer rows, and the
// Name, Size and Captured columns sort.
import { computed, inject, nextTick, onBeforeUnmount, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import type { EntryId, HistoryItem, SortOrder } from "../lib/api/history";
import { entryName, thumbUrl } from "../lib/api/history";
import { t } from "../lib/i18n";
import AppIcon from "./AppIcon.vue";
import { HISTORY, type HistoryContext } from "./context";
import Marked from "./Marked.vue";
import { extensionOf, formatBytes, formatDuration, formatWhen } from "./model";

const props = defineProps<{ items: readonly HistoryItem[]; sort: SortOrder; extraBottom: number }>();
const emit = defineEmits<{ sort: [sort: SortOrder] }>();
const ctx = inject(HISTORY) as HistoryContext;

const ROW = 32;
const HEAD = 34;
const scroller = ref<HTMLElement | null>(null);
const top = ref(0);
const viewH = ref(0);
const width = ref(0);
let observer: ResizeObserver | null = null;

onMounted(() => {
  if (!scroller.value) return;
  observer = new ResizeObserver(([entry]) => {
    viewH.value = entry.contentRect.height;
    width.value = entry.contentRect.width;
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

const OVERSCAN = 20;
const range = computed(() => {
  const first = Math.max(0, Math.floor((top.value - HEAD) / ROW) - OVERSCAN);
  const last = Math.min(props.items.length, Math.ceil((top.value + viewH.value) / ROW) + OVERSCAN);
  return { first, last };
});
const rows = computed(() => props.items.slice(range.value.first, range.value.last));
const order = computed(() => props.items.map((i) => i.entry.id));

// Fixed columns give way to the name when the list is narrow (an open inspector, a small
// window): the link goes first, then the kind, the source and the dimensions.
type Column = "source" | "kind" | "dims" | "size" | "date" | "link";
const WIDTHS: Record<Column, number> = { source: 140, kind: 76, dims: 104, size: 78, date: 150, link: 92 };
const DROP: Column[] = ["link", "kind", "source", "dims"];
const NAME_MIN = 220;
const shown = computed(() => {
  const on = new Set<Column>(Object.keys(WIDTHS) as Column[]);
  const room = width.value - 24;
  const used = () => [...on].reduce((sum, c) => sum + WIDTHS[c], 0);
  for (const c of DROP) {
    if (!width.value || used() + NAME_MIN <= room) break;
    on.delete(c);
  }
  return on;
});
const span = computed(() => shown.value.size + 1);

const words = computed(() => ({ today: t("history.today"), yesterday: t("history.yesterday") }));

function measure(item: HistoryItem): string {
  const e = item.entry;
  if (e.kind === "recording") return formatDuration(e.durationMs);
  if (e.kind === "textCopy") return e.textLineCount === 1 ? t("history.lines.one") : t("history.lines", { n: e.textLineCount });
  return e.size ? `${e.size.width} × ${e.size.height}` : "—";
}
function kind(item: HistoryItem): string {
  return item.entry.kind === "textCopy" ? t("history.kind.text") : extensionOf(item.entry);
}

function click(event: MouseEvent, id: EntryId): void {
  if (event.shiftKey) event.preventDefault();
  ctx.act.pick(id, event.shiftKey ? "range" : event.metaKey ? "toggle" : "one");
}

let start: { x: number; y: number; id: EntryId } | null = null;
function down(event: MouseEvent, id: EntryId): void {
  if (event.button !== 0) return;
  if (event.shiftKey) event.preventDefault();
  start = { x: event.clientX, y: event.clientY, id };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up, { once: true });
}
function move(event: MouseEvent): void {
  if (!start || event.buttons !== 1) return up();
  if (Math.hypot(event.clientX - start.x, event.clientY - start.y) < 4) return;
  const id = start.id;
  up();
  ctx.act.dragOut(id);
}
function up(): void {
  start = null;
  window.removeEventListener("mousemove", move);
}
onBeforeUnmount(up);

function sortBy(column: "name" | "size" | "date"): void {
  if (column === "name") emit("sort", "name");
  else if (column === "size") emit("sort", "size");
  else emit("sort", props.sort === "newest" ? "oldest" : "newest");
}
function ariaSort(column: "name" | "size" | "date"): "ascending" | "descending" | "none" {
  if (column === "name" && props.sort === "name") return "ascending";
  if (column === "size" && props.sort === "size") return "descending";
  if (column === "date" && (props.sort === "newest" || props.sort === "oldest")) return props.sort === "newest" ? "descending" : "ascending";
  return "none";
}

async function reveal(id: EntryId, focus = true): Promise<void> {
  const index = order.value.indexOf(id);
  const el = scroller.value;
  if (index < 0 || !el) return;
  const y = HEAD + 6 + index * ROW;
  if (y - HEAD < el.scrollTop) el.scrollTop = y - HEAD;
  else if (y + ROW > el.scrollTop + el.clientHeight) el.scrollTop = y + ROW - el.clientHeight;
  top.value = el.scrollTop;
  if (!focus) return;
  await nextTick();
  el.querySelector<HTMLElement>(`[data-id="${CSS.escape(id)}"]`)?.focus({ preventScroll: true });
}

defineExpose({ order, reveal });
</script>

<template>
  <div ref="scroller" class="hv-scroll" @scroll.passive="scrolled">
    <div class="hv-tablewrap" :style="{ paddingBottom: `${84 + extraBottom}px` }">
      <table class="g-table hv-table" role="grid" aria-multiselectable="true" :aria-label="t('history.feed.label')" :aria-rowcount="items.length + 1">
        <colgroup>
          <col style="width: auto" />
          <template v-for="(w, c) in WIDTHS" :key="c"><col v-if="shown.has(c)" :style="{ width: `${w}px` }" /></template>
        </colgroup>
        <thead>
          <tr>
            <th :aria-sort="ariaSort('name')">
              <button type="button" class="hv-sortable" :aria-sort="ariaSort('name')" @click="sortBy('name')">
                {{ t("history.col.name") }}<GIcon v-if="ariaSort('name') !== 'none'" name="chevron-up" :size="14" />
              </button>
            </th>
            <th v-if="shown.has('source')">{{ t("history.col.source") }}</th>
            <th v-if="shown.has('kind')">{{ t("history.col.kind") }}</th>
            <th v-if="shown.has('dims')" data-align="end">{{ t("history.col.dimensions") }}</th>
            <th data-align="end" :aria-sort="ariaSort('size')">
              <button type="button" class="hv-sortable" :aria-sort="ariaSort('size')" @click="sortBy('size')">
                {{ t("history.col.size") }}<GIcon v-if="ariaSort('size') !== 'none'" name="chevron-down" :size="14" />
              </button>
            </th>
            <th :aria-sort="ariaSort('date')">
              <button type="button" class="hv-sortable" :aria-sort="ariaSort('date')" @click="sortBy('date')">
                {{ t("history.col.captured") }}<GIcon v-if="ariaSort('date') !== 'none'" :name="sort === 'oldest' ? 'chevron-up' : 'chevron-down'" :size="14" />
              </button>
            </th>
            <th v-if="shown.has('link')">{{ t("history.col.link") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr class="hv-spacer" aria-hidden="true"><td :colspan="span" :style="{ height: `${range.first * ROW}px` }" /></tr>
          <tr
            v-for="(item, i) in rows"
            :key="item.entry.id"
            :aria-selected="ctx.selection.value.ids.includes(item.entry.id)"
            :aria-rowindex="range.first + i + 2"
            :tabindex="ctx.focusId.value === item.entry.id ? 0 : -1"
            :data-id="item.entry.id"
            @mousedown="down($event, item.entry.id)"
            @click="click($event, item.entry.id)"
            @dblclick="ctx.act.open(item.entry.id)"
            @contextmenu.prevent="ctx.act.menu(item.entry.id, { x: $event.clientX, y: $event.clientY })"
          >
            <td>
              <span class="hv-namecell">
                <span class="hv-tthumb"><img v-if="thumbUrl(item.entry)" :src="thumbUrl(item.entry) ?? ''" alt="" draggable="false" loading="lazy" /></span>
                <span><Marked :text="entryName(item.entry)" :query="ctx.query.value" /></span>
              </span>
            </td>
            <td v-if="shown.has('source')">
              <span v-if="item.entry.source.appName" class="hv-src"><AppIcon :name="item.entry.source.appName" :size="14" />{{ item.entry.source.appName }}</span>
              <span v-else class="g-cell-sub">—</span>
            </td>
            <td v-if="shown.has('kind')" class="g-cell-sub">{{ kind(item) }}</td>
            <td v-if="shown.has('dims')" class="g-num">{{ measure(item) }}</td>
            <td class="g-num">{{ formatBytes(item.entry.fileBytes, ctx.lang.value) }}</td>
            <td class="g-cell-sub">{{ formatWhen(item.entry.createdAtMs, ctx.now.value, ctx.lang.value, words) }}</td>
            <td v-if="shown.has('link')" class="g-cell-sub">
              <span v-if="item.entry.hosted" class="hv-src"><GIcon name="link" :size="14" />{{ ctx.hostName(item.entry.hosted.provider) }}</span>
              <template v-else>—</template>
            </td>
          </tr>
          <tr class="hv-spacer" aria-hidden="true"><td :colspan="span" :style="{ height: `${(items.length - range.last) * ROW}px` }" /></tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
