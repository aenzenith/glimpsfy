// One entry for every window. The window's label (set by src-tauri/src/windows.rs) picks the page:
// `overlay-<display id>`, `toast`, `pin-<id>`, or one of the ordinary windows.

import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { createApp, type Component } from "vue";

import { on } from "./lib/api/core";
import { settings_get } from "./lib/api/settings";
import { app_log } from "./lib/api/windows";
import { SETTINGS_CHANGED } from "./lib/events";
import { setLanguage } from "./lib/i18n";
import { loadPlatform } from "./lib/platform";
import type { Settings } from "./lib/settings";
import "./styles/tokens.css";
import "./styles/components.css";
import "./styles/app.css";

type Page = {
  load: () => Promise<{ default: Component }>;
  floating: boolean;
  /** A transparent window whose page draws a themed panel (the popover). */
  transparent?: boolean;
};

function pageFor(label: string): Page {
  if (label.startsWith("overlay-")) return { load: () => import("./overlay/App.vue"), floating: true };
  if (label === "toast") return { load: () => import("./toast/App.vue"), floating: true };
  if (label.startsWith("pin-")) return { load: () => import("./pin/App.vue"), floating: true };
  if (label.startsWith("rec-")) return { load: () => import("./recorder/Panels.vue"), floating: true };
  switch (label) {
    case "editor":
      return { load: () => import("./editor/App.vue"), floating: false };
    case "recorder":
      return { load: () => import("./recorder/App.vue"), floating: false };
    case "popover":
      return { load: () => import("./popover/App.vue"), floating: false, transparent: true };
    case "history":
      return { load: () => import("./history/App.vue"), floating: false };
    case "settings":
      return { load: () => import("./settings/App.vue"), floating: false };
    default:
      return { load: () => import("./firstrun/App.vue"), floating: false };
  }
}

/** Ordinary windows and the popover follow Settings › General › Appearance: the system's light or
 * dark appearance, or the one the user chose. */
let appearance: Settings["general"]["appearance"] = "system";
let themeApply: (() => void) | null = null;
function followTheme(): void {
  const dark = window.matchMedia("(prefers-color-scheme: dark)");
  const apply = () => {
    const theme = appearance === "system" ? (dark.matches ? "dark" : "light") : appearance;
    document.documentElement.setAttribute("data-theme", theme);
  };
  apply();
  dark.addEventListener("change", apply);
  themeApply = apply;
}

/** The last language asked for, which the page waits for before its first paint. */
let languageSet: Promise<void> = Promise.resolve();

/** Every window follows the language, and ordinary windows and the popover the theme too. */
function follow(settings: Settings): void {
  languageSet = setLanguage(settings.general.language);
  appearance = settings.general.appearance;
  themeApply?.();
}

/** Errors of hidden pages (overlays, the toast) reach the core's log instead of a console nobody
 * has open. */
function forwardErrors(): void {
  const send = (level: "error" | "warn", parts: unknown[]) =>
    void app_log(level, parts.map((p) => (p instanceof Error ? `${p.message}\n${p.stack ?? ""}` : String(p))).join(" ")).catch(() => {});
  for (const level of ["error", "warn"] as const) {
    const original = console[level].bind(console);
    console[level] = (...parts: unknown[]) => {
      original(...parts);
      send(level, parts);
    };
  }
  window.addEventListener("error", (e) => send("error", [e.error ?? e.message]));
  window.addEventListener("unhandledrejection", (e) => send("error", [e.reason]));
}

const EDITABLE = 'input, textarea, [contenteditable=""], [contenteditable="true"]';

function editable(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(EDITABLE) !== null;
}

/** Keeps the webview on its page. A window without Tauri's drop handler (the popover, pins, the
 * toast, the recording panels) hands a dropped file to WebKit, which opens it in place of the
 * page: a capture dragged out of the popover and let go over it turned the popover into that
 * picture, with no page left to hide it. Drops a page does not take itself are refused here, and
 * WebKit's own menu (Reload, Inspect Element) gives way to the page's or to none; text fields keep
 * theirs, and in development ⌥ right-click still reaches Inspect Element. */
function guardWebview(): void {
  const refuse = (e: DragEvent) => {
    if (e.defaultPrevented || editable(e.target)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "none";
  };
  window.addEventListener("dragenter", refuse);
  window.addEventListener("dragover", refuse);
  window.addEventListener("drop", (e) => {
    if (!editable(e.target)) e.preventDefault();
  });
  window.addEventListener("contextmenu", (e) => {
    if (e.defaultPrevented || editable(e.target)) return;
    if (import.meta.env.DEV && e.altKey) return;
    e.preventDefault();
  });
}

async function start(): Promise<void> {
  forwardErrors();
  guardWebview();
  const page = pageFor(getCurrentWebviewWindow().label);
  document.documentElement.dataset.surface = page.floating ? "floating" : page.transparent ? "popover" : "window";
  if (!page.floating) followTheme();

  // One listener for the language and the theme: every window registers it, and each listener is
  // a call at start and a script run in that window on every change. A change heard before the
  // settings below answered is newer than they are.
  let heard = false;
  void on<Settings>(SETTINGS_CHANGED, (settings) => {
    heard = true;
    follow(settings);
  });

  // The platform, the language and the theme before the first paint, so no page flashes in the
  // wrong one or shows, for a moment, what this platform cannot do.
  const platformRead = loadPlatform();
  try {
    const settings = await settings_get();
    if (!heard) follow(settings);
  } catch {
    if (!heard) languageSet = setLanguage("system");
  }
  await Promise.all([languageSet, platformRead]);

  const { default: root } = await page.load();
  createApp(root).mount("#app");
}

void start();
