<script setup lang="ts">
// The result chip of Remove background (design/canvas row 16, Y1): a glass chip in the options
// strip's place, 12 px above the canvas's bottom edge. While Vision works it says so and offers
// Esc; with no subject or a failure it says that; once the subjects are lifted it counts them,
// shows each as a 28 px thumbnail that drops it or brings it back, and copies the result. The
// last kept subject cannot be dropped, so the image never ends up empty. A JPEG output format
// adds a line that says Save would fill the transparent areas with white.
import { computed } from "vue";

import GIcon from "../components/GIcon.vue";
import ProgressRing from "../components/ProgressRing.vue";
import TipWrap from "../components/TipWrap.vue";
import { t } from "../lib/i18n";

const props = defineProps<{
  state: "working" | "none" | "failed" | "done";
  /** How many subjects the image has; they are numbered from 1. */
  count: number;
  /** The numbers of the subjects that stay. */
  keep: readonly number[];
  /** A small image of each subject, by its number less one. */
  thumbs: readonly string[];
  /** The output format of the settings is JPEG. */
  jpeg: boolean;
}>();
const emit = defineEmits<{
  toggle: [subject: number];
  hover: [subject: number | null];
  copy: [];
  cancel: [];
  hide: [];
  savePng: [];
}>();

const numbers = computed(() => Array.from({ length: props.count }, (_, i) => i + 1));
const kept = (n: number) => props.keep.includes(n);

/** The count with its numbers in bold, as the design sets them: "**3** subjects lifted",
 * "**2 / 3** subjects kept". The numbers go into the catalog's sentence as marks, so the
 * translation decides where they stand. */
const countParts = computed(() => {
  const all = props.keep.length === props.count;
  const n = props.count;
  const text = all
    ? t(n === 1 ? "editor.subject.found.one" : "editor.subject.found.other", { n: "\u0001" })
    : t("editor.subject.kept", { kept: "\u0002", n: "\u0001" });
  const parts: { text: string; bold: boolean }[] = [];
  for (const piece of text.split(/(\u0001|\u0002)/)) {
    if (!piece) continue;
    const bold = piece === "\u0001" || piece === "\u0002";
    const shown = piece === "\u0001" ? String(n) : piece === "\u0002" ? String(props.keep.length) : piece;
    const last = parts[parts.length - 1];
    // "2 / 3" reads as one figure: the slash between two numbers is bold with them.
    if (bold && last && last.bold) last.text += shown;
    else if (!bold && last?.bold && shown.trim() === "/") last.text += shown;
    else parts.push({ text: shown, bold });
  }
  return parts;
});
const countText = computed(() => countParts.value.map((p) => p.text).join(""));
</script>

<template>
  <div
    class="g-chip e-subject"
    :role="state === 'none' || state === 'failed' ? 'alert' : 'status'"
    :data-card="(state === 'done' && jpeg) || undefined"
    :data-wrap="state === 'none' || state === 'failed' || undefined"
    @pointerdown.stop
    @mousedown.prevent
  >
    <template v-if="state === 'working'">
      <ProgressRing :progress="null" :label="t('editor.subject.working')" :size="16" />
      <span class="e-subject-text">{{ t("editor.subject.working") }}</span>
      <button type="button" class="g-button" data-variant="plain" @click="emit('cancel')">
        {{ t("editor.subject.cancel") }}<kbd class="g-kbd">Esc</kbd>
      </button>
    </template>

    <template v-else-if="state === 'none' || state === 'failed'">
      <span class="e-subject-amber"><GIcon name="warning" :size="16" /></span>
      <span class="e-subject-text">{{ t(state === "none" ? "editor.subject.none" : "editor.subject.failed") }}</span>
      <TipWrap :tip="t('editor.subject.hide')" align="end">
        <button type="button" class="g-btn" data-size="small" :aria-label="t('editor.subject.hide')" @click="emit('hide')">
          <GIcon name="close" :size="16" />
        </button>
      </TipWrap>
    </template>

    <template v-else>
      <div class="e-subject-row">
        <GIcon name="subject" :size="16" />
        <span class="e-subject-text">
          <template v-for="(part, i) in countParts" :key="i"><b v-if="part.bold">{{ part.text }}</b><template v-else>{{ part.text }}</template></template>
        </span>
        <span class="e-subject-picks" role="group" :aria-label="countText">
          <TipWrap v-for="n in numbers" :key="n" :tip="t(kept(n) ? 'editor.subject.drop' : 'editor.subject.keep')">
            <button
              type="button"
              class="e-subject-pick"
              :aria-pressed="kept(n)"
              :aria-label="t('editor.subject.item', { n })"
              :disabled="kept(n) && keep.length === 1"
              @click="emit('toggle', n)"
              @pointerenter="emit('hover', n)"
              @pointerleave="emit('hover', null)"
              @focus="emit('hover', n)"
              @blur="emit('hover', null)"
            >
              <img v-if="thumbs[n - 1]" :src="thumbs[n - 1]" alt="" draggable="false" />
            </button>
          </TipWrap>
        </span>
        <span v-if="jpeg" class="g-spacer" />
        <span v-else class="g-divider" role="separator" />
        <button type="button" class="g-button" data-variant="primary" aria-keyshortcuts="Meta+C" @click="emit('copy')">
          <GIcon name="copy" :size="16" />{{ t("action.copySubject") }}
        </button>
        <TipWrap :tip="t('editor.subject.hide')" align="end">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('editor.subject.hide')" @click="emit('hide')">
            <GIcon name="close" :size="16" />
          </button>
        </TipWrap>
      </div>
      <div v-if="jpeg" class="e-subject-note">
        <GIcon name="warning" :size="16" />
        <span>{{ t("editor.subject.jpeg") }}</span>
        <button type="button" class="g-button" @click="emit('savePng')">{{ t("editor.subject.savePng") }}</button>
      </div>
    </template>
  </div>
</template>

<style scoped>
/* Centred by its margins between two insets rather than by a transform from the middle, so a
   sentence that has to wrap gets the canvas's width and not half of it. */
.e-subject {
  position: absolute;
  left: 12px;
  right: 12px;
  bottom: 12px;
  z-index: 12;
  width: fit-content;
  margin-inline: auto;
}
.e-subject[data-wrap] {
  height: auto;
  min-height: var(--bar-h);
  white-space: normal;
}
.e-subject[data-wrap] > .e-subject-text {
  flex: 1;
  white-space: normal;
}
.e-subject :deep(.g-ring) {
  --surface-track: var(--track);
  --surface-iris-text: var(--iris-text);
}
.e-subject-text {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  font: var(--type-label);
  color: var(--text);
  white-space: nowrap;
}
.e-subject-text b {
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.e-subject-amber {
  display: inline-flex;
  color: var(--warn);
}
.e-subject > .g-divider,
.e-subject-row > .g-divider {
  height: 18px;
  margin: 0 2px;
}
.e-subject-row {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.e-subject-row > g-icon {
  flex: none;
  color: var(--text-muted);
}
.e-subject-row > .g-button {
  height: 32px;
  border-radius: 16px;
}
.e-subject-picks {
  display: inline-flex;
  flex: none;
  gap: 4px;
}
/* The thumbnails sit on the checkerboard that stands for transparency; a dropped subject is
   faint, a kept one has the iris rim. */
.e-subject-pick {
  position: relative;
  display: block;
  width: 28px;
  height: 28px;
  padding: 0;
  border: 0;
  border-radius: 8px;
  background: repeating-conic-gradient(#3a3a42 0 25%, #1c1c21 0 50%) 0 0 / 8px 8px;
  -webkit-appearance: none;
  appearance: none;
}
.e-subject-pick > img {
  display: block;
  width: 100%;
  height: 100%;
  border-radius: inherit;
  object-fit: contain;
}
.e-subject-pick::after {
  content: "";
  position: absolute;
  inset: 0;
  border-radius: inherit;
  box-shadow: inset 0 0 0 var(--hairline) rgba(255, 255, 255, 0.16);
}
.e-subject-pick[aria-pressed="true"]::after {
  box-shadow: inset 0 0 0 1.5px var(--iris-text);
}
.e-subject-pick[aria-pressed="false"] > img {
  opacity: 0.32;
}
.e-subject-pick:focus-visible {
  outline: 2px solid var(--iris);
  outline-offset: 2px;
}
.e-subject[data-card] {
  flex-direction: column;
  align-items: stretch;
  height: auto;
  width: 452px;
  max-width: 100%;
  padding: 4px;
  border-radius: 20px;
  white-space: normal;
}
.e-subject[data-card] > .e-subject-row {
  height: 32px;
  padding-left: 8px;
}
.e-subject-note {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 4px;
  padding: 8px 4px 2px 10px;
  box-shadow: inset 0 var(--hairline) 0 var(--divider);
  font: var(--type-body);
  color: var(--text-muted);
}
.e-subject-note > g-icon {
  flex: none;
  color: var(--warn);
}
.e-subject-note > span {
  flex: 1;
  min-width: 0;
}
.e-subject-note > .g-button {
  flex: none;
}
</style>
