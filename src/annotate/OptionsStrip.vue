<script setup lang="ts">
// The controls of the options strip for one tool (DESIGN.md "Annotation bars"): the eight inks
// (six for the highlighter) and three widths, the text sizes and the background, or the tool's
// levels and ratios. The page supplies the `g-strip` around them and places it. A choice is
// remembered for the tool and, when an object of that tool is selected, applied to it.
import { computed } from "vue";

import GIcon from "../components/GIcon.vue";
import { t, type Key } from "../lib/i18n";
import type { AnnotationEngine } from "./engine";
import type { ToolId, ToolOptions } from "./prefs";
import { CROP_RATIOS, HIGHLIGHTER_INKS, HIGHLIGHTER_WIDTHS, INKS, STEP_SIZES, STROKE_WIDTHS, TEXT_SIZES } from "./style";

const props = defineProps<{
  engine: AnnotationEngine;
  tool: ToolId;
  /** Levels and ratios as one segmented control, as the overlay shows them; the editor shows
   * them as separate options. */
  segmented?: boolean;
}>();

const current = computed<ToolOptions>(() => (props.engine.rev.value, props.engine.current(props.tool)));

const inks = computed(() => {
  if (["blur", "pixelate", "spotlight", "crop"].includes(props.tool)) return [];
  return props.tool === "highlighter" ? HIGHLIGHTER_INKS : INKS;
});

const widths = computed<readonly number[]>(() => {
  switch (props.tool) {
    case "highlighter":
      return HIGHLIGHTER_WIDTHS;
    case "step":
      return STEP_SIZES;
    case "arrow":
    case "line":
    case "rect":
    case "ellipse":
    case "pen":
      return STROKE_WIDTHS;
    default:
      return [];
  }
});

const levels = computed<Key[]>(() => {
  switch (props.tool) {
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

const ratioLabel = (r: (typeof CROP_RATIOS)[number]) => (r === "free" ? t("overlay.free") : r === "original" ? t("annotate.ratio.original") : r);

function choose(change: Partial<ToolOptions>): void {
  props.engine.choose(props.tool, change);
}
</script>

<template>
  <!-- `mousedown.prevent` keeps the focus in a text that is being typed. -->
  <template v-if="inks.length">
    <button
      v-for="name in inks"
      :key="name"
      type="button"
      class="g-swatch"
      role="radio"
      :aria-checked="current.ink === name"
      :aria-label="t(`annotate.ink.${name}` as Key)"
      :style="{ '--swatch': `var(--ink-${name})` }"
      @mousedown.prevent
      @click="choose({ ink: name })"
    />
    <span class="g-divider" role="separator" />
  </template>
  <button
    v-for="(w, i) in widths"
    :key="`w${w}`"
    type="button"
    class="g-width"
    role="radio"
    :data-shape="tool === 'step' ? 'dot' : undefined"
    :aria-checked="current.width === i"
    :aria-label="t('annotate.width', { n: w })"
    :style="{ '--w': `${w}px` }"
    @mousedown.prevent
    @click="choose({ width: i })"
  />
  <template v-if="tool === 'text'">
    <button
      v-for="(s, i) in TEXT_SIZES"
      :key="`s${s}`"
      type="button"
      class="g-option"
      role="radio"
      :aria-checked="current.size === i"
      :aria-label="t('annotate.textSize', { n: s })"
      :style="{ fontSize: `${s}px` }"
      @mousedown.prevent
      @click="choose({ size: i })"
    >
      A
    </button>
    <span class="g-divider" role="separator" />
    <button
      type="button"
      class="g-option"
      :aria-pressed="current.background"
      :aria-label="t('overlay.textBackground')"
      @mousedown.prevent
      @click="choose({ background: !current.background })"
    >
      <GIcon name="textbg" />
    </button>
  </template>
  <div v-if="levels.length" :class="segmented ? 'g-seg' : 'a-group'" role="radiogroup" :aria-label="t(tool === 'pixelate' ? 'overlay.size' : 'overlay.strength')">
    <button
      v-for="(label, i) in levels"
      :key="label"
      type="button"
      :class="segmented ? undefined : 'g-option'"
      role="radio"
      :aria-checked="current.level === i"
      @mousedown.prevent
      @click="choose({ level: i })"
    >
      {{ t(label) }}
    </button>
  </div>
  <div v-if="tool === 'crop'" :class="segmented ? 'g-seg' : 'a-group'" role="radiogroup" :aria-label="t('overlay.ratio')">
    <button
      v-for="r in CROP_RATIOS"
      :key="r"
      type="button"
      :class="segmented ? undefined : 'g-option'"
      role="radio"
      :aria-checked="current.ratio === r"
      @mousedown.prevent
      @click="choose({ ratio: r })"
    >
      {{ ratioLabel(r) }}
    </button>
  </div>
</template>

<style scoped>
/* Separate options sit straight in the strip; the group only names them for assistive tech. */
.a-group {
  display: contents;
}
</style>
