<script setup lang="ts" generic="T extends string | number">
// The kit's pop-up button (`g-select` around a native select), bound to a typed value.
const props = defineProps<{
  modelValue: T;
  options: readonly { value: T; label: string }[];
  id?: string;
  disabled?: boolean;
  minWidth?: number;
  label?: string;
}>();
const emit = defineEmits<{ "update:modelValue": [value: T] }>();

function change(event: Event): void {
  const index = (event.target as HTMLSelectElement).selectedIndex;
  const option = props.options[index];
  if (option) emit("update:modelValue", option.value);
}
</script>

<template>
  <label class="g-select" :style="minWidth ? { minWidth: `${minWidth}px` } : undefined">
    <select :id="id" :disabled="disabled" :aria-label="label" @change="change">
      <option v-for="option in options" :key="String(option.value)" :selected="option.value === modelValue">
        {{ option.label }}
      </option>
    </select>
  </label>
</template>
