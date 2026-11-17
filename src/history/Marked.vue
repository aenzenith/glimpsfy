<script setup lang="ts">
// Text with its matches in iris, without v-html: either the ranges the store returned (UTF-16
// units of `text`) or the page's own folding of `query`.
import { computed } from "vue";

import { matchRanges, parts, type Range } from "./fold";

const props = defineProps<{ text: string; query?: string; ranges?: readonly Range[] }>();

const pieces = computed(() => parts(props.text, props.ranges ?? (props.query ? matchRanges(props.text, props.query) : [])));
</script>

<template>
  <template v-for="(p, i) in pieces" :key="i"><mark v-if="p.hit" class="hv-hit">{{ p.text }}</mark><template v-else>{{ p.text }}</template></template>
</template>
