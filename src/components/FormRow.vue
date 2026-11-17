<script setup lang="ts">
// One settings row of the kit (`g-form-row`): the label and its help sentence on the left, the
// control on the right. A dependent row (`sub`) sits indented under the switch it depends on and
// greys out with it (`disabled`), so the layout never jumps. `stacked` gives the control the
// whole width under the label. The `after` slot holds actions under the help sentence.
import GIcon from "./GIcon.vue";

defineProps<{
  label?: string | null;
  /** The control's id, so a click on the label reaches it. */
  for?: string;
  /** An id for the label, for a control that names itself with `aria-labelledby`. */
  labelId?: string;
  help?: string | null;
  helpId?: string;
  tone?: "warn" | "danger" | "ok" | null;
  sub?: boolean;
  disabled?: boolean;
  stacked?: boolean;
  /** The label meets the first line of a tall control, such as a list of choices. */
  top?: boolean;
}>();

const icons = { warn: "warning", danger: "bang", ok: "check" } as const;
</script>

<template>
  <div
    class="g-form-row"
    :class="{ 'c-sub': sub, 'c-top': top }"
    :aria-disabled="disabled ? 'true' : undefined"
    :data-layout="stacked ? 'stacked' : undefined"
  >
    <div v-if="label || help || $slots.after || $slots.label" class="g-form-label">
      <slot name="label">
        <label v-if="label && $props.for" :for="$props.for" :id="labelId">{{ label }}</label>
        <span v-else-if="label" class="g-label" :id="labelId">{{ label }}</span>
      </slot>
      <span v-if="help" class="g-help" :data-tone="tone || undefined" :id="helpId"
        ><GIcon v-if="tone" :name="icons[tone]" :size="14" />{{ help }}</span
      >
      <slot name="after" />
    </div>
    <div v-if="$slots.default" class="g-form-control"><slot /></div>
  </div>
</template>

<style>
.c-sub > .g-form-label { margin-left: 24px; }
.c-top { align-items: flex-start; }
.c-top > .g-form-label { margin-top: 5px; }
.g-form-row[data-layout="stacked"] > .g-form-control > * { flex: 1 1 auto; min-width: 0; }
</style>
