<script setup lang="ts">
// An app's mark: its first letter on a tinted tile. The history keeps the app's name and bundle
// id, not its icon, so the tile stands in until the core can serve app icons.
import { computed } from "vue";

const props = defineProps<{ name: string; size?: 14 | 16 }>();
const letter = computed(() => (props.name.trim()[0] ?? "?").toLocaleUpperCase());
</script>

<template>
  <span class="hv-appicon" :data-size="size ?? 16" aria-hidden="true">{{ letter }}</span>
</template>
