<script setup lang="ts">
// The heading of a group of rows (`g-section-head`), with an optional sentence under it and room
// on the right for an action (the `default` slot).
defineProps<{ title: string; desc?: string | null }>();
</script>

<template>
  <div class="g-section-head">
    <div>
      <h2 class="g-section-title">{{ title }}</h2>
      <p v-if="desc" class="g-section-desc">{{ desc }}</p>
    </div>
    <slot />
  </div>
</template>
