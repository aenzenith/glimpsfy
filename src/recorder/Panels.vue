<script setup lang="ts">
// The recorder's floating panels. The core makes and places each `rec-*` window, and its label
// picks what the page draws: the target (`rec-target`), the setup and control bars (`rec-bar`),
// the recorded canvas for click rings and keystrokes (`rec-canvas`) and the webcam bubble's hover
// controls (`rec-bubble-ctl`).
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { defineAsyncComponent, type Component } from "vue";

const PAGES: Record<string, () => Promise<Component>> = {
  "rec-target": () => import("./Target.vue"),
  "rec-bar": () => import("./Bar.vue"),
  "rec-canvas": () => import("./Canvas.vue"),
  "rec-bubble-ctl": () => import("./BubbleControls.vue"),
};

const label = getCurrentWebviewWindow().label;
const load = PAGES[label];
if (!load) console.error(`The recorder has no panel named ${label}.`);
const page = load ? defineAsyncComponent(load) : null;
</script>

<template>
  <component :is="page" v-if="page" />
</template>
