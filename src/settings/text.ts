// Small helpers of the settings window: error sentences, sizes, folder names and translations
// whose key comes from another area at run time.

import { homeDir } from "@tauri-apps/api/path";
import { ref } from "vue";

import { ApiError } from "../lib/api/core";
import { language, t, tp, type Key } from "../lib/i18n";
import { platform } from "../lib/platform";

/** A sentence for a failed command: what happened and what to do. */
export function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "not_implemented":
        return t("settings.error.notImplemented");
      case "permission_denied":
        return tp("settings.error.permission");
      case "io":
        return t("settings.error.io", { detail: error.message });
      case "invalid_argument":
        return t("settings.error.invalid", { detail: error.message });
      case "not_found":
        return t("settings.error.notFound", { detail: error.message });
      case "provider":
        return t("settings.error.provider", { detail: error.message });
      case "not_confirmed":
        return t("error.not_confirmed");
      case "uploads_off":
        return t("error.uploads_off");
      case "offline":
        return t("error.offline");
      case "keychain":
        return tp("settings.error.keychain", { detail: error.message });
      default:
        return t("settings.error.other", { detail: error.message });
    }
  }
  return t("settings.error.other", { detail: String(error) });
}

export const isNotImplemented = (error: unknown) => error instanceof ApiError && error.code === "not_implemented";

/** A translation whose key another area owns (a provider form's labels, help and rules), in this
 * platform's words where it has them. A key that is not in the catalogs yet shows `fallback`
 * instead of breaking the page. */
export function tr(key: string | null | undefined, params: Record<string, string | number> = {}, fallback = ""): string {
  if (!key) return fallback;
  try {
    const text = tp(key as Key, params);
    return typeof text === "string" ? text : fallback || key;
  } catch {
    return fallback || key;
  }
}

/** A byte count for people: "1.42 GB" in English, "1,42 GB" in Turkish. */
export function formatBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : value < 10 ? 2 : value < 100 ? 1 : 0;
  return `${new Intl.NumberFormat(language.value, { maximumFractionDigits: digits }).format(value)} ${units[unit]}`;
}

export const formatCount = (n: number) => new Intl.NumberFormat(language.value).format(n);

/** A folder as the user knows it: the part under the home folder joined with "›", such as
 * "Pictures › Glimpsfy", or the whole path outside it. Windows separates folders with either
 * slash, the other systems with "/" alone. */
export function folderLabel(path: string, home: string | null): string {
  const separator = platform.os === "windows" ? /[\\/]/ : /\//;
  const trailing = platform.os === "windows" ? /[\\/]+$/ : /\/+$/;
  const trimmed = path.replace(trailing, "");
  if (home) {
    const base = home.replace(trailing, "");
    if (trimmed === base) return "~";
    if (trimmed.startsWith(base) && separator.test(trimmed.charAt(base.length))) return trimmed.slice(base.length + 1).split(separator).join(" › ");
  }
  return trimmed || "/";
}

/** The time of day for a result line, such as 14:32. */
export const timeOfDay = (ms: number) =>
  new Intl.DateTimeFormat(language.value, { hour: "2-digit", minute: "2-digit" }).format(new Date(ms));

/** A date for a "checked on" line, such as 2 Nov 2026. */
export function dayOf(iso: string): string {
  const date = new Date(`${iso}T12:00:00`);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(language.value, { day: "numeric", month: "short", year: "numeric" }).format(date);
}

/** A unique id for a label and its control. */
let next = 0;
export const uid = (prefix: string) => `${prefix}-${++next}`;

/** The user's home folder, for `folderLabel`; `null` until it is known. */
export const home = ref<string | null>(null);
void homeDir()
  .then((path) => (home.value = path))
  .catch(() => {});
