<script setup lang="ts">
// A pinned capture: an always-on-top window that shows one image (src-tauri/src/pins.rs). The
// window's frame is the image plus `margin` points on every side; the page draws the rounded
// image, the float shadow and the glass edge in that margin (the design's .s-pin), so the image
// and its shadow fade with the opacity while the controls stay solid.
//
// Drag moves the pin; a corner resizes it and keeps its aspect, anchored at the opposite corner;
// the wheel zooms at the pointer up to 800 % and ⌘0 returns to actual size; 1–9 set the opacity
// to 10–90 % and 0 to 100 %; ⌘L locks it (clicks go through, ⌥ wakes it); ⌘M or a double-click
// collapses it to a 96 pt tile at its top-left and restores it; ⌘C ⌘S ⌘O ⌘W ⌥⌘W copy, save, edit
// and close. The right button opens the native menu.
import { CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu } from "@tauri-apps/api/menu";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { computed, onMounted, onUnmounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import KeyCaps from "../components/KeyCaps.vue";
import TipWrap from "../components/TipWrap.vue";
import type { LogicalRect } from "../lib/api/capture";
import { onWindow } from "../lib/api/core";
import {
  PIN_OPTION_KEY,
  pin_close,
  pin_close_all,
  pin_copy,
  pin_edit,
  pin_frame,
  pin_image,
  pin_info,
  pin_save,
  pin_set_all_spaces,
  pin_set_frame,
  pin_set_locked,
  type PinInfo,
  type PinOptionKey,
} from "../lib/api/windows";
import { t } from "../lib/i18n";
import { codeOf, reasonText } from "../toast/reason";

const MAX_ZOOM = 8;
/** The collapsed tile's side in points. */
const TILE = 96;
/** The smallest side a corner drag leaves the image. */
const MIN_SIDE = 40;
/** Below this width only Copy and Opacity fit in the tools (DESIGN.md). */
const NARROW = 220;
const ERROR_MS = 4000;

const info = ref<PinInfo | null>(null);
const src = ref<string | null>(null);
const alpha = ref(1);
/** Zoom and the image's offset as shares of the pin's size, so a resize keeps the view. */
const zoom = ref(1);
const offset = ref({ x: 0, y: 0 });
const locked = ref(false);
const optionHeld = ref(false);
/** Pins open on every Space (pins.rs builds them so); `PinInfo` does not say it yet. */
const allSpaces = ref(true);
/** The frame before collapsing; `null` while the pin is open. */
const collapsedFrom = ref<LogicalRect | null>(null);
const showOpacity = ref(false);
const sizeLabel = ref<{ w: number; h: number; p: number } | null>(null);
const error = ref<string | null>(null);
const width = ref(window.innerWidth);

const margin = computed(() => info.value?.margin ?? 24);
const naturalWidth = computed(() => (info.value ? info.value.pixelWidth / info.value.scale : 1));
const naturalHeight = computed(() => (info.value ? info.value.pixelHeight / info.value.scale : 1));
const aspect = computed(() => naturalWidth.value / naturalHeight.value);
const narrow = computed(() => width.value - 2 * margin.value < NARROW);
const controls = computed(() => !locked.value && !collapsedFrom.value);
const percent = (value: number) => t("pin.percent", { p: Math.round(value) });

const mediaStyle = computed(() =>
  collapsedFrom.value || zoom.value === 1
    ? {}
    : { transform: `translate(${offset.value.x * 100}%, ${offset.value.y * 100}%) scale(${zoom.value})` },
);

// ------------------------------------------------------------------------------------ errors

let errorTimer: ReturnType<typeof setTimeout> | null = null;
function fail(cause: unknown): void {
  error.value = reasonText(codeOf(cause));
  if (errorTimer) clearTimeout(errorTimer);
  errorTimer = setTimeout(() => (error.value = null), ERROR_MS);
}

async function attempt(action: () => Promise<unknown>): Promise<void> {
  try {
    await action();
  } catch (cause) {
    fail(cause);
  }
}

// ------------------------------------------------------------------------------------- frame

let wanted: LogicalRect | null = null;
let sending = false;

/** Sends the latest wanted frame; frames asked for while one is on its way replace each other. */
function setFrame(frame: LogicalRect): void {
  wanted = frame;
  if (!sending) void flush();
}

async function flush(): Promise<void> {
  sending = true;
  while (wanted) {
    const frame = wanted;
    wanted = null;
    try {
      await pin_set_frame(frame);
    } catch (cause) {
      fail(cause);
      wanted = null;
    }
  }
  sending = false;
}

const imageOf = (frame: LogicalRect) => ({
  x: frame.x + margin.value,
  y: frame.y + margin.value,
  width: frame.width - 2 * margin.value,
  height: frame.height - 2 * margin.value,
});

const frameOf = (image: { x: number; y: number; width: number; height: number }): LogicalRect => ({
  x: image.x - margin.value,
  y: image.y - margin.value,
  width: image.width + 2 * margin.value,
  height: image.height + 2 * margin.value,
});

let sizeTimer: ReturnType<typeof setTimeout> | null = null;
function showSize(w: number, h: number): void {
  sizeLabel.value = { w: Math.round(w), h: Math.round(h), p: (w / naturalWidth.value) * 100 };
  if (sizeTimer) clearTimeout(sizeTimer);
  sizeTimer = setTimeout(() => (sizeLabel.value = null), 900);
}

// ----------------------------------------------------------------------------- corner resize

type Corner = "nw" | "ne" | "sw" | "se";
const CORNERS: Corner[] = ["nw", "ne", "sw", "se"];

type Drag = {
  corner: Corner;
  /** Where the pointer went down, in screen points. */
  x: number;
  y: number;
  /** The image's rect when the drag started; `null` until `pin_frame` answers. */
  start: { x: number; y: number; width: number; height: number } | null;
  last: { x: number; y: number };
};
let drag: Drag | null = null;

async function resizeDown(event: PointerEvent, corner: Corner): Promise<void> {
  if (event.button !== 0) return;
  event.preventDefault();
  event.stopPropagation();
  (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  const current: Drag = { corner, x: event.screenX, y: event.screenY, start: null, last: { x: event.screenX, y: event.screenY } };
  drag = current;
  try {
    // The window may have moved since the last frame this page set, so ask for it.
    current.start = imageOf(await pin_frame());
    if (drag === current) resizeTo(current.last.x, current.last.y);
  } catch (cause) {
    drag = null;
    fail(cause);
  }
}

function resizeMove(event: PointerEvent): void {
  if (!drag) return;
  drag.last = { x: event.screenX, y: event.screenY };
  resizeTo(event.screenX, event.screenY);
}

function resizeTo(screenX: number, screenY: number): void {
  const start = drag?.start;
  if (!drag || !start) return;
  const corner = drag.corner;
  const sx = corner.endsWith("e") ? 1 : -1;
  const sy = corner.startsWith("s") ? 1 : -1;
  const dx = screenX - drag.x;
  const dy = screenY - drag.y;
  // The axis the pointer moved more along, relative to the size, leads; the other follows the aspect.
  const byWidth = start.width + sx * dx;
  const byHeight = (start.height + sy * dy) * aspect.value;
  let w = Math.abs(dx) / start.width >= Math.abs(dy) / start.height ? byWidth : byHeight;
  w = Math.max(w, MIN_SIDE, MIN_SIDE * aspect.value);
  const h = w / aspect.value;
  const x = sx > 0 ? start.x : start.x + start.width - w;
  const y = sy > 0 ? start.y : start.y + start.height - h;
  setFrame(frameOf({ x, y, width: w, height: h }));
  showSize(w, h);
}

function resizeUp(event: PointerEvent): void {
  if (!drag) return;
  (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
  drag = null;
}

// ------------------------------------------------------------------------------ move, collapse

function pinDown(event: PointerEvent): void {
  if (event.button !== 0 || (locked.value && !optionHeld.value)) return;
  if ((event.target as HTMLElement).closest("button, input, .s-pin-corner, .s-pin-strip, .s-pin-wake")) return;
  if (event.detail === 2) {
    void toggleCollapse();
    return;
  }
  getCurrentWindow()
    .startDragging()
    .catch((cause) => console.warn("startDragging", cause));
}

async function toggleCollapse(): Promise<void> {
  if (locked.value) return;
  await attempt(async () => {
    const now = await pin_frame();
    if (collapsedFrom.value) {
      // Restore at the place the tile was moved to.
      const from = collapsedFrom.value;
      await pin_set_frame({ x: now.x, y: now.y, width: from.width, height: from.height });
      collapsedFrom.value = null;
    } else {
      showOpacity.value = false;
      collapsedFrom.value = now;
      const image = imageOf(now);
      await pin_set_frame(frameOf({ x: image.x, y: image.y, width: TILE, height: TILE }));
    }
  });
}

async function actualSize(): Promise<void> {
  if (collapsedFrom.value) return;
  zoom.value = 1;
  offset.value = { x: 0, y: 0 };
  await attempt(async () => {
    const image = imageOf(await pin_frame());
    setFrame(frameOf({ x: image.x, y: image.y, width: naturalWidth.value, height: naturalHeight.value }));
    showSize(naturalWidth.value, naturalHeight.value);
  });
}

// --------------------------------------------------------------------------------------- zoom

const media = ref<HTMLElement | null>(null);

function wheel(event: WheelEvent): void {
  if (collapsedFrom.value || locked.value || !media.value) return;
  event.preventDefault();
  const rect = media.value.getBoundingClientRect();
  const step = event.deltaMode === 1 ? event.deltaY * 16 : event.deltaY;
  const next = Math.min(MAX_ZOOM, Math.max(1, zoom.value * Math.exp(-step * 0.0025)));
  if (next === zoom.value) return;
  // The point under the pointer stays under it.
  const fx = (event.clientX - rect.left) / rect.width;
  const fy = (event.clientY - rect.top) / rect.height;
  const px = (fx - offset.value.x) / zoom.value;
  const py = (fy - offset.value.y) / zoom.value;
  const clamp = (v: number) => Math.min(0, Math.max(1 - next, v));
  offset.value = next === 1 ? { x: 0, y: 0 } : { x: clamp(fx - px * next), y: clamp(fy - py * next) };
  zoom.value = next;
}

// ---------------------------------------------------------------------------------- opacity

function setAlpha(value: number): void {
  alpha.value = Math.min(1, Math.max(0.1, value));
}

function sliderInput(event: Event): void {
  setAlpha(Number((event.target as HTMLInputElement).value) / 100);
}

// ------------------------------------------------------------------------------- lock, spaces

async function toggleLock(): Promise<void> {
  const next = !locked.value;
  await attempt(async () => {
    await pin_set_locked(next);
    locked.value = next;
    showOpacity.value = false;
    if (!next) optionHeld.value = false;
  });
}

async function toggleAllSpaces(): Promise<void> {
  const next = !allSpaces.value;
  await attempt(async () => {
    await pin_set_all_spaces(next);
    allSpaces.value = next;
  });
}

// ------------------------------------------------------------------------------------ actions

const copy = () => attempt(pin_copy);
const save = () => attempt(pin_save);
// A history pin opens its entry with the drawings; any other pin opens its own copy of the image.
const edit = () => attempt(pin_edit);
const close = () => attempt(pin_close);
const closeAll = () => attempt(pin_close_all);

// --------------------------------------------------------------------------------------- menu

async function contextMenu(event: MouseEvent): Promise<void> {
  event.preventDefault();
  if (locked.value && !optionHeld.value) return;
  const separator = () => PredefinedMenuItem.new({ item: "Separator" });
  try {
    const items: (MenuItem | CheckMenuItem | Submenu | PredefinedMenuItem)[] = [
      await MenuItem.new({ text: t("pin.menu.copy"), accelerator: "CmdOrCtrl+C", action: () => void copy() }),
      await MenuItem.new({ text: t("pin.menu.save"), accelerator: "CmdOrCtrl+S", action: () => void save() }),
    ];
    items.push(await MenuItem.new({ text: t("pin.menu.edit"), accelerator: "CmdOrCtrl+O", action: () => void edit() }));
    const levels = await Promise.all(
      [100, 90, 75, 50, 25].map((p) =>
        CheckMenuItem.new({ text: percent(p), checked: Math.round(alpha.value * 100) === p, action: () => setAlpha(p / 100) }),
      ),
    );
    items.push(
      await separator(),
      await Submenu.new({ text: t("pin.menu.opacity"), items: levels }),
      await MenuItem.new({
        text: t("pin.menu.actualSize"),
        accelerator: "CmdOrCtrl+0",
        enabled: !collapsedFrom.value,
        action: () => void actualSize(),
      }),
      await separator(),
      await CheckMenuItem.new({
        text: t("pin.menu.lock"),
        accelerator: "CmdOrCtrl+L",
        checked: locked.value,
        action: () => void toggleLock(),
      }),
      await CheckMenuItem.new({ text: t("pin.menu.allSpaces"), checked: allSpaces.value, action: () => void toggleAllSpaces() }),
      await separator(),
      await MenuItem.new({
        text: collapsedFrom.value ? t("pin.menu.restore") : t("pin.menu.collapse"),
        accelerator: "CmdOrCtrl+M",
        enabled: !locked.value,
        action: () => void toggleCollapse(),
      }),
      await MenuItem.new({ text: t("pin.menu.close"), accelerator: "CmdOrCtrl+W", action: () => void close() }),
      await MenuItem.new({ text: t("pin.menu.closeAll"), accelerator: "Alt+CmdOrCtrl+W", action: () => void closeAll() }),
    );
    const menu = await Menu.new({ items });
    await menu.popup();
  } catch (cause) {
    console.warn("pin menu", cause);
  }
}

// ----------------------------------------------------------------------------------- keyboard

function onKey(event: KeyboardEvent): void {
  if ((event.target as HTMLElement).closest("input")) {
    if (event.key === "Escape") showOpacity.value = false;
    return;
  }
  const mod = event.metaKey || event.ctrlKey;
  const key = event.key.toLowerCase();
  let handled = true;
  if (mod && event.altKey && event.code === "KeyW") void closeAll();
  else if (mod && key === "l") void toggleLock();
  else if (locked.value) handled = false;
  else if (mod && key === "c") void copy();
  else if (mod && key === "s") void save();
  else if (mod && key === "o") void edit();
  else if (mod && key === "w") void close();
  else if (mod && key === "m") void toggleCollapse();
  else if (mod && key === "0") void actualSize();
  else if (!mod && !event.altKey && /^Digit\d$/.test(event.code)) {
    const digit = Number(event.code.slice(5));
    setAlpha(digit === 0 ? 1 : digit / 10);
  } else if (event.key === "Escape" && showOpacity.value) showOpacity.value = false;
  else handled = false;
  if (handled) event.preventDefault();
}

// ------------------------------------------------------------------------------------- wiring

const stops: (() => void)[] = [];
const resized = () => (width.value = window.innerWidth);

onMounted(async () => {
  window.addEventListener("keydown", onKey);
  window.addEventListener("resize", resized);
  try {
    info.value = await pin_info();
    allSpaces.value = info.value.allSpaces;
    const bytes = await pin_image();
    const blob = new Blob([bytes], { type: info.value.imageType === "jpeg" ? "image/jpeg" : "image/png" });
    src.value = URL.createObjectURL(blob);
  } catch (cause) {
    error.value = t("pin.loadFailed");
    console.warn("pin load", cause);
  }
  stops.push(
    await onWindow<PinOptionKey>(PIN_OPTION_KEY, ({ held }) => {
      optionHeld.value = held && locked.value;
    }),
  );
});

onUnmounted(() => {
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("resize", resized);
  stops.forEach((stop) => stop());
  if (src.value) URL.revokeObjectURL(src.value);
  for (const timer of [errorTimer, sizeTimer]) if (timer) clearTimeout(timer);
});
</script>

<template>
  <div
    class="s-pin"
    role="figure"
    tabindex="0"
    :aria-label="t('pin.aria', { name: info?.name ?? '' })"
    :title="collapsedFrom ? t('pin.collapsed') : undefined"
    :data-locked="locked && !optionHeld ? '' : undefined"
    :data-collapsed="collapsedFrom ? '' : undefined"
    :style="{ inset: `${margin}px`, '--pin-alpha': alpha }"
    @pointerdown="pinDown"
    @wheel="wheel"
    @contextmenu="contextMenu"
  >
    <div ref="media" class="s-pin-media">
      <img v-if="src" :src="src" alt="" draggable="false" :style="mediaStyle" />
    </div>

    <template v-if="controls">
      <div class="g-glass s-pin-close">
        <TipWrap :tip="t('pin.close')" :caps="['⌘', 'W']" side="below">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('pin.close')" @click="close">
            <GIcon name="close" :size="16" />
          </button>
        </TipWrap>
      </div>
      <div class="g-glass s-pin-tools" role="toolbar" :aria-label="t('pin.tools')">
        <TipWrap :tip="t('pin.copy')" :caps="['⌘', 'C']" side="below">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('pin.copy')" @click="copy">
            <GIcon name="copy" :size="16" />
          </button>
        </TipWrap>
        <TipWrap v-if="!narrow" :tip="t('pin.save')" :caps="['⌘', 'S']" side="below">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('pin.save')" @click="save">
            <GIcon name="save" :size="16" />
          </button>
        </TipWrap>
        <TipWrap :tip="t('pin.opacity')" side="below" align="end">
          <button
            type="button"
            class="g-btn"
            data-size="small"
            :aria-label="t('pin.opacity')"
            :aria-pressed="showOpacity"
            @click="showOpacity = !showOpacity"
          >
            <GIcon name="opacity" :size="16" />
          </button>
        </TipWrap>
        <TipWrap v-if="!narrow" :tip="t('pin.edit')" :caps="['⌘', 'O']" side="below" align="end">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('pin.edit')" @click="edit">
            <GIcon name="external-link" :size="16" />
          </button>
        </TipWrap>
      </div>
      <div v-if="showOpacity" class="g-strip s-pin-strip">
        <GIcon name="opacity" :size="16" />
        <input
          type="range"
          class="g-slider"
          min="10"
          max="100"
          step="1"
          :value="Math.round(alpha * 100)"
          :style="{ '--value': `${((alpha * 100 - 10) / 90) * 100}%` }"
          :aria-label="t('pin.opacity')"
          @input="sliderInput"
        />
        <span class="s-pin-value">{{ percent(alpha * 100) }}</span>
      </div>
      <span
        v-for="corner in CORNERS"
        :key="corner"
        class="s-pin-corner"
        :data-corner="corner"
        :aria-label="t('pin.resize')"
        @pointerdown="resizeDown($event, corner)"
        @pointermove="resizeMove"
        @pointerup="resizeUp"
        @pointercancel="resizeUp"
      />
      <span v-if="zoom > 1" class="g-tag s-pin-zoom"><GIcon name="zoom-in" :size="12" />{{ percent(zoom * 100) }}</span>
    </template>

    <span v-if="collapsedFrom" class="g-status" data-status="info"><GIcon name="fit" :size="12" /></span>

    <div v-if="locked && optionHeld" class="g-glass s-pin-wake">
      <GIcon name="lock" :size="16" />
      <span>{{ t("pin.locked") }}</span>
      <button type="button" class="g-button" @click="toggleLock">{{ t("pin.unlock") }}<KeyCaps :caps="['⌘', 'L']" /></button>
    </div>

    <div v-if="sizeLabel" class="g-size s-pin-size" aria-live="polite">
      {{ sizeLabel.w }}<span class="g-size-x">×</span>{{ sizeLabel.h }}<span class="g-size-x">·</span>{{ percent(sizeLabel.p) }}
    </div>
    <div v-if="error" class="g-glass s-pin-error" role="alert">
      <GIcon name="warning" :size="14" /><span>{{ error }}</span>
    </div>
  </div>
</template>

<style>
/* The pin's own parts, ported from design/round-2/small-surfaces (.s-pin). The window is its own
   page, so these rules reach nothing else. */
.s-pin {
  position: absolute;
  border-radius: var(--radius-thumb);
  --pin-alpha: 1;
  outline: 0;
  cursor: default;
}
.s-pin::before {
  content: "";
  position: absolute;
  inset: 0;
  border-radius: inherit;
  box-shadow: 0 0 0 var(--hairline) var(--edge-outer), var(--shadow-float);
  opacity: var(--pin-alpha);
  pointer-events: none;
}
.s-pin-media {
  position: absolute;
  inset: 0;
  border-radius: inherit;
  overflow: hidden;
  opacity: var(--pin-alpha);
}
.s-pin-media > img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: cover;
  transform-origin: 0 0;
  -webkit-user-drag: none;
}
.s-pin::after {
  content: "";
  position: absolute;
  inset: 0;
  z-index: 1;
  border-radius: inherit;
  box-shadow: inset 0 0 0 var(--hairline) var(--edge-inner), inset 0 1px 0 0 var(--edge-bottom);
  opacity: var(--pin-alpha);
  pointer-events: none;
}
.s-pin-close,
.s-pin-tools {
  position: absolute;
  top: 6px;
  z-index: 2;
  display: inline-flex;
  padding: 2px;
  border-radius: 16px;
  transition: opacity var(--dur-base) var(--ease-out);
}
.s-pin-close { left: 6px; }
.s-pin-tools { right: 6px; }
.s-pin-strip {
  position: absolute;
  right: 6px;
  top: 44px;
  z-index: 2;
  gap: 8px;
  padding: 4px 12px 4px 10px;
  transition: opacity var(--dur-base) var(--ease-out);
}
.s-pin-strip > g-icon { color: var(--text-muted); }
.s-pin-strip > .g-slider { width: 112px; }
.s-pin-strip > .s-pin-value { min-width: 34px; font: var(--type-num); font-variant-numeric: tabular-nums; text-align: right; }
.s-pin:not(:hover):not(:focus-within) > :is(.s-pin-close, .s-pin-tools, .s-pin-strip, .s-pin-zoom) {
  opacity: 0;
  pointer-events: none;
}
.s-pin:focus-visible { outline: 2px solid var(--iris-text); outline-offset: 3px; }
.s-pin > .s-pin-zoom { position: absolute; left: 8px; bottom: 8px; z-index: 2; transition: opacity var(--dur-base) var(--ease-out); }
.s-pin > .g-status { position: absolute; right: -6px; bottom: -6px; z-index: 2; }
.s-pin-corner { position: absolute; z-index: 3; width: 16px; height: 16px; }
.s-pin-corner[data-corner="nw"] { left: -6px; top: -6px; cursor: nwse-resize; }
.s-pin-corner[data-corner="se"] { right: -6px; bottom: -6px; cursor: nwse-resize; }
.s-pin-corner[data-corner="ne"] { right: -6px; top: -6px; cursor: nesw-resize; }
.s-pin-corner[data-corner="sw"] { left: -6px; bottom: -6px; cursor: nesw-resize; }
.s-pin-wake {
  position: absolute;
  left: 50%;
  top: 8px;
  z-index: 3;
  transform: translateX(-50%);
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 36px;
  padding: 0 4px 0 10px;
  border-radius: 18px;
  font: var(--type-label);
  white-space: nowrap;
  pointer-events: auto;
}
.s-pin-wake > g-icon { color: var(--text-muted); }
.s-pin[data-locked] { pointer-events: none; }
.s-pin-size { position: absolute; left: 50%; bottom: 8px; z-index: 3; transform: translateX(-50%); pointer-events: none; }
.s-pin-error {
  position: absolute;
  left: 8px;
  right: 8px;
  bottom: 8px;
  z-index: 3;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 10px;
  border-radius: var(--radius-thumb);
  font: var(--type-body);
}
.s-pin-error > g-icon { flex: none; color: var(--warn); }
</style>
