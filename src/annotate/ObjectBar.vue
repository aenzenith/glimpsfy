<script setup lang="ts">
// The small glass bar of a selected object (DESIGN.md "Editor window", ab8): its colour (⌥C
// changes it), widths or sizes or level, Bring forward ⌘], Send backward ⌘[ and Delete ⌫. The
// page places it 8 px above the object.
import { computed } from "vue";

import GIcon from "../components/GIcon.vue";
import TipWrap from "../components/TipWrap.vue";
import { t, type Key } from "../lib/i18n";
import { TOOL_OF, type AnnotationEngine } from "./engine";
import type { Color } from "../lib/annotate";
import type { ToolOptions } from "./prefs";
import { HIGHLIGHTER_WIDTHS, STEP_SIZES, STROKE_WIDTHS, TEXT_SIZES } from "./style";

const props = defineProps<{ engine: AnnotationEngine }>();

const object = computed(() => (props.engine.rev.value, props.engine.selected()));
const tool = computed(() => (object.value ? TOOL_OF[object.value.kind] : null));
const current = computed<ToolOptions | null>(() => (props.engine.rev.value, tool.value ? props.engine.current(tool.value) : null));

/** A colour written as hex by another tool shows as itself until an ink replaces it. */
const hex = computed<string | null>(() => {
  const o = object.value;
  if (!o) return null;
  let color: Color | null = null;
  if ("stroke" in o) color = o.stroke.color;
  else if (o.kind === "text") color = o.background ?? o.color;
  else if ("color" in o) color = o.color;
  return color?.kind === "hex" ? color.value : null;
});
const swatchName = computed(() => hex.value?.toUpperCase() ?? t(`annotate.ink.${current.value?.ink ?? "red"}` as Key));

const hasInk = computed(() => tool.value !== null && !["blur", "pixelate", "spotlight", "crop"].includes(tool.value));
const widths = computed<readonly number[]>(() => {
  switch (tool.value) {
    case "highlighter":
      return HIGHLIGHTER_WIDTHS;
    case "step":
      return STEP_SIZES;
    case "text":
    case "blur":
    case "pixelate":
    case "spotlight":
    case "crop":
    case null:
      return [];
    default:
      return STROKE_WIDTHS;
  }
});
const levels = computed<Key[]>(() => {
  switch (tool.value) {
    case "blur":
      return ["overlay.soft", "overlay.medium", "overlay.strong"];
    case "pixelate":
      return ["overlay.fine", "overlay.medium", "overlay.coarse"];
    case "spotlight":
      return ["overlay.light", "overlay.medium", "overlay.dark"];
    default:
      return [];
  }
});

function choose(change: Partial<ToolOptions>): void {
  if (tool.value) props.engine.choose(tool.value, change);
}

function arrange(by: 1 | -1): void {
  if (object.value) props.engine.shift(object.value.id, by);
}
</script>

<template>
  <div v-if="object && current" class="g-bar a-objectbar" role="toolbar" :aria-label="t(`tool.${tool}` as Key)" @mousedown.stop.prevent>
    <template v-if="hasInk">
      <TipWrap :tip="t('annotate.colour')" :caps="['⌥', 'C']">
        <button
          type="button"
          class="g-swatch"
          role="radio"
          aria-checked="true"
          :aria-label="t('annotate.colourIs', { ink: swatchName })"
          :style="{ '--swatch': hex ?? `var(--ink-${current.ink})` }"
          @click="engine.cycleInk()"
        />
      </TipWrap>
      <span class="g-divider" role="separator" />
    </template>
    <template v-if="widths.length">
      <button
        v-for="(w, i) in widths"
        :key="w"
        type="button"
        class="g-width"
        role="radio"
        :data-shape="tool === 'step' ? 'dot' : undefined"
        :aria-checked="current.width === i"
        :aria-label="t('annotate.width', { n: w })"
        :style="{ '--w': `${w}px` }"
        @click="choose({ width: i })"
      />
      <span class="g-divider" role="separator" />
    </template>
    <template v-if="tool === 'text'">
      <button
        v-for="(s, i) in TEXT_SIZES"
        :key="s"
        type="button"
        class="g-option"
        role="radio"
        :aria-checked="current.size === i"
        :aria-label="t('annotate.textSize', { n: s })"
        :style="{ fontSize: `${s}px` }"
        @click="choose({ size: i })"
      >
        A
      </button>
      <button type="button" class="g-option" :aria-pressed="current.background" :aria-label="t('overlay.textBackground')" @click="choose({ background: !current.background })">
        <GIcon name="textbg" />
      </button>
      <span class="g-divider" role="separator" />
    </template>
    <template v-if="levels.length">
      <button v-for="(label, i) in levels" :key="label" type="button" class="g-option" role="radio" :aria-checked="current.level === i" @click="choose({ level: i })">
        {{ t(label) }}
      </button>
      <span class="g-divider" role="separator" />
    </template>
    <TipWrap :tip="t('annotate.bringForward')" :caps="['⌘', ']']">
      <button type="button" class="g-btn" :aria-label="t('annotate.bringForward')" @click="arrange(1)"><GIcon name="bring-forward" /></button>
    </TipWrap>
    <TipWrap :tip="t('annotate.sendBackward')" :caps="['⌘', '[']">
      <button type="button" class="g-btn" :aria-label="t('annotate.sendBackward')" @click="arrange(-1)"><GIcon name="send-backward" /></button>
    </TipWrap>
    <span class="g-divider" role="separator" />
    <TipWrap :tip="t('annotate.delete')" :caps="['⌫']">
      <button type="button" class="g-btn" :aria-label="t('annotate.delete')" @click="engine.deleteSelected()"><GIcon name="trash" /></button>
    </TipWrap>
  </div>
</template>

<style scoped>
.a-objectbar {
  position: absolute;
  z-index: 9;
}
</style>
