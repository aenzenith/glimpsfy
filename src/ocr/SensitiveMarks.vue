<script setup lang="ts">
// The marks smart redaction (D28) puts over what it found while its list is open: a dashed iris
// frame with a faint fill per box, a solid one for the item under the pointer in the list, a faded
// one for an item switched off, and an amber bar on a side the area's edge cuts. They are page
// elements over the image, so they never reach an export.
import { computed } from "vue";

import type { View } from "../annotate/textLayer";
import type { Sensitive } from "./sensitive";

const props = defineProps<{ sensitive: Sensitive; view: View }>();

/** Width of the bar on a cut side, in points. */
const CUT = 3;

const marks = computed(() => {
  const s = props.sensitive;
  if (!s.open.value || s.phase.value !== "done") return [];
  const { k, ox, oy } = props.view;
  return s.items.value.flatMap((item) =>
    item.marks.map((mark, i) => {
      const x = ox + mark.rect.x * k;
      const y = oy + mark.rect.y * k;
      const w = mark.rect.width * k;
      const h = mark.rect.height * k;
      const bars = mark.cut.flatMap((edge) => {
        if (edge === "left") return [{ left: x, top: y, width: CUT, height: h }];
        if (edge === "right") return [{ left: x + w - CUT, top: y, width: CUT, height: h }];
        if (edge === "top") return [{ left: x, top: y, width: w, height: CUT }];
        return [{ left: x, top: y + h - CUT, width: w, height: CUT }];
      });
      return { key: `${item.id}-${i}`, box: { left: x, top: y, width: w, height: h }, hl: s.hovered.value === item.id, off: s.off.value.has(item.id), bars };
    }),
  );
});

const css = (r: { left: number; top: number; width: number; height: number }) => ({
  left: `${r.left}px`,
  top: `${r.top}px`,
  width: `${r.width}px`,
  height: `${r.height}px`,
});
</script>

<template>
  <div class="s-marks" aria-hidden="true">
    <template v-for="m in marks" :key="m.key">
      <div class="s-mark" :style="css(m.box)" :data-hl="m.hl ? '' : undefined" :data-off="m.off ? '' : undefined" />
      <span v-for="(bar, i) in m.bars" :key="i" class="s-cut" :style="css(bar)" />
    </template>
  </div>
</template>

<style scoped>
.s-marks {
  position: absolute;
  inset: 0;
  pointer-events: none;
}
.s-mark {
  position: absolute;
  border-radius: 4px;
  outline: 1.5px dashed var(--iris);
  background: color-mix(in srgb, var(--iris) 10%, transparent);
}
.s-mark[data-hl] {
  outline: 2px solid var(--iris);
  background: color-mix(in srgb, var(--iris) 18%, transparent);
}
.s-mark[data-off] {
  outline-color: color-mix(in srgb, var(--iris) 45%, transparent);
  background: none;
}
.s-cut {
  position: absolute;
  border-radius: 1px;
  background: var(--warn);
}
</style>
