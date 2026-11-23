<script setup lang="ts">
// The beautify panel (B11, DESIGN.md "Annotation bars" and "Editor"): the preset, the switch for
// this capture or image, a preview of the output, its size, and the padding, corner, shadow,
// window frame and background. The overlay shows it on glass beside the action bar, the editor in
// its window's material over the canvas without a preview, since the canvas shows the result.
// The panel holds no state of its own beyond the open menu and the name being typed: every change
// goes to the page as a new style, and the page decides what it belongs to.
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";

import GIcon from "./GIcon.vue";
import { MAX_CORNER, MAX_PADDING, SHADOWS, WINDOW_FRAMES, writeBeautify, type Background, type BeautifyStyle, type Shadow, type WindowFrame } from "../lib/annotate";
import type { ColorSpace } from "../lib/api/capture";
import { t, type Key } from "../lib/i18n";
import type { ImageFormat } from "../lib/settings";
import {
  SWATCHES,
  backgroundCss,
  clamped,
  drawBeautified,
  fileName,
  hexOf,
  jpegLoses,
  layoutOf,
  outputSize,
  presetName,
  sameBackground,
  suggestedName,
  type Preset,
} from "../annotate/beautify";

/** What the preview draws the style around: `from` of `image`, at `scale` pixels per point. */
export type PreviewSource = { image: CanvasImageSource; from: { x: number; y: number; width: number; height: number }; scale: number; imageRadius?: number };

const props = defineProps<{
  variant: "glass" | "window";
  /** Whether the style applies to this capture or image. */
  on: boolean;
  /** The style the panel shows: the one that applies, or the one the switch turns on. (`style`
   * would be the element's own attribute.) */
  current: BeautifyStyle;
  presets: Preset[];
  /** The preset Settings › Beautify applies by itself, or `null` while that is off. */
  autoId: string | null;
  window: boolean;
  format: ImageFormat;
  /** The user chose PNG for this capture's Save (`output_save`'s format). */
  pngOnce: boolean;
  /** The image's size in points. */
  points: { width: number; height: number };
  /** The Match colour, in the image's own values. */
  match: [number, number, number] | null;
  /** The background image cannot be read, so its fallback gradient shows. */
  missing: boolean;
  backgroundImage: CanvasImageSource | null;
  space: ColorSpace;
  /** `null` leaves the preview out (the editor). */
  preview: PreviewSource | null;
  scope: "capture" | "image";
}>();

const emit = defineEmits<{
  toggle: [on: boolean];
  /** `step`: the change starts a new undo step; a slider's later values in one drag join it. */
  change: [style: BeautifyStyle, step: boolean];
  pick: [id: string];
  png: [];
  chooseImage: [];
  update: [];
  create: [name: string];
}>();

const menuOpen = ref(false);
const naming = ref(false);
const name = ref("");
const nameField = ref<HTMLInputElement | null>(null);
const canvas = ref<HTMLCanvasElement | null>(null);

const s = computed(() => clamped(props.current));
const preset = computed(() => props.presets.find((p) => p.id === props.current.preset) ?? null);
const presetLabel = computed(() => (preset.value ? presetName(preset.value) : t("beautify.title")));
const same = (a: BeautifyStyle, b: BeautifyStyle) => JSON.stringify(writeBeautify({ ...a, preset: null })) === JSON.stringify(writeBeautify({ ...b, preset: null }));
/** The style differs from its preset, so Update preset has something to write. */
const custom = computed(() => !preset.value || !same(preset.value.style, props.current));
const matchCss = computed(() => (props.match ? hexOf(props.match) : null));

const size = computed(() =>
  props.on ? outputSize(props.points.width, props.points.height, props.current, props.window) : { width: Math.round(props.points.width), height: Math.round(props.points.height) },
);
const formatName = computed(() => (props.pngOnce ? "PNG" : { png: "PNG", jpeg: "JPEG", webp: "WebP" }[props.format]));
const describe = computed(() => {
  if (!props.on) return t("beautify.meta.off");
  if (s.value.background.kind === "match" && matchCss.value) return `${t("beautify.bg.match")} ${matchCss.value}`;
  if (custom.value) return t(props.scope === "capture" ? "beautify.meta.custom" : "beautify.meta.customImage");
  if (props.autoId && props.autoId === props.current.preset) return t("beautify.meta.auto");
  return presetLabel.value;
});
const jpegWarning = computed(() => props.on && props.format === "jpeg" && !props.pngOnce && jpegLoses(props.current));
const image = computed(() => (s.value.background.kind === "image" ? s.value.background : null));

const SHADOW_KEYS: Record<Shadow, Key> = {
  none: "beautify.shadow.none",
  soft: "beautify.shadow.soft",
  medium: "beautify.shadow.medium",
  deep: "beautify.shadow.deep",
};
const FRAME_KEYS: Record<WindowFrame, Key> = { none: "beautify.frame.none", light: "beautify.frame.light", dark: "beautify.frame.dark" };

// ------------------------------------------------------------------------------ changing

let fresh = true;
function slide(key: "padding" | "corner", e: Event): void {
  const value = Number((e.target as HTMLInputElement).value);
  emit("change", { ...props.current, [key]: value }, fresh);
  fresh = false;
}
const settle = () => (fresh = true);
/** A slider's arrows, Home, End and Page keys move it, not the selection under the panel. */
function ownKey(e: KeyboardEvent): void {
  if (/^(Arrow|Page)|^(Home|End)$/.test(e.key)) e.stopPropagation();
}

function set<K extends keyof BeautifyStyle>(key: K, value: BeautifyStyle[K]): void {
  if (props.current[key] === value) return;
  emit("change", { ...props.current, [key]: value }, true);
}

function setBackground(background: Background): void {
  if (sameBackground(props.current.background, background)) return;
  emit("change", { ...props.current, background }, true);
}

function pickPreset(id: string): void {
  menuOpen.value = false;
  emit("pick", id);
}

async function startNaming(): Promise<void> {
  name.value = suggestedName(props.presets);
  naming.value = true;
  await nextTick();
  nameField.value?.focus();
  nameField.value?.select();
}

function saveName(): void {
  const text = name.value.trim();
  if (!text) return;
  naming.value = false;
  emit("create", text);
}

function onNameKey(e: KeyboardEvent): void {
  // The page's own keys (Esc closes the capture, letters choose tools) stay out of the field.
  e.stopPropagation();
  if (e.key === "Enter") {
    e.preventDefault();
    saveName();
  } else if (e.key === "Escape") {
    e.preventDefault();
    naming.value = false;
  }
}

/** Esc closes the preset menu or the name field first; `true` when it did. */
function escape(): boolean {
  if (menuOpen.value) {
    menuOpen.value = false;
    return true;
  }
  if (naming.value) {
    naming.value = false;
    return true;
  }
  return false;
}
defineExpose({ escape });

// ------------------------------------------------------------------------------ the preview

const PREVIEW_W = 300;
const PREVIEW_H = 136;
let frame = 0;

function draw(): void {
  frame = 0;
  const el = canvas.value;
  const src = props.preview;
  if (!el || !src) return;
  const style: BeautifyStyle = props.on
    ? props.current
    : { preset: null, padding: 0, corner: 0, shadow: "none", background: { kind: "transparent" }, frame: "none" };
  const b = { image: src.image, from: src.from, scale: src.scale, imageRadius: src.imageRadius, style, window: props.window, space: props.space, match: props.match, backgroundImage: props.backgroundImage, jpeg: props.format === "jpeg" && !props.pngOnce };
  const L = layoutOf(b);
  const k = Math.min(PREVIEW_W / L.width, PREVIEW_H / L.height);
  const dpr = window.devicePixelRatio || 1;
  const cssW = Math.max(1, Math.round(L.width * k));
  const cssH = Math.max(1, Math.round(L.height * k));
  el.style.width = `${cssW}px`;
  el.style.height = `${cssH}px`;
  el.width = Math.round(cssW * dpr);
  el.height = Math.round(cssH * dpr);
  const ctx = el.getContext("2d", { colorSpace: props.space });
  if (!ctx) return;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, el.width, el.height);
  ctx.imageSmoothingQuality = "high";
  ctx.setTransform((el.width / L.width), 0, 0, el.height / L.height, 0, 0);
  drawBeautified(ctx, b);
}

watch(
  () => [props.preview, props.current, props.on, props.backgroundImage, props.match, props.format, props.pngOnce, props.window, canvas.value],
  () => {
    if (!frame) frame = requestAnimationFrame(draw);
  },
  { immediate: true, flush: "post" },
);
onBeforeUnmount(() => cancelAnimationFrame(frame));
</script>

<template>
  <div
    :class="variant === 'glass' ? 'g-pop bp-pan' : 'bp-wpan'"
    role="dialog"
    :aria-label="t('beautify.title')"
    @mousedown.stop
    @pointerdown.stop
    @dblclick.stop
  >
    <div class="bp-top">
      <span class="g-tipwrap bp-prewrap" data-align="start">
        <button
          type="button"
          class="bp-pre"
          :aria-label="t('beautify.presetButton', { name: presetLabel })"
          :aria-expanded="menuOpen"
          @click="menuOpen = !menuOpen"
        >
          <span class="bp-dot" :style="{ background: backgroundCss(current.background, matchCss) }" />
          <span class="bp-nm">{{ presetLabel }}</span>
          <GIcon name="chevron-up-down" :size="12" />
        </button>
        <span class="g-tip" role="tooltip">{{ presetLabel }}</span>
      </span>
      <label class="bp-onl">
        {{ t(scope === "capture" ? "beautify.onCapture" : "beautify.onImage") }}
        <input
          type="checkbox"
          role="switch"
          class="g-switch"
          :checked="on"
          :aria-label="t(scope === 'capture' ? 'beautify.switchCapture' : 'beautify.switchImage')"
          @change="emit('toggle', ($event.target as HTMLInputElement).checked)"
        />
      </label>
      <div v-if="menuOpen" class="g-pop bp-menu" role="listbox" :aria-label="t('beautify.presets')">
        <button
          v-for="p in presets"
          :key="p.id"
          type="button"
          class="g-pop-item"
          role="option"
          :aria-selected="p.id === current.preset"
          :aria-checked="p.id === current.preset"
          @click="pickPreset(p.id)"
        >
          <GIcon name="check" :size="16" class="g-pop-check" />
          <span class="bp-dot" :style="{ background: backgroundCss(p.style.background, matchCss) }" />
          <span class="g-pop-name">{{ presetName(p) }}</span>
        </button>
      </div>
    </div>

    <div v-if="preview" class="bp-prev"><canvas ref="canvas" class="bp-out" aria-hidden="true" /></div>
    <div class="bp-meta">
      <span>{{ size.width }} × {{ size.height }} pt</span>
      <span class="bp-desc">{{ formatName }} · {{ describe }}</span>
    </div>

    <div v-if="jpegWarning" class="bp-help">
      <GIcon name="warning" :size="16" />
      <div>
        {{ t("beautify.jpeg") }}<br />
        <button type="button" class="g-button" @click="emit('png')">{{ t("beautify.pngOnce") }}</button>
      </div>
    </div>
    <div v-if="on && image && missing" class="bp-help">
      <GIcon name="warning" :size="16" />
      <div>
        {{ t("beautify.missing", { name: fileName(image.path) }) }}<br />
        <button type="button" class="g-button" @click="emit('chooseImage')">{{ t("beautify.chooseImage") }}</button>
      </div>
    </div>

    <div class="bp-row">
      <span class="bp-lab">{{ t("beautify.padding") }}</span>
      <input
        type="range"
        class="g-slider"
        min="0"
        :max="MAX_PADDING"
        step="1"
        :value="s.padding"
        :style="{ '--value': `${(s.padding / MAX_PADDING) * 100}%` }"
        :aria-label="t('beautify.padding')"
        @pointerdown="settle"
        @keydown="ownKey"
        @input="slide('padding', $event)"
        @change="settle"
      />
      <span class="bp-val">{{ Math.round(s.padding) }}</span>
    </div>
    <div class="bp-row" :aria-disabled="window || undefined">
      <span class="bp-lab">{{ t("beautify.corner") }}</span>
      <input
        type="range"
        class="g-slider"
        min="0"
        :max="MAX_CORNER"
        step="1"
        :value="window ? 0 : s.corner"
        :disabled="window"
        :style="{ '--value': `${window ? 0 : (s.corner / MAX_CORNER) * 100}%` }"
        :aria-label="t('beautify.corner')"
        @pointerdown="settle"
        @keydown="ownKey"
        @input="slide('corner', $event)"
        @change="settle"
      />
      <span class="bp-val">{{ window ? "—" : Math.round(s.corner) }}</span>
    </div>
    <div class="bp-row" data-seg :aria-disabled="window || undefined">
      <span class="bp-lab">{{ t("beautify.shadow") }}</span>
      <div class="g-seg" role="radiogroup" :aria-label="t('beautify.shadow')">
        <button
          v-for="v in SHADOWS"
          :key="v"
          type="button"
          role="radio"
          :aria-checked="!window && s.shadow === v"
          :disabled="window"
          @click="set('shadow', v)"
        >
          {{ t(SHADOW_KEYS[v]) }}
        </button>
      </div>
    </div>
    <div class="bp-row" data-seg :aria-disabled="window || undefined">
      <span class="bp-lab">{{ t("beautify.frame") }}</span>
      <div class="g-seg" role="radiogroup" :aria-label="t('beautify.frame')">
        <button
          v-for="v in WINDOW_FRAMES"
          :key="v"
          type="button"
          role="radio"
          :aria-checked="window ? v === 'none' : s.frame === v"
          :disabled="window"
          @click="set('frame', v)"
        >
          {{ t(FRAME_KEYS[v]) }}
        </button>
      </div>
    </div>
    <p v-if="window" class="bp-note">{{ t("beautify.windowNote") }}</p>

    <div class="bp-lab2">{{ t("beautify.background") }}</div>
    <div class="bp-sws" role="radiogroup" :aria-label="t('beautify.background')">
      <template v-for="sw in SWATCHES" :key="sw.id">
        <button
          v-if="sw.id === 'match'"
          type="button"
          class="g-option"
          role="radio"
          :aria-checked="current.background.kind === 'match'"
          :aria-label="t('beautify.bg.matchHelp')"
          @click="setBackground(sw.background)"
        >
          <span class="bp-in"><GIcon name="eyedropper" :size="16" />{{ t("beautify.bg.match") }}</span>
        </button>
        <span v-else class="bp-markwrap">
          <button
            type="button"
            class="g-swatch"
            role="radio"
            :aria-checked="sameBackground(current.background, sw.background)"
            :aria-label="t(sw.label)"
            :style="{ '--swatch': backgroundCss(sw.background) }"
            @click="setBackground(sw.background)"
          />
          <span v-if="sw.id === 'transparent' && format === 'jpeg' && !pngOnce" class="bp-mark" />
        </span>
      </template>
      <span class="bp-markwrap">
        <button
          type="button"
          class="g-option"
          role="radio"
          :aria-checked="current.background.kind === 'image'"
          :aria-label="t('beautify.bg.image')"
          @click="emit('chooseImage')"
        >
          <GIcon name="image" :size="16" />
        </button>
        <span v-if="image && missing" class="bp-mark" />
      </span>
    </div>

    <div class="bp-sep" />
    <div v-if="naming" class="bp-saverow">
      <input
        ref="nameField"
        v-model="name"
        class="bp-field"
        maxlength="60"
        :placeholder="t('beautify.name')"
        :aria-label="t('beautify.name')"
        @keydown="onNameKey"
      />
      <button type="button" class="g-button" data-variant="primary" :disabled="!name.trim()" @mousedown.prevent @click="saveName">
        {{ t("beautify.saveNew") }}
      </button>
    </div>
    <div v-else :class="variant === 'glass' ? 'g-pop-actions bp-acts' : 'bp-acts'">
      <button type="button" class="g-button" :disabled="!preset || !custom" @click="emit('update')">{{ t("beautify.update") }}</button>
      <button type="button" class="g-button" @click="startNaming">{{ t("beautify.new") }}</button>
    </div>
  </div>
</template>

<style>
.bp-pan { --pop-w: 340px; --bp-well: rgba(0, 0, 0, 0.26); --bp-well-edge: var(--edge-inner); cursor: default; }
.bp-wpan {
  --bp-well: var(--win-sunken); --bp-well-edge: var(--win-line);
  box-sizing: border-box; display: flex; flex-direction: column; width: 340px; padding: 6px; border-radius: var(--radius-popover);
  background: var(--win-material); -webkit-backdrop-filter: var(--win-material-filter); backdrop-filter: var(--win-material-filter);
  box-shadow: var(--win-material-rim), var(--win-shadow-popover); color: var(--win-text);
  --surface-text: var(--win-text); --surface-text-muted: var(--win-text-2); --surface-text-faint: var(--win-text-3);
  --surface-hover: var(--win-hover, var(--control-hover)); --surface-line: var(--win-line);
}
.bp-top { position: relative; display: flex; align-items: center; gap: 8px; min-height: 40px; padding: 0 4px 0 2px; }
.bp-prewrap { flex: 0 1 auto; min-width: 0; max-width: 196px; }
.bp-pre {
  display: inline-flex; align-items: center; gap: 8px; min-width: 0; max-width: 100%; height: 32px; margin: 0; padding: 0 8px 0 6px;
  border: 0; border-radius: 16px; background: none; color: var(--surface-text); font: var(--type-label); -webkit-appearance: none; appearance: none;
}
.bp-pre:hover, .bp-pre[aria-expanded="true"] { background: var(--surface-hover); }
.bp-pre > g-icon, .bp-pre > .g-icon { color: var(--surface-text-muted); }
/* The full name shows in the tip only when the button cuts it. */
.bp-prewrap:not(:hover) > .g-tip { display: none; }
.bp-nm { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.bp-dot { flex: none; width: 16px; height: 16px; border-radius: 5px; box-shadow: inset 0 0 0 var(--hairline) rgba(255, 255, 255, 0.3), 0 0 0 var(--hairline) rgba(0, 0, 0, 0.35); }
.bp-onl { flex: none; display: inline-flex; align-items: center; gap: 8px; margin-left: auto; font: var(--type-label); color: var(--surface-text-muted); white-space: nowrap; }
.bp-menu { position: absolute; left: 0; top: calc(100% + 4px); z-index: 3; --pop-w: 240px; }
.bp-menu .g-pop-item { gap: 8px; }
.bp-prev { position: relative; display: grid; place-items: center; height: 152px; margin: 2px 2px 0; overflow: hidden; border-radius: var(--radius-lens); background: var(--bp-well); box-shadow: inset 0 0 0 var(--hairline) var(--bp-well-edge); }
.bp-out { display: block; }
.bp-meta { display: flex; justify-content: space-between; gap: 8px; padding: 7px 8px 3px; font: var(--type-num-small); font-variant-numeric: tabular-nums; color: var(--surface-text-muted); white-space: nowrap; }
.bp-desc { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.bp-row { display: grid; grid-template-columns: 108px minmax(0, 1fr) 28px; align-items: center; gap: 0 10px; min-height: 36px; padding: 0 6px 0 8px; font: var(--type-label); color: var(--surface-text); }
.bp-row[data-seg] { grid-template-columns: 108px auto; justify-content: start; }
.bp-row[aria-disabled="true"] > .bp-lab, .bp-row[aria-disabled="true"] > .bp-val { color: var(--surface-text-faint); }
.bp-row .g-seg > button { padding: 0 10px; }
.bp-lab { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.bp-val { font: var(--type-num); font-variant-numeric: tabular-nums; color: var(--surface-text-muted); text-align: right; }
.bp-lab2 { padding: 6px 8px 0; font: var(--type-label); color: var(--surface-text); }
.bp-sws { display: flex; align-items: center; flex-wrap: wrap; padding: 2px 2px 2px 4px; }
.bp-in { display: inline-flex; align-items: center; gap: 6px; font: var(--type-label); white-space: nowrap; }
.bp-in > :first-child { flex: none; color: var(--surface-text-muted); }
.bp-markwrap { position: relative; display: inline-flex; }
.bp-mark { position: absolute; right: 1px; top: 1px; width: 9px; height: 9px; border-radius: 50%; background: var(--warn); box-shadow: 0 0 0 1.5px rgba(19, 17, 32, 0.9); pointer-events: none; }
.bp-sep { height: var(--hairline); margin: 6px 8px; background: var(--surface-line, var(--edge-inner)); }
.bp-acts { display: flex; gap: 6px; padding: 0 2px 2px; }
.bp-acts > .g-button { flex: 1; }
.bp-help { display: flex; gap: 8px; align-items: flex-start; margin: 6px 2px 2px; padding: 8px 10px; border-radius: var(--radius-lens); background: var(--tone-warn-soft); font: var(--type-body); color: var(--surface-text); }
.bp-help > :first-child { flex: none; margin-top: 1px; color: var(--tone-warn); }
.bp-help .g-button { margin-top: 8px; }
.bp-note { margin: 0; padding: 0 8px 6px 126px; font: var(--type-body); color: var(--surface-text-muted); }
.bp-saverow { display: flex; align-items: center; gap: 6px; padding: 0 2px 2px; }
.bp-field {
  flex: 1; min-width: 0; height: 28px; margin: 0; padding: 0 8px; border: 0; border-radius: var(--radius-thumb); outline: 0;
  -webkit-appearance: none; appearance: none;
  background: var(--control-hover); box-shadow: inset 0 0 0 var(--hairline, 1px) var(--edge-inner);
  font: var(--type-label); color: var(--surface-text);
}
</style>
