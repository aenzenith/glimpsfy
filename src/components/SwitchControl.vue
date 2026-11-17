<script setup lang="ts">
// The kit's switch (`g-switch`): a checkbox with the switch role, bound to a boolean.
defineProps<{ modelValue: boolean; id?: string; disabled?: boolean; label?: string }>();
const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();
</script>

<template>
  <input
    :id="id"
    type="checkbox"
    role="switch"
    class="g-switch"
    :checked="modelValue"
    :disabled="disabled"
    :aria-label="label"
    @change="emit('update:modelValue', ($event.target as HTMLInputElement).checked)"
  />
</template>
