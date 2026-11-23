<script setup lang="ts">
// The approval card (D34, Y1): the request an agent waits on, as one 340 px toast at the bottom of
// the stack. A small picture of what would go sits on the left, and the agent, its countdown,
// what it asks for and the target sit on the right; the answers are labelled buttons with their
// keys, and ‹ › walk the waiting requests. The core counts the answers only once the card has
// shown the request whole for 0.6 s (the page reports that in `toast_fit`), so the buttons look
// the same from the start and a refused click is only logged.
import { computed, onUnmounted, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import ProgressRing from "../components/ProgressRing.vue";
import TipWrap from "../components/TipWrap.vue";
import { agentName, kindIcon, monogram } from "../lib/agents";
import { agents_answer, agents_show, type AgentAnswer, type AgentQueue, type AgentRequest } from "../lib/api/agents";
import { t } from "../lib/i18n";
import { capsOf, capsText } from "../lib/keys";
import { ANSWER_MS, askText, fitIn } from "./agent";

const props = defineProps<{
  queue: AgentQueue;
  /** When `queue` arrived (`performance.now()`), so the card counts down between the core's reports. */
  at: number;
}>();

const request = computed<AgentRequest | null>(() => props.queue.requests.find((r) => r.id === props.queue.shown) ?? null);
const index = computed(() => props.queue.requests.findIndex((r) => r.id === props.queue.shown));
const name = computed(() => agentName(request.value?.agent));
const picture = computed(() => fitIn(request.value?.width, request.value?.height, 76, 52));

// ------------------------------------------------------------------------------- countdown

const now = ref(performance.now());
let tick: ReturnType<typeof setTimeout> | null = null;

const leftMs = computed(() => Math.max(0, (request.value?.leftMs ?? 0) - (now.value - props.at)));
const seconds = computed(() => Math.ceil(leftMs.value / 1000));
const share = computed(() => Math.min(100, (leftMs.value / ANSWER_MS) * 100));

/** Wakes when the shown second changes, not more often: the page already polls the pointer. */
function schedule(): void {
  if (tick) clearTimeout(tick);
  tick = null;
  now.value = performance.now();
  if (leftMs.value <= 0) return;
  tick = setTimeout(schedule, (leftMs.value % 1000 || 1000) + 15);
}

watch(() => [props.queue, props.at], schedule, { immediate: true });
onUnmounted(() => {
  if (tick) clearTimeout(tick);
});

// --------------------------------------------------------------------------------- answers

function answer(kind: AgentAnswer): void {
  const shown = request.value;
  if (!shown) return;
  // The core refuses an answer that came too soon or that another process posted; the card stays
  // as it is, so the user can click again.
  agents_answer(shown.id, kind).catch((error) => console.warn("agents_answer", error));
}

function step(by: number): void {
  const next = props.queue.requests[index.value + by];
  if (next) agents_show(next.id).catch((error) => console.warn("agents_show", error));
}

const caps = (accelerator: string | null) => (accelerator ? capsText(accelerator) : "");
</script>

<template>
  <div v-if="request" class="g-toast ag-card" role="alertdialog" :aria-label="`${name} ${askText(request)}`">
    <div class="ag-top">
      <span class="ag-thumb">
        <span v-if="request.pick" class="ag-pic" data-ph data-pick><GIcon name="area" :size="20" /></span>
        <span v-else-if="request.thumbnail" class="ag-pic" :data-whole="picture ? undefined : ''" :style="picture ?? undefined">
          <img :src="request.thumbnail" alt="" draggable="false" />
          <span v-if="request.kind === 'text' && request.boxes.length" class="ag-ocr">
            <i
              v-for="(box, i) in request.boxes"
              :key="i"
              :style="{ left: `${box[0] * 100}%`, top: `${box[1] * 100}%`, width: `${box[2] * 100}%`, height: `${box[3] * 100}%` }"
            ></i>
          </span>
        </span>
        <span v-else class="ag-pic" data-ph><GIcon :name="kindIcon(request.kind)" :size="20" /></span>
      </span>
      <div class="ag-text">
        <div class="ag-who">
          <span class="ag-mono" data-size="small" aria-hidden="true">{{ monogram(name) }}</span>
          <TipWrap v-if="request.process" class="ag-name" :tip="request.process" side="below" align="end"><b>{{ name }}</b></TipWrap>
          <span v-else class="ag-name"><b>{{ name }}</b></span>
          <span class="ag-left">
            <ProgressRing :progress="share" :label="t('agent.leftLabel')" :size="16" />{{ t("agent.left", { n: seconds }) }}
          </span>
        </div>
        <div class="ag-body">{{ askText(request) }}</div>
        <div v-if="request.pick || request.target" class="ag-target">
          <GIcon :name="kindIcon(request.kind)" :size="14" />
          <span>{{ request.pick ? t("agent.pickTarget") : request.target }}</span>
        </div>
      </div>
    </div>

    <div class="ag-acts">
      <button type="button" class="g-button" @click="answer('deny')">
        <span class="ag-label">{{ t("agent.deny") }}</span><kbd v-if="queue.keys.deny" class="g-kbd">{{ caps(queue.keys.deny) }}</kbd>
      </button>
      <button type="button" class="g-button" data-variant="primary" @click="answer('allow')">
        <span class="ag-label">{{ request.pick ? t("agent.selectArea") : t("agent.allow") }}</span
        ><kbd v-if="queue.keys.allow" class="g-kbd">{{ caps(queue.keys.allow) }}</kbd>
      </button>
    </div>
    <div class="ag-acts2">
      <button type="button" class="g-button" data-variant="plain" @click="answer('allowTimed')">
        <span class="ag-label">{{ t("agent.allowTimed") }}</span><kbd v-if="queue.keys.timed" class="g-kbd">{{ caps(queue.keys.timed) }}</kbd>
      </button>
      <span class="g-spacer"></span>
      <span v-if="queue.requests.length > 1" class="ag-nav">
        <TipWrap :tip="t('agent.previous')" :caps="capsOf(queue.keys.previous)" align="end">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('agent.previous')" :disabled="index <= 0" @click="step(-1)">
            <GIcon name="chevron-left" :size="16" />
          </button>
        </TipWrap>
        <span class="ag-navn">{{ index + 1 }} / {{ queue.requests.length }}</span>
        <TipWrap :tip="t('agent.next')" :caps="capsOf(queue.keys.next)" align="end">
          <button
            type="button"
            class="g-btn"
            data-size="small"
            :aria-label="t('agent.next')"
            :disabled="index >= queue.requests.length - 1"
            @click="step(1)"
          >
            <GIcon name="chevron-right" :size="16" />
          </button>
        </TipWrap>
      </span>
    </div>
  </div>
</template>

<style scoped>
/* One toast of the stack, wider and taller than a row: the parent's motion, native glass and hit
   testing take it like any other (`.g-toast`). */
.g-toast.ag-card {
  display: block;
  width: 340px;
  max-width: none;
  padding: 10px 10px 8px;
}
.ag-top {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  column-gap: 12px;
  align-items: start;
}
.ag-text {
  min-width: 0;
}
.ag-who {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.ag-name {
  flex: 1;
  min-width: 0;
}
.ag-name b {
  display: block;
  min-width: 0;
  font: var(--type-title);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
/* The process can be a long path; its tip wraps inside the card instead of running out of the
   window, which ends a shadow's width past the card. */
.ag-name :deep(.g-tip) {
  width: max-content;
  max-width: 260px;
  height: auto;
  min-height: 26px;
  padding: 5px 10px;
  white-space: normal;
  overflow-wrap: anywhere;
}
.ag-left {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  flex: none;
  font: var(--type-num-small);
  font-variant-numeric: tabular-nums;
  color: var(--surface-text-muted);
}
.ag-body {
  margin-top: 2px;
  font: var(--type-body);
  color: var(--text-muted);
}
.ag-target {
  display: flex;
  align-items: flex-start;
  gap: 5px;
  min-width: 0;
  margin-top: 4px;
  font: var(--type-body);
  color: var(--text);
}
.ag-target > g-icon {
  margin-top: 1px;
  color: var(--text-muted);
}
/* A long window title takes two lines and no more. */
.ag-target > span {
  min-width: 0;
  overflow: hidden;
  display: -webkit-box;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 2;
  overflow-wrap: anywhere;
}
.ag-acts {
  display: flex;
  gap: 6px;
  margin-top: 10px;
}
.ag-acts > .g-button {
  flex: 1;
  min-width: 0;
}
.ag-acts2 {
  display: flex;
  align-items: center;
  gap: 2px;
  margin: 4px -2px 0 -4px;
}
.ag-acts2 > .g-button {
  min-width: 0;
  flex: 0 1 auto;
  color: var(--text-muted);
}
.ag-label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}
.ag-nav {
  display: inline-flex;
  align-items: center;
  flex: none;
}
.ag-nav .g-btn {
  color: var(--text-muted);
}
.ag-navn {
  min-width: 30px;
  text-align: center;
  font: var(--type-num-small);
  font-variant-numeric: tabular-nums;
  color: var(--text-muted);
}
/* WebKit's own hover in the toast panel is the spot of the last click (App.vue, "the pointer"); a
   plain button draws nothing for it, as the page's rules do for the others. */
.ag-acts2 > .g-button[data-variant="plain"]:hover:not([data-state="hover"], :active) {
  background: none;
}
</style>

<style>
/* The picture and the monogram are shared with the agents' quiet cards (AgentNote.vue). */
.ag-mono {
  display: inline-grid;
  place-items: center;
  flex: none;
  width: 20px;
  height: 20px;
  border-radius: 6px;
  background: var(--tone-iris-soft);
  color: var(--tone-iris);
  font: 600 9.5px/1 var(--font);
  letter-spacing: 0.02em;
}
.ag-thumb {
  position: relative;
  display: block;
  width: 76px;
  height: 52px;
  margin-top: 1px;
  border-radius: var(--radius-thumb);
  background: var(--glass-solid);
}
.ag-thumb::after {
  content: "";
  position: absolute;
  inset: 0;
  border-radius: inherit;
  box-shadow: inset 0 0 0 var(--hairline) var(--edge-inner);
  pointer-events: none;
}
.ag-thumb > .g-status {
  position: absolute;
  right: -5px;
  bottom: -5px;
  z-index: 1;
  box-shadow: 0 0 0 2px rgba(19, 17, 32, 0.92);
}
/* The picture is drawn whole in its frame, at the size the script fits it to. */
.ag-pic {
  position: absolute;
  inset: 0;
  margin: auto;
  overflow: hidden;
  border-radius: 3px;
}
.ag-pic > img {
  display: block;
  width: 100%;
  height: 100%;
}
.ag-pic[data-whole] > img {
  object-fit: contain;
}
.ag-pic[data-ph] {
  display: grid;
  place-items: center;
  margin: 0;
  border-radius: inherit;
  color: var(--surface-text-muted);
}
.ag-pic[data-pick] {
  inset: 6px;
  border: 1px dashed var(--surface-text-muted);
  border-radius: 4px;
}
.ag-ocr {
  position: absolute;
  inset: 0;
  background: var(--ocr-veil);
}
.ag-ocr > i {
  position: absolute;
  display: block;
  box-sizing: border-box;
  border: 1px solid var(--ocr-line);
  border-radius: 2px;
}
</style>
