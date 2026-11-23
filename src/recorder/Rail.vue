<script setup lang="ts">
// The drawing rail (`rec-rail`, design B19 Y2 `rail`): a vertical bar beside the recorded area while
// drawing mode is on, with the four tools, the three inks, the fade, undo, clear and done. The core
// places the window (8 points left of the area, else right of it, else inside it) and puts the
// glass under the rail; the window reaches right of the rail for the tips and takes the mouse only
// over the rail. Nothing here is recorded, since the core keeps the window out of the capture.
import { computed, onMounted, onUnmounted, ref, shallowRef } from "vue";

import GIcon from "../components/GIcon.vue";
import KeyCaps from "../components/KeyCaps.vue";
import {
  onRecordPresent,
  record_present,
  record_present_state,
  record_rail_fit,
  type InkColor,
  type InkTool,
  type PresentChange,
  type PresentState,
} from "../lib/api/record";
import { t } from "../lib/i18n";

/** The kit's tooltip delay. */
const TIP_DELAY = 500;

const TOOLS: { tool: InkTool; icon: string; key: string }[] = [
  { tool: "pen", icon: "pen", key: "P" },
  { tool: "highlighter", icon: "highlighter", key: "H" },
  { tool: "arrow", icon: "arrow", key: "A" },
  { tool: "rect", icon: "rect", key: "R" },
];
const INKS: { ink: InkColor; key: string }[] = [
  { ink: "red", key: "1" },
  { ink: "yellow", key: "2" },
  { ink: "blue", key: "3" },
];

const present = shallowRef<PresentState | null>(null);
const railEl = ref<HTMLElement | null>(null);
const tip = ref<{ text: string; keys: string[] | null; top: number } | null>(null);
const railWidth = ref(0);

let tipTimer = 0;
const stops: (() => void)[] = [];

const tool = computed(() => present.value?.tool ?? "pen");
const ink = computed(() => present.value?.ink ?? "red");
const fade = computed(() => present.value?.fade ?? false);

function send(change: PresentChange): void {
  hideTip();
  void record_present(change).then(
    (next) => (present.value = next),
    (error) => console.error("The drawing rail's change was refused.", error),
  );
}

// ------------------------------------------------------------------------------------ tips

function showTip(el: HTMLElement): void {
  window.clearTimeout(tipTimer);
  tipTimer = window.setTimeout(() => {
    if (!el.isConnected) return;
    const keys = el.dataset.keys ? el.dataset.keys.split(" ") : null;
    tip.value = { text: el.dataset.tip ?? "", keys, top: el.offsetTop + el.offsetHeight / 2 };
  }, TIP_DELAY);
}

function hideTip(): void {
  window.clearTimeout(tipTimer);
  tip.value = null;
}

function onPointerOver(event: PointerEvent): void {
  const el = (event.target as Element | null)?.closest<HTMLElement>("[data-tip]");
  if (el) showTip(el);
  else hideTip();
}

function onPointerOut(event: PointerEvent): void {
  const to = (event.relatedTarget as Element | null)?.closest?.("[data-tip]");
  if (!to) hideTip();
}

// --------------------------------------------------------------------------------- the size

function fit(): void {
  const el = railEl.value;
  if (!el) return;
  const width = el.offsetWidth;
  const height = el.offsetHeight;
  if (!width || !height) return;
  railWidth.value = width;
  const radius = parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0;
  void record_rail_fit({ width, height, radius }).catch((error) => console.error("The drawing rail could not fit.", error));
}

const observer = new ResizeObserver(fit);

onMounted(async () => {
  let heard = false;
  try {
    stops.push(
      await onRecordPresent((next) => {
        heard = true;
        present.value = next;
        if (!next?.drawing) hideTip();
      }),
    );
    const first = await record_present_state();
    if (!heard) present.value = first;
  } catch (error) {
    console.error("The drawing rail could not load.", error);
  }
  if (railEl.value) observer.observe(railEl.value);
  fit();
});

onUnmounted(() => {
  window.clearTimeout(tipTimer);
  observer.disconnect();
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div class="r-rail-page" :style="{ '--r-rail-w': `${railWidth}px` }">
    <div
      ref="railEl"
      class="g-bar"
      aria-orientation="vertical"
      role="toolbar"
      :aria-label="t('record.present.rail')"
      @pointerover="onPointerOver"
      @pointerout="onPointerOut"
      @pointerdown="hideTip"
    >
      <button
        v-for="item in TOOLS"
        :key="item.tool"
        type="button"
        class="g-btn"
        :aria-label="t(`tool.${item.tool}`)"
        :aria-pressed="tool === item.tool"
        :data-tip="t(`tool.${item.tool}`)"
        :data-keys="item.key"
        @click="send({ tool: item.tool })"
      >
        <GIcon :name="item.icon" />
      </button>
      <span class="g-divider" role="separator" />
      <div class="r-rail-inks" role="radiogroup" :aria-label="t('record.present.rail')">
        <button
          v-for="item in INKS"
          :key="item.ink"
          type="button"
          class="g-swatch"
          role="radio"
          :aria-checked="ink === item.ink"
          :aria-label="t(`annotate.ink.${item.ink}`)"
          :data-tip="t(`annotate.ink.${item.ink}`)"
          :data-keys="item.key"
          :style="{ '--swatch': `var(--ink-${item.ink})` }"
          @click="send({ ink: item.ink })"
        />
      </div>
      <span class="g-divider" role="separator" />
      <button
        type="button"
        class="g-btn"
        :aria-label="t('record.present.fade')"
        :aria-pressed="fade"
        :data-tip="t('record.present.fade')"
        @click="send({ fade: !fade })"
      >
        <GIcon name="timer" />
      </button>
      <button type="button" class="g-btn" :aria-label="t('tool.undo')" :data-tip="t('tool.undo')" data-keys="⌘ Z" @click="send({ undo: true })">
        <GIcon name="undo" />
      </button>
      <button
        type="button"
        class="g-btn"
        :aria-label="t('record.present.clear')"
        :data-tip="t('record.present.clear')"
        data-keys="⌫"
        @click="send({ clear: true })"
      >
        <GIcon name="close" />
      </button>
      <span class="g-divider" role="separator" />
      <button
        type="button"
        class="g-btn"
        :aria-label="t('record.present.done')"
        :data-tip="t('record.present.done')"
        data-keys="Esc"
        @click="send({ drawing: false })"
      >
        <GIcon name="check" />
      </button>
    </div>
    <div v-if="tip" class="g-tip solid r-rail-tip" role="tooltip" :style="{ top: `${tip.top}px` }">
      {{ tip.text }}<KeyCaps v-if="tip.keys" :caps="tip.keys" />
    </div>
  </div>
</template>

<style>
.r-rail-page { position: fixed; inset: 0; overflow: hidden; }
/* The core draws the shadow and the outer ring under the rail's glass, as it does for the bar
   (`record_rail_fit`), so the rail keeps only the edges inside it. */
.r-rail-page > .g-bar {
  position: absolute; left: 0; top: 0;
  box-shadow: inset 0 0 0 var(--hairline) var(--edge-inner), inset 0 1px 0 0 var(--edge-top),
    inset 0 -1px 0 0 var(--edge-bottom), inset 0 0 12px 0 var(--edge-glow);
}
.r-rail-inks { display: flex; flex-direction: column; align-items: center; }
/* The tips sit 8 points right of the rail, on solid glass: the native blur is under the rail alone. */
.r-rail-tip { position: absolute; left: calc(var(--r-rail-w) + 8px); transform: translateY(-50%); }
</style>
