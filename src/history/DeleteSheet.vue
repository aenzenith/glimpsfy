<script setup lang="ts">
// The delete sheet (g-scrim and g-dialog): the file the user saved (to the Trash, on by default)
// and the hosted copy (off by default, with its warning) are two choices, and the summary
// sentence and the button follow them, so the user reads what is about to go before it goes.
// `deletePlan.ts` decides all of that; the sheet only shows it.
import { computed, inject, nextTick, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import { entryName, thumbUrl, type DeleteOptions, type Entry } from "../lib/api/history";
import { t, tp } from "../lib/i18n";
import { online, uploadsAllowed } from "../lib/network";
import { HISTORY, type HistoryContext } from "./context";
import { deletePlan } from "./deletePlan";
import { fileName } from "./model";

const props = defineProps<{ entries: readonly Entry[]; busy: boolean }>();
const ctx = inject(HISTORY) as HistoryContext;
const emit = defineEmits<{ confirm: [options: DeleteOptions]; cancel: [] }>();

const many = computed(() => props.entries.length > 1);
const first = computed(() => props.entries[0]);
const oneSeries = computed(() => !many.value && first.value?.kind === "series");

const file = ref(true);
const hosted = ref(false);
const plan = computed(() =>
  deletePlan({
    entries: props.entries,
    file: file.value,
    hosted: hosted.value,
    uploadsAllowed: uploadsAllowed.value,
    online: online.value,
    hostName: (provider) => ctx.hostName(provider),
  }),
);
const saved = computed(() => plan.value.saved);
const uploaded = computed(() => plan.value.uploaded);
const host = computed(() => plan.value.host);

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
  emit("confirm", plan.value.options);
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
          <input
            type="checkbox"
            class="g-checkbox"
            :checked="plan.options.deleteHostedCopy"
            :disabled="plan.locked !== null"
            @change="hosted = ($event.target as HTMLInputElement).checked"
          />
          <span>
            <span>{{ uploaded.length > 1 ? t("history.delete.hostChoiceMany", { n: uploaded.length, host }) : t("history.delete.hostChoice", { host }) }}</span>
            <span class="g-help" :data-tone="plan.locked ? 'warn' : undefined">{{ plan.locked ?? t("history.delete.hostHelp") }}</span>
          </span>
        </label>
      </div>
      <p class="hv-summary" role="status"><GIcon name="info" :size="16" /><span>{{ plan.summary }}</span></p>
      <div class="g-dialog-foot">
        <button type="button" class="g-button" :disabled="busy" @click="emit('cancel')">{{ t("history.delete.cancel") }}</button>
        <button ref="go" type="button" class="g-button" data-variant="destructive" :disabled="busy" @click="confirm">{{ plan.label }}</button>
      </div>
    </div>
  </div>
</template>
