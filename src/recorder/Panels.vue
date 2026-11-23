<script setup lang="ts">
// The recorder's floating panels. The core makes and places each `rec-*` window, and its label
// picks what the page draws: the target (`rec-target`), the setup and control bars (`rec-bar`),
// the recorded canvas for click rings, keystrokes, drawings and the spotlight (`rec-canvas`), the
// drawing rail (`rec-rail`) and the webcam bubble's hover controls (`rec-bubble-ctl`).
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { defineAsyncComponent, type Component } from "vue";

import { can } from "../lib/platform";

const PAGES: Record<string, () => Promise<Component>> = {
  "rec-target": () => import("./Target.vue"),
  "rec-bar": () => import("./Bar.vue"),
  "rec-canvas": () => import("./Canvas.vue"),
  "rec-rail": () => import("./Rail.vue"),
  "rec-bubble-ctl": () => import("./BubbleControls.vue"),
};

const label = getCurrentWebviewWindow().label;
// Without a native blur under the bar (`nativeGlass`), the panels over the live desktop take the
// kit's solid glass, since a backdrop filter there blurs nothing.
if (!can("nativeGlass")) document.documentElement.classList.add("no-blur");
const load = PAGES[label];
if (!load) console.error(`The recorder has no panel named ${label}.`);
const page = load ? defineAsyncComponent(load) : null;
</script>

<template>
  <component :is="page" v-if="page" />
</template>
