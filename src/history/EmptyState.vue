<script setup lang="ts">
// One empty or error state: an icon, a title, one sentence that explains it, and the next step.
// A `{keys}` placeholder in the sentence becomes the shortcut's key caps.
import { computed } from "vue";

import GIcon from "../components/GIcon.vue";

const props = defineProps<{ icon: string; title: string; text: string; keys?: string | null }>();
const pieces = computed(() => props.text.split("{keys}"));
</script>

<template>
  <div class="g-empty hv-empty" role="status">
    <div class="g-empty-icon"><GIcon :name="icon" :size="32" /></div>
    <div class="g-empty-title">{{ title }}</div>
    <p class="g-empty-text">
      {{ pieces[0] }}<template v-if="pieces.length > 1"><kbd v-if="keys" class="g-kbd">{{ keys }}</kbd>{{ pieces[1] }}</template>
    </p>
    <div class="g-empty-actions"><slot /></div>
  </div>
</template>
