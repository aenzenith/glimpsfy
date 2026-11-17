<script setup lang="ts">
// A control with the kit's tooltip (`g-tipwrap` and `g-tip`): the tip names the action and its
// keys, and shows on hover and keyboard focus after the kit's delay.
import KeyCaps from "./KeyCaps.vue";

defineProps<{ tip: string; caps?: readonly string[]; side?: "above" | "below"; align?: "start" | "end" }>();
</script>

<template>
  <span class="g-tipwrap" :data-side="side === 'below' ? 'below' : undefined" :data-align="align">
    <slot />
    <span class="g-tip" role="tooltip">{{ tip }}<KeyCaps v-if="caps?.length" :caps="caps" /></span>
  </span>
</template>
