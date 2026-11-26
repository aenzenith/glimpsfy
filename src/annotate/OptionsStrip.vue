<script setup lang="ts">
// The controls of the options strip for one tool (DESIGN.md "Annotation bars"): the eight inks
// (six for the highlighter) and three widths, the text sizes and the background, the magnifier's
// factors and, where they differ, its Sharp and Smooth pixels, or the tool's levels and ratios. The page supplies the container around them: the overlay's `g-strip`, the
// editor's options strip. A choice is
// remembered for the tool and, when an object of that tool is selected, applied to it. A tool that
// snaps to words ends with a warning when the text could not be read, which says why it draws
// freely.
import { computed } from "vue";

import GIcon from "../components/GIcon.vue";
import TipWrap from "../components/TipWrap.vue";
import { MAGNIFY_FACTORS } from "../lib/annotate";
import { language, t, type Key } from "../lib/i18n";
import type { AnnotationEngine } from "./engine";
import { beyondPixels } from "./magnify";
import type { ToolId, ToolOptions } from "./prefs";
import { snaps } from "./snap";
import { CROP_RATIOS, HIGHLIGHTER_INKS, HIGHLIGHTER_WIDTHS, INKS, STEP_SIZES, STROKE_WIDTHS, TEXT_SIZES } from "./style";

const props = defineProps<{
  engine: AnnotationEngine;
  tool: ToolId;
  /** Levels and ratios as one segmented control, as the overlay shows them; the editor shows
   * them as separate options. */
  segmented?: boolean;
  /** The text could not be read (`useSnapWords`), so the snapping tools draw freely. */
  snapProblem?: boolean;
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

// Sharp and Smooth only differ where the factor goes past the capture's own pixels, so the choice
// shows only then, with a tip that says why it is there.
const beyond = computed(() => props.tool === "magnify" && beyondPixels(current.value.factor, props.engine.scale));
const beyondTip = computed(() => {
  const scale = new Intl.NumberFormat(language.value, { maximumFractionDigits: 2 }).format(props.engine.scale);
  return t("annotate.magnify.beyond", { scale, factor: current.value.factor });
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
  <template v-if="tool === 'magnify'">
    <div class="a-group" role="radiogroup" :aria-label="t('annotate.magnify.factor')">
      <button
        v-for="f in MAGNIFY_FACTORS"
        :key="`f${f}`"
        type="button"
        class="g-option"
        role="radio"
        :aria-checked="current.factor === f"
        :aria-label="t('annotate.magnify.factorN', { n: f })"
        :aria-keyshortcuts="String(f)"
        @mousedown.prevent
        @click="choose({ factor: f })"
      >
        {{ f }}×
      </button>
    </div>
    <template v-if="beyond">
      <span class="g-divider" role="separator" />
      <div class="a-group" role="radiogroup" :aria-label="t('annotate.magnify.pixels')">
        <button type="button" class="g-option" role="radio" :aria-checked="!current.smooth" @mousedown.prevent @click="choose({ smooth: false })">
          {{ t("annotate.magnify.sharp") }}
        </button>
        <button type="button" class="g-option" role="radio" :aria-checked="current.smooth" @mousedown.prevent @click="choose({ smooth: true })">
          {{ t("annotate.magnify.smooth") }}
        </button>
      </div>
      <TipWrap :tip="beyondTip" :side="segmented ? 'below' : undefined" align="end">
        <button type="button" class="g-btn" data-size="small" :aria-label="t('annotate.magnify.why')" @mousedown.prevent>
          <GIcon name="info" :size="16" />
        </button>
      </TipWrap>
    </template>
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
  <template v-if="snapProblem && snaps(tool)">
    <span class="g-divider" role="separator" />
    <!-- The overlay's strip hangs under its tool bar, so its tip opens downwards. -->
    <TipWrap :tip="t('annotate.snapUnavailable')" :side="segmented ? 'below' : undefined" align="end">
      <button type="button" class="g-btn a-warn" data-size="small" :aria-label="t('annotate.snapUnavailable')" @mousedown.prevent>
        <GIcon name="warning" :size="16" />
      </button>
    </TipWrap>
  </template>
</template>

<style scoped>
/* Separate options sit straight in the strip; the group only names them for assistive tech. */
.a-group {
  display: contents;
}
.a-warn {
  color: var(--tone-warn);
}
</style>
