<script setup lang="ts">
// The recorded canvas (`rec-canvas`): a click-through window that covers the recorded rectangle
// exactly and is part of the file, so it draws nothing but what belongs to the video. A click
// draws the ring (36 points rippling to 58 over 520 ms, in the colour and size Settings › Recording
// chose). Shortcut keys show as key caps that fade 1.2 s after the last one; they sit at their
// place (bottom centre by default) and move to the opposite side while the pointer or the webcam
// bubble comes within 24 points of it, unless that side is taken too. Reduce Motion drops the
// ripple and the slide.
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, shallowRef } from "vue";

import type { LogicalPoint } from "../lib/api/capture";
import {
  onRecordBubble,
  onRecordCanvas,
  onRecordClick,
  onRecordKeys,
  onRecordPointer,
  record_bubble_state,
  record_canvas_state,
  type BubbleState,
  type CanvasConfig,
  type RingColor,
} from "../lib/api/record";
import { placeKeys, toLocal, type Rect } from "./geometry";

/** How long a ring lives: the ripple's 520 ms and a little room. */
const RING_MS = 560;
/** How long the key caps stay after the last key. */
const KEYS_MS = 1200;
/** The ring's diameter for each size. */
const RING_SIZE = { small: 36, large: 54 } as const;
const RING_COLOR: Record<RingColor, string> = {
  yellow: "var(--ink-yellow)",
  red: "var(--ink-red)",
  blue: "var(--ink-blue)",
  white: "var(--ink-white)",
};

const config = shallowRef<CanvasConfig | null>(null);
const bubble = shallowRef<BubbleState | null>(null);
const pointer = ref<LogicalPoint | null>(null);
const rings = ref<{ id: number; x: number; y: number }[]>([]);
const caps = ref<string[]>([]);
const keysShown = ref(false);
const keysEl = ref<HTMLElement | null>(null);
const keysSize = reactive({ width: 0, height: 0 });

let ringId = 0;
let keysTimer = 0;
const stops: (() => void)[] = [];

const origin = computed(() => config.value?.rect ?? { x: 0, y: 0, width: window.innerWidth, height: window.innerHeight });

const ringStyle = computed(() => {
  const c = config.value?.clicks;
  const size = RING_SIZE[c?.size ?? "small"];
  return { "--r-ring": RING_COLOR[c?.color ?? "yellow"], "--r-ring-size": `${size}px`, "--r-ring-reach": `${Math.round((size * 11) / 36)}px` };
});

/** What the keystroke overlay keeps away from, in local points. */
const near = computed<Rect[]>(() => {
  const list: Rect[] = [];
  const p = pointer.value;
  if (p) list.push({ x: p.x, y: p.y, width: 0, height: 0 });
  const b = bubble.value;
  if (b?.visible) list.push(toLocal(b.rect, origin.value));
  return list;
});

const keysAt = computed(() => {
  const c = config.value;
  if (!c) return { left: "0px", top: "0px" };
  const place = placeKeys(origin.value, keysSize, c.keystrokes.position, near.value, c.liftBottom, c.dropTop);
  return { left: `${Math.round(place.x)}px`, top: `${Math.round(place.y)}px` };
});

function measureKeys(): void {
  const el = keysEl.value;
  if (!el) return;
  keysSize.width = el.offsetWidth;
  keysSize.height = el.offsetHeight;
}

function click(p: LogicalPoint): void {
  const c = config.value;
  if (!c?.clicks.enabled) return;
  const id = ++ringId;
  rings.value.push({ id, x: p.x - origin.value.x, y: p.y - origin.value.y });
  window.setTimeout(() => {
    rings.value = rings.value.filter((ring) => ring.id !== id);
  }, RING_MS);
}

async function keys(next: { caps: string[] }): Promise<void> {
  if (!config.value?.keystrokes.enabled || !next.caps.length) return;
  caps.value = next.caps;
  keysShown.value = true;
  window.clearTimeout(keysTimer);
  keysTimer = window.setTimeout(() => (keysShown.value = false), KEYS_MS);
  await nextTick();
  measureKeys();
}

const observer = new ResizeObserver(measureKeys);

onMounted(async () => {
  let heardConfig = false;
  let heardBubble = false;
  try {
    stops.push(
      await onRecordCanvas((next) => {
        heardConfig = true;
        config.value = next;
      }),
    );
    stops.push(await onRecordClick(click));
    stops.push(await onRecordKeys((next) => void keys(next)));
    stops.push(
      await onRecordPointer((p) => {
        pointer.value = p ? { x: p.x - origin.value.x, y: p.y - origin.value.y } : null;
      }),
    );
    stops.push(
      await onRecordBubble((next) => {
        heardBubble = true;
        bubble.value = next;
      }),
    );
    const [first, firstBubble] = await Promise.all([record_canvas_state(), record_bubble_state()]);
    if (!heardConfig) config.value = first;
    if (!heardBubble) bubble.value = firstBubble;
  } catch (error) {
    console.error("The recording canvas could not load.", error);
  }
  if (keysEl.value) observer.observe(keysEl.value);
});

onUnmounted(() => {
  window.clearTimeout(keysTimer);
  observer.disconnect();
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div class="r-canvas" :style="ringStyle" aria-hidden="true">
    <div v-for="ring in rings" :key="ring.id" class="r-click" :style="{ left: `${ring.x}px`, top: `${ring.y}px` }" />
    <div
      ref="keysEl"
      class="g-glass r-keys"
      :data-size="config?.keystrokes.size ?? 'medium'"
      :data-shown="keysShown ? 'true' : 'false'"
      :style="keysAt"
    >
      <kbd v-for="(cap, i) in caps" :key="i" class="r-key">{{ cap }}</kbd>
    </div>
  </div>
</template>

<style>
.r-canvas { position: fixed; inset: 0; overflow: hidden; pointer-events: none; }

/* the click ring: the colour is the user's ink, with a white line and a soft glow around it */
.r-click {
  position: absolute; width: var(--r-ring-size); height: var(--r-ring-size);
  margin: calc(var(--r-ring-size) / -2) 0 0 calc(var(--r-ring-size) / -2); border-radius: 50%;
  background: color-mix(in srgb, var(--r-ring) 26%, transparent);
  box-shadow: 0 0 0 2px var(--r-ring), 0 0 0 3.5px var(--selection-inner);
  animation: r-click 520ms var(--ease-out) both;
}
.r-click::after { content: ""; position: absolute; inset: calc(-1 * var(--r-ring-reach)); border-radius: 50%;
  box-shadow: 0 0 0 1.5px color-mix(in srgb, var(--r-ring) 45%, transparent); }
@keyframes r-click { from { transform: scale(0.6); opacity: 1; } 70% { opacity: 1; } to { transform: scale(1.15); opacity: 0; } }
@keyframes r-click-still { 0%, 70% { opacity: 1; } to { opacity: 0; } }

/* the keystroke overlay: 36, 44 or 56 px key caps on glass. Faded out, it is also hidden once the
   fade ends (as the kit's tips are): its glass is a composited backdrop layer, which opacity 0
   alone leaves in the recorded window's layers for the whole take. */
.r-keys {
  position: absolute; display: inline-flex; align-items: center; gap: var(--space-6); padding: var(--space-6);
  border-radius: var(--radius-card); white-space: nowrap;
  transition: opacity var(--dur-base) var(--ease-out), top var(--dur-move) var(--ease-out), visibility 0s;
}
.r-keys[data-shown="false"] { opacity: 0; visibility: hidden; transition-delay: 0s, 0s, var(--dur-base); }
.r-key {
  display: inline-grid; place-items: center; min-width: var(--r-cap); height: var(--r-cap); padding: 0 var(--r-cap-pad);
  border-radius: var(--r-cap-radius); background: var(--control-fill); box-shadow: var(--surface-kbd-shadow);
  color: var(--text); font: 600 var(--r-cap-font) / 1 var(--font); white-space: nowrap;
}
.r-keys[data-size="small"] { --r-cap: 36px; --r-cap-pad: 10px; --r-cap-radius: 10px; --r-cap-font: 16px; }
.r-keys[data-size="medium"] { --r-cap: 44px; --r-cap-pad: 12px; --r-cap-radius: 12px; --r-cap-font: 20px; }
.r-keys[data-size="large"] { --r-cap: 56px; --r-cap-pad: 15px; --r-cap-radius: 14px; --r-cap-font: 26px; }

@media (prefers-reduced-motion: reduce) {
  .r-click { animation-name: r-click-still; }
  .r-keys { transition: opacity var(--dur-base) var(--ease-out), visibility 0s; }
  .r-keys[data-shown="false"] { transition-delay: 0s, var(--dur-base); }
}
</style>
