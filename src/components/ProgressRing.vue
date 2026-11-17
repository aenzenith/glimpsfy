<script setup lang="ts">
// The kit's progress ring (`g-ring`): a share from 0 to 100, or a spinning arc while the share is
// unknown (`progress` null).
defineProps<{ progress: number | null; label: string; size?: number }>();
</script>

<template>
  <svg
    class="g-ring"
    viewBox="0 0 22 22"
    :style="{ '--progress': progress ?? 0, '--ring-size': `${size ?? 18}px` }"
    :data-state="progress === null ? 'indeterminate' : undefined"
    role="progressbar"
    :aria-label="label"
    aria-valuemin="0"
    aria-valuemax="100"
    :aria-valuenow="progress === null ? undefined : Math.round(progress)"
  >
    <circle cx="11" cy="11" r="8" pathLength="100" />
    <circle cx="11" cy="11" r="8" pathLength="100" />
  </svg>
</template>
