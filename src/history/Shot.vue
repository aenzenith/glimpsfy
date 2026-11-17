<script setup lang="ts">
// A capture inside its box: covering a card from the top, contained in the inspector, or zoomed
// onto a match with the rest veiled, as the spotlight tool does. The boxes of the matches are in
// pixels of `size`, the image the text was read from; the image shown may be the thumbnail or the
// file, and is scaled to that size either way.
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import type { TextBox } from "../lib/api/history";
import { place } from "./model";

const props = defineProps<{
  src: string | null;
  /** The pixel size the boxes refer to (the capture's own). */
  size: { width: number; height: number } | null;
  mode: "cover" | "contain";
  /** The box the card's view is in, when the caller knows it; otherwise it is measured. */
  box?: { w: number; h: number } | null;
  boxes?: readonly TextBox[];
  /** The match the view centres on and leaves unveiled. */
  current?: number | null;
  /** Outlines every match, not only the current one (the inspector). */
  all?: boolean;
  /** Zooms a cover view onto the current match (a search result). */
  zoom?: boolean;
  scale?: number | null;
}>();

const root = ref<HTMLElement | null>(null);
const measured = ref<{ w: number; h: number }>({ w: 0, h: 0 });
const natural = ref<{ w: number; h: number } | null>(null);
const failed = ref(false);
let observer: ResizeObserver | null = null;

onMounted(() => {
  if (props.box || !root.value) return;
  observer = new ResizeObserver(([entry]) => {
    measured.value = { w: entry.contentRect.width, h: entry.contentRect.height };
  });
  observer.observe(root.value);
});
onBeforeUnmount(() => observer?.disconnect());
watch(
  () => props.src,
  () => {
    failed.value = false;
  },
);

const view = computed(() => props.box ?? measured.value);
const image = computed(() => (props.size ? { w: props.size.width, h: props.size.height } : (natural.value ?? { w: 0, h: 0 })));
const focus = computed(() => {
  const i = props.current;
  return i != null && props.boxes && props.boxes[i] ? props.boxes[i] : null;
});
const at = computed(() => place(view.value, image.value, props.mode, props.zoom ? focus.value : null, props.scale ?? 1));

const imgStyle = computed(() => ({
  left: `${at.value.x}px`,
  top: `${at.value.y}px`,
  width: `${image.value.w * at.value.k}px`,
  height: `${image.value.h * at.value.k}px`,
}));

const outlines = computed(() => {
  if (!props.boxes?.length || !image.value.w) return [];
  const { x, y, k } = at.value;
  const pad = 2;
  const list = props.all ? props.boxes.map((b, i) => ({ b, i })) : focus.value ? [{ b: focus.value, i: props.current ?? 0 }] : [];
  return list.map(({ b, i }) => ({
    key: i,
    current: i === props.current,
    style: { left: `${x + b.x * k - pad}px`, top: `${y + b.y * k - pad}px`, width: `${b.width * k + 2 * pad}px`, height: `${b.height * k + 2 * pad}px` },
  }));
});

function loaded(event: Event): void {
  const img = event.target as HTMLImageElement;
  if (!props.size) natural.value = { w: img.naturalWidth, h: img.naturalHeight };
}
</script>

<template>
  <div ref="root" class="hv-shot">
    <img v-if="src && !failed && image.w" :src="src" alt="" draggable="false" decoding="async" :style="imgStyle" @load="loaded" @error="failed = true" />
    <img v-else-if="src && !failed" :src="src" alt="" draggable="false" style="visibility: hidden" @load="loaded" @error="failed = true" />
    <slot v-if="!src || failed" name="missing" />
    <span v-for="o in outlines" :key="o.key" class="hv-match" :data-current="o.current ? '' : undefined" :style="o.style" />
  </div>
</template>
