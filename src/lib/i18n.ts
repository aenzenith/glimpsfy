// A tiny typed translator. English is the reference catalog: every other catalog must have every
// English key, or the type check fails. Keys are flat ("toast.copied"), and `{name}` placeholders
// are filled from `params`. Settings › General › Language picks the language; "system" follows
// the first of the system's preferred languages that Glimpsfy speaks. The core reads the same
// files for the menu bar menu (`src-tauri/src/i18n.rs`).

import { ref } from "vue";

import de from "../locales/de.json";
import en from "../locales/en.json";
import es from "../locales/es.json";
import ja from "../locales/ja.json";
import ru from "../locales/ru.json";
import tr from "../locales/tr.json";
import zhHans from "../locales/zh-Hans.json";
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

const catalogs: Record<Language, Catalog> = { en, tr, de, es, ru, ja, "zh-Hans": zhHans };

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

export function setLanguage(choice: LanguageChoice): void {
  language.value = resolve(choice);
  if (typeof document !== "undefined") document.documentElement.lang = language.value;
}

export function t(key: Key, params: Record<string, string | number> = {}): string {
  const text = catalogs[language.value][key] ?? catalogs.en[key];
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in params ? String(params[name]) : whole));
}
