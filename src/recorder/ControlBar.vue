<script setup lang="ts">
// The control bar (design `controlBar`): Discard · the elapsed time · the microphone with its
// meter · pause or resume · stop. After Discard the core pauses the take and the bar turns into
// the question: the time, "Discard this recording?", Keep recording (focused, with its ring kept
// after a pointer click until the focus leaves) and the amber Discard at the far end. It only draws
// and reports; `Bar.vue` acts.
import { nextTick, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import { t } from "../lib/i18n";

export type ControlAction = "discard" | "mute" | "pause" | "stop" | "keep" | "confirm";

const props = defineProps<{
  time: string;
  paused: boolean;
  /** The file is being completed after Stop: every control waits. */
  finishing: boolean;
  /** `recorded`: the take records a microphone; `live`: it is not muted. */
  mic: { recorded: boolean; live: boolean; level: number };
  asked: boolean;
  /** The global keys as one key cap each, empty when there is none. */
  keys: { mute: string; pause: string; stop: string };
}>();
const emit = defineEmits<{ action: [id: ControlAction] }>();

const keep = ref<HTMLButtonElement | null>(null);
const keepRing = ref(false);

watch(
  () => props.asked,
  async (asked) => {
    if (!asked) return;
    await nextTick();
    keepRing.value = true;
    keep.value?.focus({ preventScroll: true });
  },
  { immediate: true },
);

const level = (v: number) => Math.max(0, Math.min(1, v));
</script>

<template>
  <div v-if="asked" class="g-bar" role="toolbar" :aria-label="t('record.discard.question')" data-bar="control" data-confirm>
    <span class="g-timecode" role="timer" :aria-label="t('record.elapsed')">
      <span class="g-rec-dot" data-state="paused" /><span>{{ time }}</span>
    </span>
    <span class="g-divider" role="separator" />
    <span class="r-bar-text"><GIcon name="warning" :size="16" class="r-warn" />{{ t("record.discard.question") }}</span>
    <button
      ref="keep"
      type="button"
      class="g-button"
      data-id="keep"
      :data-state="keepRing ? 'focus' : undefined"
      :data-tip="t('record.discard.keep')"
      data-keys="↩"
      @blur="keepRing = false"
      @click="emit('action', 'keep')"
    >
      {{ t("record.discard.keep") }}
    </button>
    <button type="button" class="g-button r-record" data-variant="destructive" data-id="confirm" @click="emit('action', 'confirm')">
      <GIcon name="trash" :size="16" />{{ t("record.discard.confirm") }}
    </button>
  </div>
  <div v-else class="g-bar" role="toolbar" :aria-label="t('record.bar.control')" data-bar="control">
    <button
      type="button"
      class="g-btn"
      data-tone="muted"
      data-id="discard"
      :aria-label="t('record.discard')"
      :data-tip="t('record.discard')"
      :disabled="finishing"
      @click="emit('action', 'discard')"
    >
      <GIcon name="trash" />
    </button>
    <span class="g-divider" role="separator" />
    <span class="g-timecode" role="timer" :aria-label="t('record.elapsed')">
      <span class="g-rec-dot" :data-state="paused || finishing ? 'paused' : undefined" /><span>{{ time }}</span>
    </span>
    <span class="g-divider" role="separator" />
    <button
      type="button"
      class="g-toggle"
      data-id="mute"
      :aria-pressed="mic.recorded && mic.live"
      :aria-label="t('record.mic.tip')"
      :disabled="!mic.recorded || finishing"
      :data-tip="!mic.recorded ? t('record.mic.notRecorded') : mic.live ? t('record.mic.mute') : t('record.mic.unmute')"
      :data-keys="mic.recorded ? keys.mute : undefined"
      @click="emit('action', 'mute')"
    >
      <GIcon :name="mic.recorded && mic.live ? 'microphone' : 'microphone-off'" />
      <span
        class="g-meter"
        role="meter"
        :aria-label="t('record.meter.level', { name: t('record.mic.tip') })"
        aria-valuemin="0"
        aria-valuemax="1"
        :aria-valuenow="level(mic.level).toFixed(2)"
        :style="{ '--level': level(mic.level) }"
      />
    </button>
    <button
      type="button"
      class="g-btn"
      data-id="pause"
      :aria-label="paused ? t('record.resume') : t('record.pause')"
      :disabled="finishing"
      :data-tip="paused ? t('record.resume') : t('record.pause')"
      :data-keys="keys.pause"
      @click="emit('action', 'pause')"
    >
      <GIcon :name="paused ? 'play' : 'pause'" />
    </button>
    <button
      type="button"
      class="g-btn"
      data-tone="record"
      data-id="stop"
      :aria-label="t('record.stop')"
      :disabled="finishing"
      :data-tip="t('record.stop')"
      :data-keys="keys.stop"
      @click="emit('action', 'stop')"
    >
      <GIcon name="stop" />
    </button>
  </div>
</template>

<style>
/* text inside a bar, such as the discard question */
.r-bar-text { display: inline-flex; align-items: center; gap: var(--space-6); padding: 0 var(--space-12) 0 var(--space-6); font: var(--type-label); color: var(--text); white-space: nowrap; }
</style>
