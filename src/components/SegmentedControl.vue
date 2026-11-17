<script setup lang="ts" generic="T extends string | number">
// The kit's segmented control (`g-seg`): a radio group of buttons. Arrow keys move the choice, as
// they do in a native one, and only the chosen button sits in the tab order.
import { ref } from "vue";

const props = defineProps<{
  modelValue: T;
  options: readonly { value: T; label: string }[];
  /** The id of the label that names the group. */
  labelledby?: string;
  label?: string;
  disabled?: boolean;
}>();
const emit = defineEmits<{ "update:modelValue": [value: T] }>();

const root = ref<HTMLElement | null>(null);

function pick(value: T, focus = false): void {
  if (props.disabled) return;
  if (value !== props.modelValue) emit("update:modelValue", value);
  if (focus) {
    const index = props.options.findIndex((o) => o.value === value);
    (root.value?.children[index] as HTMLElement | undefined)?.focus();
  }
}

function key(event: KeyboardEvent): void {
  const step = ({ ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 } as Record<string, number>)[event.key];
  if (!step) return;
  event.preventDefault();
  const index = props.options.findIndex((o) => o.value === props.modelValue);
  const next = props.options[(index + step + props.options.length) % props.options.length];
  if (next) pick(next.value, true);
}
</script>

<template>
  <div ref="root" class="g-seg" role="radiogroup" :aria-labelledby="labelledby" :aria-label="label" @keydown="key">
    <button
      v-for="option in options"
      :key="String(option.value)"
      type="button"
      role="radio"
      :aria-checked="option.value === modelValue"
      :tabindex="option.value === modelValue ? 0 : -1"
      :disabled="disabled"
      @click="pick(option.value)"
    >
      {{ option.label }}
    </button>
  </div>
</template>
