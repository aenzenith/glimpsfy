<script setup lang="ts">
// The webcam bubble's hover controls (`rec-bubble-ctl`): a small glass toolbar the core lays over
// the bubble's lower edge while the pointer is on it, and keeps out of the file. Size switches the
// bubble between small and large, and Turn camera off ends the camera for the take. Neither has a
// key, so no key is taken from the app being recorded.
import { onMounted, onUnmounted, shallowRef } from "vue";

import GIcon from "../components/GIcon.vue";
import { onRecordBubble, record_bubble_set, record_bubble_state, type BubbleState } from "../lib/api/record";
import { t } from "../lib/i18n";

const bubble = shallowRef<BubbleState | null>(null);
const stops: (() => void)[] = [];

function report(error: unknown): void {
  // The panel has no room for a sentence; the core's log keeps the failure.
  console.error("The webcam bubble could not change.", error);
}

function toggleSize(): void {
  const size = bubble.value?.size === "large" ? "small" : "large";
  record_bubble_set({ size }).catch(report);
}

function turnOff(): void {
  record_bubble_set({ off: true }).catch(report);
}

onMounted(async () => {
  let heard = false;
  try {
    stops.push(
      await onRecordBubble((next) => {
        heard = true;
        bubble.value = next;
      }),
    );
    const first = await record_bubble_state();
    if (!heard) bubble.value = first;
  } catch (error) {
    report(error);
  }
});

onUnmounted(() => stops.forEach((stop) => stop()));
</script>

<template>
  <div class="r-bubble-page">
    <div class="g-glass r-cam-controls" role="toolbar" :aria-label="t('record.bubble.label')">
      <button
        type="button"
        class="g-btn"
        data-size="small"
        :aria-label="bubble?.size === 'large' ? t('record.bubble.smaller') : t('record.bubble.larger')"
        @click="toggleSize"
      >
        <GIcon :name="bubble?.size === 'large' ? 'zoom-out' : 'zoom-in'" :size="16" />
      </button>
      <button type="button" class="g-btn" data-size="small" :aria-label="t('record.bubble.off')" @click="turnOff">
        <GIcon name="webcam-off" :size="16" />
      </button>
    </div>
  </div>
</template>

<style>
.r-bubble-page { position: fixed; inset: 0; display: grid; place-items: center; overflow: hidden; }
.r-cam-controls { display: flex; width: max-content; padding: 2px; border-radius: 16px; }
</style>
