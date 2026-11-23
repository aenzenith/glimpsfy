<script setup lang="ts">
// The frames of the open series under its card's row: its name, counts and folder, the export
// menu, and as many frame tiles as the row has room for, the rest folded into one "+N" tile.
// A frame is a file in the series' folder, so a tile shows it there rather than open an editor.
import { homeDir } from "@tauri-apps/api/path";
import { computed, inject, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import { entryName, frameUrl, history_series_frames, type HistoryItem, type SeriesFrame } from "../lib/api/history";
import { t, tp } from "../lib/i18n";
import { HISTORY, type HistoryContext } from "./context";
import { folderTrail, formatClock, framePath, GRID } from "./model";
import { seriesMeta } from "./series";

const props = defineProps<{ item: HistoryItem; notch: number; width: number }>();
const ctx = inject(HISTORY) as HistoryContext;

const home = ref<string | null>(null);
void homeDir()
  .then((path) => (home.value = path))
  .catch(() => {});

const entry = computed(() => props.item.entry);
const id = computed(() => entry.value.id);
const folder = computed(() => entry.value.savedPath ?? "");
const trail = computed(() => (folder.value ? folderTrail(folder.value, home.value) : ""));

// The frames are read again as the series grows, so a running series fills its band.
const frames = ref<SeriesFrame[]>([]);
watch(
  () => [id.value, entry.value.series?.count ?? 0] as const,
  ([entryId]) => {
    history_series_frames(entryId)
      .then((list) => {
        if (entryId === id.value) frames.value = list;
      })
      .catch(() => {});
  },
  { immediate: true },
);

/** The tiles the band's width holds (12 px of padding on each side), the last one "+N" when
 * the series has more frames than that. */
const room = computed(() => Math.max(2, Math.floor((props.width - 24 + GRID.tileGap) / (GRID.tileW + GRID.tileGap))));
const shown = computed(() => (frames.value.length > room.value ? frames.value.slice(0, room.value - 1) : frames.value));
const rest = computed(() => frames.value.length - shown.value.length);
const last = computed(() => frames.value[frames.value.length - 1] ?? null);

const clock = (ms: number) => formatClock(ms, ctx.lang.value);

function reveal(frame: SeriesFrame): void {
  if (frame.file && folder.value) ctx.act.revealPath(framePath(folder.value, frame.file));
}
function exportMenu(event: MouseEvent): void {
  const r = (event.currentTarget as HTMLElement).getBoundingClientRect();
  ctx.act.seriesMenu(id.value, { x: r.left, y: r.bottom + 4 });
}
</script>

<template>
  <section class="hv-band" :style="{ '--notch': `${notch}px` }" :aria-label="t('history.series.band', { name: entryName(entry) })">
    <div class="hv-band-head">
      <b>{{ entryName(entry) }}</b>
      <span class="hv-bmeta">{{ seriesMeta(entry) }}</span>
      <button v-if="folder" type="button" class="hv-path" :data-tip="tp('history.series.revealFolder')" data-key="⌥⌘R" @click="ctx.act.reveal(id)">
        <GIcon name="folder" :size="14" /><span>{{ trail }}</span>
      </button>
      <span class="g-spacer"></span>
      <button type="button" class="g-button" aria-haspopup="menu" @click="exportMenu">{{ t("history.series.export") }}</button>
      <button
        type="button"
        class="g-btn"
        data-size="small"
        :aria-label="t('history.series.close')"
        :data-tip="t('history.series.close')"
        aria-expanded="true"
        @click="ctx.act.toggleSeries(id)"
      >
        <GIcon name="chevron-up" :size="16" />
      </button>
    </div>
    <div class="hv-frames">
      <template v-for="frame in shown" :key="frame.index">
        <div v-if="!frame.file" class="hv-fr" data-skip>
          <span class="hv-fr-media" role="img" :aria-label="`${clock(frame.takenAtMs)} · ${t('history.series.skippedTile')}`"><GIcon name="bang" :size="16" /></span>
          <span class="hv-fr-time">{{ t("history.series.skippedTile") }}</span>
        </div>
        <button v-else type="button" class="hv-fr" :data-tip="tp('history.series.revealFrame')" @click="reveal(frame)">
          <span class="hv-fr-media"><img :src="frameUrl(id, frame.index)" alt="" loading="lazy" decoding="async" draggable="false" /></span>
          <span class="hv-fr-time">{{ clock(frame.takenAtMs) }}</span>
        </button>
      </template>
      <button v-if="rest > 0 && last" type="button" class="hv-fr" data-more :aria-label="t('history.series.more', { n: rest })" :data-tip="tp('history.series.revealFolder')" @click="ctx.act.reveal(id)">
        <span class="hv-fr-media">+{{ rest }}</span>
        <span class="hv-fr-time">{{ t("history.series.until", { time: clock(last.takenAtMs) }) }}</span>
      </button>
    </div>
  </section>
</template>
