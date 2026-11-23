<script setup lang="ts">
// Key caps (`g-keys` of `g-kbd`) for a shortcut, from caps such as ["⇧", "⌘", "2"] or from an
// accelerator such as "Shift+CommandOrControl+2". Caps written in macOS's form show in the
// platform's words (Ctrl, Shift) on Windows and Linux.
import { computed } from "vue";

import { capsOf, chordCaps } from "../lib/keys";

const props = defineProps<{ caps?: readonly string[]; accelerator?: string | null }>();
const shown = computed(() => (props.caps ? chordCaps(props.caps) : capsOf(props.accelerator)));
</script>

<template>
  <span v-if="shown.length" class="g-keys"><kbd v-for="(cap, i) in shown" :key="i" class="g-kbd">{{ cap }}</kbd></span>
</template>
