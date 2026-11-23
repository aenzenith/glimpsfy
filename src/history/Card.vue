<script setup lang="ts">
// One capture in the grid: the thumbnail (zoomed onto the match during a search), the marks,
// the meta line, the quoted line of a search result, the upload state and the quick actions.
import { computed, inject, onBeforeUnmount, onMounted } from "vue";

import GIcon from "../components/GIcon.vue";
import { entryName, fileUrl, thumbUrl, type HistoryItem } from "../lib/api/history";
import { t } from "../lib/i18n";
import { commandHeld } from "../lib/keys";
import { uploadsAllowed } from "../lib/network";
import { HISTORY, type HistoryContext } from "./context";
import Marked from "./Marked.vue";
import { copyable, formatBytes, formatDuration, formatTime, GRID } from "./model";
import { seriesSub, skippedMark } from "./series";
import Shot from "./Shot.vue";

const props = defineProps<{ item: HistoryItem; box: { w: number; h: number }; height: number; index: number; count: number }>();
const ctx = inject(HISTORY) as HistoryContext;

const entry = computed(() => props.item.entry);
const id = computed(() => entry.value.id);
const selected = computed(() => ctx.selection.value.ids.includes(id.value));
const match = computed(() => props.item.textMatch);
const upload = computed(() => ctx.uploads.value.get(id.value) ?? null);
const lang = ctx.lang;
// A series is drawn as a deck: its frames stack behind the newest one, which gives the deck's
// edges the top of the media box.
const series = computed(() => (entry.value.kind === "series" ? entry.value.series : null));
const isSeries = computed(() => entry.value.kind === "series");
const open = computed(() => ctx.openSeries.value === id.value);
const mediaBox = computed(() => (isSeries.value ? { w: props.box.w, h: props.box.h - GRID.deckH } : props.box));

const zoomed = computed(() => !!match.value && match.value.best != null && match.value.boxes.length > 0);
const src = computed(() => {
  if (entry.value.kind === "screenshot") return zoomed.value ? fileUrl(entry.value.id) : (thumbUrl(entry.value) ?? fileUrl(entry.value.id));
  return thumbUrl(entry.value);
});
const size = computed(() => match.value?.imageSize ?? entry.value.size);

const sub = computed(() => {
  const e = entry.value;
  if (e.kind === "series") return seriesSub(e, lang.value);
  const time = formatTime(e.createdAtMs, lang.value);
  if (e.kind === "recording") return `${time} · ${formatDuration(e.durationMs)} · ${formatBytes(e.fileBytes, lang.value)}`;
  if (e.kind === "textCopy") return `${time} · ${t("history.lines", { n: e.textLineCount })}`;
  const dims = e.size ? `${e.size.width} × ${e.size.height}` : "";
  return [time, dims, formatBytes(e.fileBytes, lang.value)].filter(Boolean).join(" · ");
});

const uploadText = computed(() => {
  const u = upload.value;
  if (!u) return "";
  if (u.state === "failed") return t("history.upload.failedLine", { reason: u.reason });
  return t("history.upload.progressLine", { host: u.host, sent: formatBytes(u.sent, lang.value), total: formatBytes(u.total, lang.value) });
});
const percent = computed(() => {
  const u = upload.value;
  return u && u.state === "uploading" && u.total > 0 ? Math.round((u.sent / u.total) * 100) : 0;
});

const preview = computed(() => (ctx.previews.value.get(id.value) ?? "").split("\n").filter((l) => l.trim()).slice(0, 6));
onMounted(() => {
  if (entry.value.kind === "textCopy") ctx.act.wantPreview(id.value);
});

// The drag of the file starts once the pointer moves 4 px with the button down.
let start: { x: number; y: number } | null = null;
function down(event: MouseEvent): void {
  if (event.button !== 0 || (event.target as HTMLElement).closest("button")) return;
  // ⇧ would otherwise extend the page's text selection across the cards.
  if (event.shiftKey) event.preventDefault();
  start = { x: event.clientX, y: event.clientY };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up, { once: true });
}
function move(event: MouseEvent): void {
  if (!start || event.buttons !== 1) return up();
  if (Math.hypot(event.clientX - start.x, event.clientY - start.y) < 4) return;
  up();
  // A series is a folder of frames, which its own menu shows in the file manager instead.
  if (entry.value.kind !== "textCopy" && entry.value.kind !== "series") ctx.act.dragOut(id.value);
}
function up(): void {
  start = null;
  window.removeEventListener("mousemove", move);
}
onBeforeUnmount(up);

function click(event: MouseEvent): void {
  if ((event.target as HTMLElement).closest("button")) return;
  const mode = event.shiftKey ? "range" : commandHeld(event) ? "toggle" : "one";
  ctx.act.pick(id.value, mode);
  // A plain click on a series opens its frames under the row; its chevron closes them again.
  if (mode === "one" && isSeries.value && !open.value) ctx.act.toggleSeries(id.value);
}
function more(event: MouseEvent): void {
  const r = (event.currentTarget as HTMLElement).getBoundingClientRect();
  ctx.act.menu(id.value, { x: r.left, y: r.bottom + 4 });
}
</script>

<template>
  <div
    class="g-card"
    :class="{ 'hv-series': isSeries }"
    role="option"
    :style="{ height: `${height}px` }"
    :aria-selected="selected"
    :aria-posinset="index + 1"
    :aria-setsize="count"
    :tabindex="ctx.focusId.value === id ? 0 : -1"
    :data-id="id"
    @mousedown="down"
    @click="click"
    @dblclick="ctx.act.open(id)"
    @contextmenu.prevent="ctx.act.menu(id, { x: $event.clientX, y: $event.clientY })"
  >
    <div v-if="isSeries" class="hv-deck" aria-hidden="true"><i></i><i></i></div>
    <div class="g-card-media">
      <div v-if="entry.kind === 'textCopy'" class="hv-textcard">
        <div>{{ entryName(entry) }}</div>
        <div v-for="(line, i) in preview" :key="i">{{ line }}</div>
      </div>
      <Shot
        v-else
        :src="src"
        :size="size"
        mode="cover"
        :box="mediaBox"
        :boxes="match?.boxes"
        :current="match?.best"
        :zoom="zoomed"
        :scale="entry.scale"
      >
        <template #missing>
          <div class="hv-placeholder"><GIcon :name="entry.kind === 'recording' ? 'film' : isSeries ? 'grid' : 'image'" :size="24" /></div>
        </template>
      </Shot>
      <span v-if="entry.kind === 'recording'" class="g-tag"><GIcon name="play" :size="12" />{{ formatDuration(entry.durationMs) }}</span>
      <span v-else-if="series" class="g-tag"><GIcon name="grid" :size="12" />{{ series.count }}</span>
      <div v-if="upload?.state === 'uploading'" class="hv-media-state g-glass" data-material="strong" role="status">
        <svg class="g-ring" viewBox="0 0 22 22" :style="{ '--progress': percent }"><circle cx="11" cy="11" r="8" pathLength="100" /><circle cx="11" cy="11" r="8" pathLength="100" /></svg>
        <span class="hv-ms-text">{{ percent }} %</span>
        <button type="button" class="g-btn" data-size="small" :aria-label="t('history.upload.cancel')" :data-tip="t('history.upload.cancel')" @click="ctx.act.cancelUpload(id)">
          <GIcon name="close" :size="16" />
        </button>
      </div>
      <div v-else-if="upload?.state === 'failed'" class="hv-media-state g-glass" data-material="strong" role="status">
        <span class="g-status" data-status="warn"><GIcon name="bang" :size="12" /></span>
        <span class="hv-ms-text">{{ t("history.upload.notUploaded") }}</span>
        <button type="button" class="g-button" :data-tip="t('history.upload.retry')" data-key="⌘U" @click="ctx.act.upload([id])">
          <GIcon name="retry" :size="16" />{{ t("history.upload.retryShort") }}
        </button>
      </div>
      <div v-else-if="isSeries" class="g-card-actions g-glass" role="toolbar" :aria-label="t('history.card.actions')">
        <button
          type="button"
          class="g-btn"
          data-size="small"
          :aria-label="t(open ? 'history.series.close' : 'history.series.open')"
          :aria-expanded="open"
          :data-tip="t(open ? 'history.series.close' : 'history.series.open')"
          @click="ctx.act.toggleSeries(id)"
        >
          <GIcon :name="open ? 'chevron-up' : 'chevron-down'" :size="16" />
        </button>
        <button
          type="button"
          class="g-btn"
          data-size="small"
          :aria-label="t('history.series.copyLast')"
          :data-tip="t('history.series.copyLast')"
          data-key="⌘C"
          @click="ctx.act.copy(id)"
        >
          <GIcon name="copy" :size="16" />
        </button>
        <button type="button" class="g-btn" data-size="small" :aria-label="t('history.action.more')" :data-tip="t('history.action.more')" @click="more">
          <GIcon name="more" :size="16" />
        </button>
      </div>
      <div v-else class="g-card-actions g-glass" role="toolbar" :aria-label="t('history.card.actions')">
        <button
          v-if="entry.kind !== 'textCopy' && copyable(entry)"
          type="button"
          class="g-btn"
          data-size="small"
          :aria-label="t('history.action.copy')"
          :data-tip="t('history.action.copy')"
          data-key="⌘C"
          @click="ctx.act.copy(id)"
        >
          <GIcon name="copy" :size="16" />
        </button>
        <button
          v-if="entry.hosted"
          type="button"
          class="g-btn"
          data-size="small"
          :aria-label="t('history.action.copyLink')"
          :data-tip="t('history.action.copyLink')"
          data-key="⇧⌘L"
          @click="ctx.act.copyLink(id)"
        >
          <GIcon name="link" :size="16" />
        </button>
        <button
          v-else-if="entry.kind !== 'textCopy' && uploadsAllowed"
          type="button"
          class="g-btn"
          data-size="small"
          :aria-label="t('history.action.upload')"
          :data-tip="t('history.action.upload')"
          data-key="⌘U"
          @click="ctx.act.upload([id])"
        >
          <GIcon name="upload" :size="16" />
        </button>
        <button type="button" class="g-btn" data-size="small" :aria-label="t('history.action.more')" :data-tip="t('history.action.more')" @click="more">
          <GIcon name="more" :size="16" />
        </button>
      </div>
    </div>
    <div class="g-card-meta">
      <div class="g-card-text">
        <span class="hv-titlerow">
          <span class="g-card-title"><Marked :text="entryName(entry)" :query="ctx.query.value" /></span>
          <span v-if="entry.pinned || entry.hosted || entry.textLineCount > 0 || (series?.skipped ?? 0) > 0" class="hv-marks">
            <span v-if="entry.pinned" class="hv-mark" data-tone="iris" role="img" :aria-label="t('history.mark.pinned')"><GIcon name="pin" :size="12" /></span>
            <span v-if="entry.hosted && !upload" class="hv-mark" data-tone="ok" role="img" :aria-label="t('history.mark.uploaded')"><GIcon name="link" :size="12" /></span>
            <span v-if="series && series.skipped > 0" class="hv-mark" data-tone="warn" role="img" :aria-label="skippedMark(series.skipped)" :data-tip="skippedMark(series.skipped)">
              <GIcon name="bang" :size="12" />
            </span>
            <span v-if="entry.kind === 'screenshot' && entry.textLineCount > 0" class="hv-mark" role="img" :aria-label="t('history.mark.text', { n: entry.textLineCount })">
              <GIcon name="ocr" :size="12" />
            </span>
          </span>
        </span>
        <span class="g-card-sub"><span v-if="upload" :class="{ 'hv-warn': upload.state === 'failed' }">{{ uploadText }}</span><template v-else>{{ sub }}</template></span>
      </div>
    </div>
    <div v-if="match" class="hv-snip"><Marked :text="match.snippet" :ranges="match.highlights" /></div>
  </div>
</template>
