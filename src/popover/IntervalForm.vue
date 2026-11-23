<script setup lang="ts">
// The interval capture's form (DESIGN.md "Popover", Y1 of the interval board): it takes the place
// of the preview when "Interval capture…" is picked in the delay's menu. Target, the time between
// frames and the end, a sentence that says how long the series lasts and where it goes, then
// Cancel and Start (Return). The form starts from the plan of Settings and the core keeps the
// plan it starts with for the next time.
import { computed, nextTick, onMounted, reactive, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import KeyCaps from "../components/KeyCaps.vue";
import SegmentedControl from "../components/SegmentedControl.vue";
import SelectControl from "../components/SelectControl.vue";
import { t } from "../lib/i18n";
import { can } from "../lib/platform";
import type { IntervalEnd, IntervalPlan, IntervalTarget } from "../lib/settings";

const props = defineProps<{ plan: IntervalPlan; accelerator: string | null | undefined; busy: boolean }>();
const emit = defineEmits<{ cancel: []; start: [plan: IntervalPlan] }>();

/** The spec's intervals; anything else is a custom number of seconds. */
const EVERY = [1, 2, 5, 10, 30, 60, 300] as const;
const CUSTOM = -1;
const MAX_EVERY = 3600;
const MAX_FRAMES = 2000;
const MAX_MINUTES = 12 * 60;
/** The ends the select offers: a number of frames, a number of minutes, or none. */
const ENDS: { end: IntervalEnd; value: number }[] = [
  { end: "frames", value: 10 },
  { end: "frames", value: 30 },
  { end: "frames", value: 60 },
  { end: "frames", value: 120 },
  { end: "frames", value: 500 },
  { end: "minutes", value: 5 },
  { end: "minutes", value: 15 },
  { end: "minutes", value: 30 },
  { end: "minutes", value: 60 },
  { end: "manual", value: 0 },
];

const plan = reactive<IntervalPlan>({ ...props.plan });
const custom = ref(!EVERY.includes(plan.everySeconds as (typeof EVERY)[number]));
const root = ref<HTMLElement | null>(null);

const targets = computed(() =>
  (
    [
      { value: "area", label: t("popover.interval.area") },
      { value: "window", label: t("popover.interval.window") },
      { value: "display", label: t("popover.interval.display") },
    ] satisfies { value: IntervalTarget; label: string }[]
  ).filter((option) => option.value !== "window" || can("windowCapture")),
);

const seconds = (n: number) => (n === 1 ? t("popover.interval.seconds.one") : t("popover.interval.seconds.many", { n }));
const minutes = (n: number) => (n === 1 ? t("popover.interval.minutes.one") : t("popover.interval.minutes.many", { n }));
const everyName = (n: number) => (n >= 60 && n % 60 === 0 ? minutes(n / 60) : seconds(n));

const everyOptions = computed(() => [
  ...EVERY.map((n) => ({ value: n as number, label: everyName(n) })),
  { value: CUSTOM, label: t("popover.interval.custom") },
]);
const everyChoice = computed(() => (custom.value ? CUSTOM : plan.everySeconds));
function pickEvery(value: number): void {
  custom.value = value === CUSTOM;
  if (!custom.value) plan.everySeconds = value;
  else void nextTick(() => root.value?.querySelector<HTMLInputElement>(".s-iv-custom input")?.focus());
}
function typeEvery(event: Event): void {
  const n = Math.round(Number((event.target as HTMLInputElement).value));
  if (Number.isFinite(n)) plan.everySeconds = Math.min(MAX_EVERY, Math.max(1, n));
}

const endKey = (end: IntervalEnd, value: number) => (end === "manual" ? "manual" : `${end}:${value}`);
const endOptions = computed(() => {
  const options = ENDS.map((o) => ({ value: endKey(o.end, o.value), label: endName(o.end, o.value) }));
  // A plan from Settings that the list does not hold (a hand-edited file) keeps its own row.
  const own = endKey(plan.end, plan.endValue);
  if (!options.some((o) => o.value === own)) options.unshift({ value: own, label: endName(plan.end, plan.endValue) });
  return options;
});
function endName(end: IntervalEnd, value: number): string {
  if (end === "frames") return t("popover.interval.endFrames", { n: value });
  if (end === "minutes") {
    return value === 1 ? t("popover.interval.endMinutes.one") : t("popover.interval.endMinutes.many", { n: value });
  }
  return t("popover.interval.endManual");
}
function pickEnd(key: string): void {
  if (key === "manual") {
    plan.end = "manual";
    return;
  }
  const [end, value] = key.split(":");
  plan.end = end as IntervalEnd;
  plan.endValue = Number(value);
}

/** How long the series lasts: the frames after the first are one interval apart, and a series
 * holds at most `MAX_FRAMES` (`IntervalPlan::frame_limit` in settings.rs). */
const lastsSeconds = computed(() => {
  const every = Math.min(MAX_EVERY, Math.max(1, Math.round(plan.everySeconds)));
  const minutes = Math.min(MAX_MINUTES, Math.max(1, plan.endValue));
  const planned = plan.end === "frames" ? plan.endValue : Math.floor((minutes * 60) / every) + 1;
  const frames = Math.min(MAX_FRAMES, Math.max(1, planned));
  const span = (frames - 1) * every;
  return plan.end === "minutes" ? Math.min(minutes * 60, span) : span;
});
function lasts(total: number): string {
  if (total < 60) return t("popover.interval.duration.seconds", { n: total });
  const m = Math.round(total / 60);
  if (m < 60) return t("popover.interval.duration.minutes", { n: m });
  return t("popover.interval.duration.hours", { h: Math.floor(m / 60), m: m % 60 });
}
/** The folder's name as the core makes it (`interval.folder`), for the minute the series starts. */
const folder = computed(() => {
  const now = new Date();
  const two = (n: number) => String(n).padStart(2, "0");
  const date = `${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())}`;
  return t("interval.folder", { date, time: `${two(now.getHours())}.${two(now.getMinutes())}` });
});
/** The sentence, cut at its two values so the page can set them in bold. */
const summary = computed(() => {
  const marker = "\u0000";
  const manual = plan.end === "manual";
  const text = manual
    ? t("popover.interval.summaryManual", { folder: marker })
    : t("popover.interval.summary", { duration: `${marker}d`, folder: marker });
  const parts: { text: string; strong: boolean }[] = [];
  text.split(marker).forEach((piece, i) => {
    if (i === 0) {
      parts.push({ text: piece, strong: false });
      return;
    }
    if (piece.startsWith("d")) {
      parts.push({ text: lasts(lastsSeconds.value), strong: true }, { text: piece.slice(1), strong: false });
    } else {
      parts.push({ text: folder.value, strong: true }, { text: piece, strong: false });
    }
  });
  return parts;
});

function start(): void {
  if (props.busy) return;
  emit("start", { ...plan, everySeconds: Math.min(MAX_EVERY, Math.max(1, Math.round(plan.everySeconds))) });
}

/** Return on the target's segments starts too; a field submits the form itself, and the other
 * buttons and the selects keep their own Return. */
function onKey(event: KeyboardEvent): void {
  if (event.key === "Enter" && (event.target as Element | null)?.closest(".g-seg")) {
    event.preventDefault();
    start();
  }
}

onMounted(() => root.value?.querySelector<HTMLElement>(".g-seg [aria-checked='true']")?.focus());
</script>

<template>
  <form ref="root" class="s-iv" :aria-label="t('popover.interval.title')" @submit.prevent="start" @keydown="onKey">
    <div class="s-iv-head">
      <GIcon name="grid" :size="16" />
      <b>{{ t("popover.interval.title") }}</b>
      <KeyCaps :accelerator="accelerator" />
      <button type="button" class="g-btn" :aria-label="t('popover.interval.close')" @click="emit('cancel')">
        <GIcon name="close" :size="16" />
      </button>
    </div>
    <div class="s-iv-row">
      <span id="s-iv-target" class="s-iv-label">{{ t("popover.interval.target") }}</span>
      <SegmentedControl v-model="plan.target" :options="targets" labelledby="s-iv-target" />
    </div>
    <div class="s-iv-row">
      <label class="s-iv-label" for="s-iv-every">{{ t("popover.interval.every") }}</label>
      <SelectControl id="s-iv-every" :model-value="everyChoice" :options="everyOptions" @update:model-value="pickEvery" />
    </div>
    <div v-if="custom" class="s-iv-row s-iv-custom">
      <label class="s-iv-label" for="s-iv-seconds">{{ t("popover.interval.customField") }}</label>
      <input
        id="s-iv-seconds"
        class="g-field"
        type="number"
        min="1"
        :max="MAX_EVERY"
        step="1"
        :value="plan.everySeconds"
        @input="typeEvery"
      />
    </div>
    <div class="s-iv-row">
      <label class="s-iv-label" for="s-iv-end">{{ t("popover.interval.end") }}</label>
      <SelectControl
        id="s-iv-end"
        :model-value="endKey(plan.end, plan.endValue)"
        :options="endOptions"
        @update:model-value="pickEnd"
      />
    </div>
    <p class="s-iv-summary">
      <template v-for="(part, i) in summary" :key="i"><b v-if="part.strong">{{ part.text }}</b><template v-else>{{ part.text }}</template></template>
      <template v-if="plan.target === 'area'"> {{ t("popover.interval.areaFirst") }}</template>
    </p>
    <div class="s-iv-actions">
      <span class="g-spacer" />
      <button type="button" class="g-button" data-variant="plain" @click="emit('cancel')">{{ t("popover.interval.cancel") }}</button>
      <button type="submit" class="g-button" data-variant="primary" :disabled="busy">
        {{ t("popover.interval.start") }}<span class="g-muted" aria-hidden="true">↩</span>
      </button>
    </div>
  </form>
</template>

<style>
/* The board's form (design/canvas `r17-form`): label column, full-width controls, the sentence. */
.s-iv { padding: 4px 4px 2px; }
.s-iv-head { display: flex; align-items: center; gap: 8px; height: 28px; padding-left: 2px; color: var(--win-text); }
.s-iv-head > b { font: var(--type-title); }
.s-iv-head > .g-btn { margin-left: auto; color: var(--win-text-2); }
.s-iv-row { display: flex; align-items: center; gap: 10px; min-height: 36px; }
.s-iv-label { flex: none; width: 52px; font: var(--type-text); color: var(--win-text-2); }
.s-iv-row > .g-seg { flex: 1; display: flex; }
.s-iv-row > .g-seg > button { flex: 1; padding: 0 6px; }
.s-iv-row > .g-select { flex: 1; min-width: 0; }
.s-iv-custom > .s-iv-label { flex: 1; width: auto; padding-left: 62px; }
.s-iv-custom > .g-field { flex: none; width: 88px; }
.s-iv-summary { margin: 8px 2px 0; font: var(--type-body); color: var(--win-text-2); }
.s-iv-summary b { font-weight: 600; color: var(--win-text); }
.s-iv-actions { display: flex; align-items: center; gap: 6px; margin-top: 12px; }
</style>
