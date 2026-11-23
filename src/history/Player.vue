<script setup lang="ts">
// A recording plays inline under a 36 px glass bar: play, the position, the length and mute.
// Space plays, ← and → move 5 s and M mutes while the player has the focus.
import { computed, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import { t } from "../lib/i18n";
import { commandHeld } from "../lib/keys";
import { formatDuration } from "./model";

defineProps<{ src: string; poster: string | null }>();

const video = ref<HTMLVideoElement | null>(null);
const playing = ref(false);
const muted = ref(false);
const position = ref(0);
const length = ref(0);
const failed = ref(false);

const percent = computed(() => (length.value ? (position.value / length.value) * 100 : 0));

function toggle(): void {
  const v = video.value;
  if (!v) return;
  if (v.paused) void v.play().catch(() => (failed.value = true));
  else v.pause();
}
function seek(event: Event): void {
  const v = video.value;
  if (v) v.currentTime = Number((event.target as HTMLInputElement).value);
}
function mute(): void {
  if (video.value) video.value.muted = muted.value = !muted.value;
}
function key(event: KeyboardEvent): void {
  const v = video.value;
  if (!v || (event.target as HTMLElement).matches("input[type=range]")) return;
  if (event.key === " ") toggle();
  else if (event.key === "ArrowLeft") v.currentTime = Math.max(0, v.currentTime - 5);
  else if (event.key === "ArrowRight") v.currentTime = Math.min(length.value, v.currentTime + 5);
  else if (event.key.toLowerCase() === "m" && !commandHeld(event)) mute();
  else return;
  event.preventDefault();
  event.stopPropagation();
}
</script>

<template>
  <div class="hv-player" tabindex="0" :aria-label="t('history.player.label')" @keydown="key">
    <video
      ref="video"
      :src="src"
      :poster="poster ?? undefined"
      preload="metadata"
      playsinline
      @play="playing = true"
      @pause="playing = false"
      @ended="playing = false"
      @timeupdate="position = video?.currentTime ?? 0"
      @loadedmetadata="length = video?.duration ?? 0"
      @error="failed = true"
    />
    <div class="hv-player-bar g-glass" role="group" :aria-label="t('history.player.controls')">
      <button
        type="button"
        class="g-btn"
        data-size="small"
        :disabled="failed"
        :aria-label="playing ? t('history.player.pause') : t('history.player.play')"
        :data-tip="playing ? t('history.player.pause') : t('history.player.play')"
        data-key="Space"
        @click="toggle"
      >
        <GIcon :name="playing ? 'pause' : 'play'" :size="16" />
      </button>
      <span class="hv-time">{{ formatDuration(position * 1000) }}</span>
      <input
        type="range"
        class="g-slider"
        min="0"
        :max="length || 0"
        step="0.1"
        :value="position"
        :style="{ '--value': `${percent}%` }"
        :aria-label="t('history.player.position')"
        :aria-valuetext="t('history.player.valuetext', { at: formatDuration(position * 1000), of: formatDuration(length * 1000) })"
        :disabled="failed"
        @input="seek"
      />
      <span class="hv-time" data-muted>{{ formatDuration(length * 1000) }}</span>
      <button
        type="button"
        class="g-btn"
        data-size="small"
        :aria-label="muted ? t('history.player.unmute') : t('history.player.mute')"
        :data-tip="muted ? t('history.player.unmute') : t('history.player.mute')"
        data-key="M"
        :aria-pressed="muted"
        @click="mute"
      >
        <GIcon :name="muted ? 'speaker-off' : 'speaker'" :size="16" />
      </button>
    </div>
  </div>
</template>
