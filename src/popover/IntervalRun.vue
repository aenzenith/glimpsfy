<script setup lang="ts">
// The interval series that runs, in place of the capture modes (DESIGN.md "Popover", Y1 of the
// interval board): its count, Stop (focused, so Return stops it), a bar of the frames due, a line
// that says what it follows and when the next frame comes, and a note once frames were skipped.
// The page keeps a one-second clock of its own so the "next frame" seconds move between events.
import { computed, onBeforeUnmount, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import type { IntervalState } from "../lib/api/capture";
import { t } from "../lib/i18n";

const props = defineProps<{ state: IntervalState }>();
const emit = defineEmits<{ stop: [] }>();

const stopButton = ref<HTMLButtonElement | null>(null);
const now = ref(Date.now());
let clock: ReturnType<typeof setInterval> | undefined;

const count = computed(() => (props.state.manual ? String(props.state.taken) : `${props.state.taken}/${props.state.total}`));
const progress = computed(() =>
  props.state.total > 0 ? Math.min(100, Math.round((props.state.taken / props.state.total) * 100)) : 0,
);

const what = computed(() => {
  if (props.state.target === "area") return t("popover.interval.ofArea");
  if (props.state.target === "window" && props.state.label) return t("popover.interval.ofWindow", { label: props.state.label });
  return props.state.label ?? "";
});
const sub = computed(() => {
  const parts = [what.value, t("popover.interval.everyShort", { n: Math.round(props.state.everyMs / 1000) })];
  if (props.state.nextAtMs !== null) {
    parts.push(t("popover.interval.next", { n: Math.max(0, Math.ceil((props.state.nextAtMs - now.value) / 1000)) }));
  }
  return parts.filter(Boolean).join(" · ");
});

const clockOf = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const skippedTitle = computed(() =>
  props.state.skipped === 1 ? t("popover.interval.skipped.one") : t("popover.interval.skipped.many", { n: props.state.skipped }),
);
const skippedWhy = computed(() => {
  const time = props.state.lastSkippedAtMs !== null ? clockOf(props.state.lastSkippedAtMs) : "";
  return props.state.skipped === 1 ? t("popover.interval.skippedWhy.one", { time }) : t("popover.interval.skippedWhy.many", { time });
});

onMounted(() => {
  clock = setInterval(() => (now.value = Date.now()), 1000);
  stopButton.value?.focus();
});
onBeforeUnmount(() => clearInterval(clock));

defineExpose({ focus: () => stopButton.value?.focus() });
</script>

<template>
  <div class="s-ivr" role="status">
    <div class="s-ivr-head">
      <span class="s-ivr-ico"><GIcon name="grid" :size="16" /></span>
      <span class="s-ivr-title">{{ t("popover.interval.running") }}<span>{{ count }}</span></span>
      <button ref="stopButton" type="button" class="g-button" @click="emit('stop')">
        <GIcon name="stop" :size="16" />{{ t("popover.interval.stop") }}
      </button>
    </div>
    <div
      class="g-progress"
      role="progressbar"
      :aria-label="t('popover.interval.progress')"
      aria-valuemin="0"
      :aria-valuemax="state.total"
      :aria-valuenow="state.taken"
      :style="{ '--progress': progress }"
    />
    <span class="s-ivr-sub">{{ sub }}</span>
    <p v-if="state.skipped > 0" class="s-ivr-note">
      <GIcon name="warning" :size="16" />
      <span><b>{{ skippedTitle }}</b> {{ skippedWhy }}</span>
    </p>
  </div>
</template>

<style>
/* The board's running block (design/canvas `r17-run`). */
.s-ivr { padding: 6px 4px 4px 6px; }
.s-ivr-head { display: flex; align-items: center; gap: 10px; }
.s-ivr-ico { flex: none; display: grid; place-items: center; width: 30px; height: 30px; border-radius: 9px; background: var(--win-selection-soft); color: var(--win-iris-text); }
.s-ivr-title { flex: 1; min-width: 0; font: var(--type-title); color: var(--win-text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.s-ivr-title > span { margin-left: 6px; font: var(--type-num); color: var(--win-text-2); }
.s-ivr > .g-progress { margin-top: 10px; }
.s-ivr-sub { display: block; margin-top: 6px; padding-left: 2px; font: var(--type-body); color: var(--win-text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.s-ivr-note { display: flex; gap: 8px; align-items: flex-start; margin: 10px 0 0; padding: 8px 10px; border-radius: var(--radius-field); background: var(--win-warn-soft); font: var(--type-body); color: var(--win-text); }
.s-ivr-note > g-icon { flex: none; color: var(--win-warn); }
.s-ivr-note b { font-weight: 600; }
</style>
