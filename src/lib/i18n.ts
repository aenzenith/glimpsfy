// A tiny typed translator. English is the reference catalog: every other catalog must have every
// English key, or the type check fails. Keys are flat ("toast.copied"), and `{name}` placeholders
// are filled from `params`. Settings › General › Language picks the language; "system" follows
// the first of the system's preferred languages that Glimpsfy speaks. The core reads the same
// files for the menu bar menu (`src-tauri/src/i18n.rs`).

import { ref } from "vue";

import en from "../locales/en.json";
import type { Language as LanguageChoice } from "./settings";

export type Key = keyof typeof en;

/** Every key English has, so a catalog missing one fails the type check. */
type Catalog = { [K in Key]: string };

/** Every language Glimpsfy speaks, each under its own name, in the order Settings lists them. */
export const LANGUAGES = [
  { code: "en", name: "English" },
  { code: "tr", name: "Türkçe" },
  { code: "de", name: "Deutsch" },
  { code: "es", name: "Español" },
  { code: "ru", name: "Русский" },
  { code: "ja", name: "日本語" },
  { code: "zh-Hans", name: "简体中文" },
] as const;

export type Language = (typeof LANGUAGES)[number]["code"];

// Every window is a page of its own that loads this module (the overlays, the toast, the popover,
// each pin, the recorder's panels), and a page speaks one language: loading only that catalog
// keeps the other six (some 600 KB of text) out of every page's memory. The type of each loader
// still holds every catalog to every English key.
const LOADERS: Record<Exclude<Language, "en">, () => Promise<{ default: Catalog }>> = {
  tr: () => import("../locales/tr.json"),
  de: () => import("../locales/de.json"),
  es: () => import("../locales/es.json"),
  ru: () => import("../locales/ru.json"),
  ja: () => import("../locales/ja.json"),
  "zh-Hans": () => import("../locales/zh-Hans.json"),
};

const catalogs: Partial<Record<Language, Catalog>> = { en };

/** The language the pages speak now. */
export const language = ref<Language>(resolve("system"));

/** The first of the preferred languages (`tr-TR`, `zh-Hans-CN`) there is a catalog for. */
function pick(tags: readonly string[]): Language {
  for (const tag of tags) {
    const parts = tag.replace(/_/g, "-").toLowerCase().split("-");
    const primary = parts[0];
    if (primary === "zh") {
      // Traditional Chinese (Taiwan, Hong Kong, Macau) has no catalog yet.
      const traditional = parts.some((p) => ["hant", "tw", "hk", "mo"].includes(p));
      if (traditional && !parts.includes("hans")) continue;
      return "zh-Hans";
    }
    const found = LANGUAGES.find((l) => l.code === primary);
    if (found) return found.code;
  }
  return "en";
}

/** The language for a Settings choice. */
export function resolve(choice: LanguageChoice): Language {
  if (choice !== "system") return choice;
  if (typeof navigator === "undefined") return "en";
  return pick(navigator.languages?.length ? navigator.languages : [navigator.language]);
}

let turns = 0;

/** Switches the pages to a language once its catalog has loaded, so no page shows a mix. */
export async function setLanguage(choice: LanguageChoice): Promise<void> {
  const next = resolve(choice);
  const turn = ++turns;
  if (next !== "en" && !catalogs[next]) {
    try {
      catalogs[next] = (await LOADERS[next]()).default;
    } catch (error) {
      // The page speaks English instead, which `t` falls back to.
      console.warn("The language could not be loaded.", next, error);
    }
  }
  // A newer choice made while this one loaded has the last word.
  if (turn !== turns) return;
  language.value = next;
  if (typeof document !== "undefined") document.documentElement.lang = next;
}

export function t(key: Key, params: Record<string, string | number> = {}): string {
  const text = (catalogs[language.value] ?? en)[key] ?? en[key];
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in params ? String(params[name]) : whole));
}
