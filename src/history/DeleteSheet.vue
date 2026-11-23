<script setup lang="ts">
// The delete sheet (g-scrim and g-dialog): the file the user saved (to the Trash, on by default)
// and the hosted copy (off by default, with its warning) are two choices, and the summary
// sentence and the button follow them, so the user reads what is about to go before it goes.
import { computed, inject, nextTick, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import { entryName, thumbUrl, type DeleteOptions, type Entry } from "../lib/api/history";
import { t, tp } from "../lib/i18n";
import { HISTORY, type HistoryContext } from "./context";
import { fileName } from "./model";

const props = defineProps<{ entries: readonly Entry[]; busy: boolean }>();
const ctx = inject(HISTORY) as HistoryContext;
const emit = defineEmits<{ confirm: [options: DeleteOptions]; cancel: [] }>();

const many = computed(() => props.entries.length > 1);
const first = computed(() => props.entries[0]);
// A series' saved path is its folder of frames, which goes to the Trash with the series every
// time (the store does that), so it is not one of the choices.
const series = computed(() => props.entries.filter((e) => e.kind === "series"));
const oneSeries = computed(() => !many.value && first.value?.kind === "series");
const saved = computed(() => props.entries.filter((e) => e.savedPath && e.kind !== "series"));
const uploaded = computed(() => props.entries.filter((e) => e.hosted));
const host = computed(() => [...new Set(uploaded.value.map((e) => (e.hosted ? ctx.hostName(e.hosted.provider) : "")))].filter(Boolean).join(", "));

const file = ref(true);
const hosted = ref(false);
const fileOn = computed(() => saved.value.length > 0 && file.value);
const hostOn = computed(() => uploaded.value.length > 0 && hosted.value);

const summary = computed(() => {
  if (oneSeries.value) return tp("history.series.deleteSummary");
  const rest = ownSummary.value;
  return many.value && series.value.length ? `${rest} ${tp("history.series.deleteSummaryMany", { n: series.value.length })}` : rest;
});

const ownSummary = computed(() => {
  const h = host.value;
  const n = props.entries.length;
  const s = saved.value.length > 0;
  const u = uploaded.value.length > 0;
  if (many.value) {
    if (fileOn.value && hostOn.value) return tp("history.delete.many.both", { n, host: h });
    if (fileOn.value) return u ? tp("history.delete.many.fileKeepHost", { n, host: h }) : tp("history.delete.many.file", { n });
    if (hostOn.value) return s ? t("history.delete.many.hostKeepFile", { n, host: h }) : tp("history.delete.many.host", { n, host: h });
    return s || u ? t("history.delete.many.none", { n }) : tp("history.delete.many.own", { n });
  }
  if (s && u) {
    if (fileOn.value && hostOn.value) return tp("history.delete.both");
    if (fileOn.value) return tp("history.delete.fileOnly", { host: h });
    if (hostOn.value) return t("history.delete.hostOnly", { host: h });
    return t("history.delete.none");
  }
  if (s) return fileOn.value ? tp("history.delete.file") : t("history.delete.keepFile");
  if (u) return hostOn.value ? tp("history.delete.ownAndHost", { host: h }) : tp("history.delete.ownKeepHost", { host: h });
  return tp("history.delete.own");
});

const label = computed(() => {
  if (oneSeries.value) return tp("history.series.deleteLabel");
  const s = saved.value.length > 0;
  const u = uploaded.value.length > 0;
  if (fileOn.value && hostOn.value) return t("history.delete.label.everywhere");
  if (hostOn.value) return s ? t("history.delete.label.host", { host: host.value }) : t("history.delete.label.everywhere");
  if (fileOn.value) return tp("history.delete.label.mac");
  if (s || u) return t("history.delete.label.history");
  return tp("history.delete.label.mac");
});

const dialog = ref<HTMLElement | null>(null);
const go = ref<HTMLButtonElement | null>(null);
const titleId = `hv-del-${Math.random().toString(36).slice(2)}`;
onMounted(() => nextTick(() => go.value?.focus()));

function key(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    if (!props.busy) emit("cancel");
    return;
  }
  if (event.key !== "Tab" || !dialog.value) return;
  event.preventDefault();
  const items = [...dialog.value.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)")];
  const index = items.indexOf(document.activeElement as HTMLElement);
  items[(index + (event.shiftKey ? -1 : 1) + items.length) % items.length]?.focus();
}

function confirm(): void {
  emit("confirm", { trashSavedFile: fileOn.value, deleteHostedCopy: hostOn.value });
}
</script>

<template>
  <div class="g-scrim" @keydown="key">
    <div ref="dialog" class="g-dialog hv-sheet" role="alertdialog" aria-modal="true" :aria-labelledby="titleId">
      <div class="g-dialog-head">
        <span v-if="!many && first && thumbUrl(first)" class="g-dialog-thumb"><img :src="thumbUrl(first) ?? ''" alt="" style="width: 100%; height: 100%; object-fit: cover" /></span>
        <span v-else class="g-dialog-icon" data-tone="danger"><GIcon name="trash" :size="24" /></span>
        <div>
          <h2 :id="titleId" class="g-dialog-title">
            {{ many ? t("history.delete.titleMany", { n: entries.length }) : oneSeries ? t("history.series.deleteTitle") : t("history.delete.title") }}
          </h2>
          <p class="g-dialog-text">
            {{
              many
                ? t("history.delete.leadMany", { n: entries.length })
                : t(oneSeries ? "history.series.deleteLead" : "history.delete.lead", { name: first ? entryName(first) : "" })
            }}
          </p>
        </div>
      </div>
      <div v-if="saved.length || uploaded.length" class="hv-choices">
        <label v-if="saved.length" class="hv-choice">
          <input v-model="file" type="checkbox" class="g-checkbox" />
          <span>
            <span>{{ saved.length > 1 ? tp("history.delete.fileChoiceMany", { n: saved.length }) : tp("history.delete.fileChoice") }}</span>
            <span class="g-help">{{ saved.length > 1 ? tp("history.delete.fileHelpMany") : fileName(saved[0].savedPath) }}</span>
          </span>
        </label>
        <label v-if="uploaded.length" class="hv-choice">
          <input v-model="hosted" type="checkbox" class="g-checkbox" />
          <span>
            <span>{{ uploaded.length > 1 ? t("history.delete.hostChoiceMany", { n: uploaded.length, host }) : t("history.delete.hostChoice", { host }) }}</span>
            <span class="g-help">{{ t("history.delete.hostHelp") }}</span>
          </span>
        </label>
      </div>
      <p class="hv-summary" role="status"><GIcon name="info" :size="16" /><span>{{ summary }}</span></p>
      <div class="g-dialog-foot">
        <button type="button" class="g-button" :disabled="busy" @click="emit('cancel')">{{ t("history.delete.cancel") }}</button>
        <button ref="go" type="button" class="g-button" data-variant="destructive" :disabled="busy" @click="confirm">{{ label }}</button>
      </div>
    </div>
  </div>
</template>
