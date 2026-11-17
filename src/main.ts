// One entry for every window. The window's label (set by src-tauri/src/windows.rs) picks the page:
// `overlay-<display id>`, `toast`, `pin-<id>`, or one of the ordinary windows.

import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { createApp, type Component } from "vue";

import { on } from "./lib/api/core";
import { settings_get } from "./lib/api/settings";
import { app_log } from "./lib/api/windows";
import { SETTINGS_CHANGED } from "./lib/events";
import { setLanguage } from "./lib/i18n";
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
  void on<Settings>(SETTINGS_CHANGED, (settings) => {
    appearance = settings.general.appearance;
    apply();
  });
  themeApply = apply;
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

async function start(): Promise<void> {
  forwardErrors();
  const page = pageFor(getCurrentWebviewWindow().label);
  document.documentElement.dataset.surface = page.floating ? "floating" : page.transparent ? "popover" : "window";
  if (!page.floating) followTheme();

  // The language and the theme before the first paint, so no page flashes in the wrong one.
  try {
    const settings = await settings_get();
    setLanguage(settings.general.language);
    appearance = settings.general.appearance;
    themeApply?.();
  } catch {
    setLanguage("system");
  }
  void on<Settings>(SETTINGS_CHANGED, (settings) => setLanguage(settings.general.language));

  const { default: root } = await page.load();
  createApp(root).mount("#app");
}

void start();
