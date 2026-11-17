// The recorder's shared wording: clocks, sizes, device kinds and the sentence for each failure a
// command reports, so every page says the same thing the same way.

import { ApiError, type ErrorCode } from "../lib/api/core";
import type { DeviceTransport } from "../lib/api/record";
import { language, t, type Key } from "../lib/i18n";

/** "0:42" or "1:02:05" for an elapsed time. */
export function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/** "0:12.4": minutes, seconds and tenths, for the trim window. */
export function tenths(ms: number): string {
  const v = Math.round(Math.max(0, ms) / 100);
  const m = Math.floor(v / 600);
  const rest = (v - m * 600) / 10;
  const seconds = new Intl.NumberFormat(language.value, {
    minimumIntegerDigits: 2,
    minimumFractionDigits: 1,
    maximumFractionDigits: 1,
    useGrouping: false,
  }).format(rest);
  return `${m}:${seconds}`;
}

/** "≈ 16 MB": an estimate, rounded the way a person reads it. */
export function approxSize(bytes: number): string {
  const n = (value: number, digits: number) =>
    new Intl.NumberFormat(language.value, { maximumFractionDigits: digits, minimumFractionDigits: digits }).format(value);
  if (bytes < 1e6) return `≈ ${n(Math.max(1, Math.round(bytes / 1000)), 0)} KB`;
  if (bytes < 1e9) return `≈ ${n(bytes / 1e6, bytes < 1e7 ? 1 : 0)} MB`;
  return `≈ ${n(bytes / 1e9, 1)} GB`;
}

/** "Today at 14:32", "Yesterday at 9:05" or "12 Nov 2026 14:32". */
export function createdAt(ms: number, now = Date.now()): string {
  const day = (v: number) => {
    const d = new Date(v);
    return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  };
  const time = new Intl.DateTimeFormat(language.value, { hour: "numeric", minute: "2-digit" }).format(ms);
  const days = Math.round((day(now) - day(ms)) / 86_400_000);
  if (days === 0) return t("record.trim.today", { time });
  if (days === 1) return t("record.trim.yesterday", { time });
  const date = new Intl.DateTimeFormat(language.value, { day: "numeric", month: "short", year: "numeric" }).format(ms);
  return `${date} ${time}`;
}

const TRANSPORT: Record<DeviceTransport, Key | null> = {
  builtIn: "record.device.builtIn",
  usb: "record.device.usb",
  bluetooth: "record.device.bluetooth",
  virtual: "record.device.virtual",
  other: null,
};

/** The sub label of a device in a menu, or nothing for a kind without a name. */
export function transportLabel(transport: DeviceTransport): string {
  const key = TRANSPORT[transport];
  return key ? t(key) : "";
}

/** Where a failure happened, which decides the sentence for codes that mean different things. */
export type ErrorPlace = "setup" | "take" | "export" | "upload" | "trash";

const BY_PLACE: Record<ErrorPlace, Partial<Record<ErrorCode, Key>>> = {
  setup: {
    permission_denied: "record.error.setup.permission_denied",
    not_found: "record.error.setup.not_found",
    invalid_argument: "record.error.setup.invalid_argument",
  },
  take: {
    not_found: "record.error.take.not_found",
  },
  export: {
    not_found: "record.error.export.not_found",
    invalid_argument: "record.error.export.invalid_argument",
    io: "record.error.export.io",
  },
  upload: {
    permission_denied: "record.error.upload.permission_denied",
    invalid_argument: "record.error.upload.invalid_argument",
    provider: "record.error.upload.provider",
    not_found: "record.error.upload.not_found",
  },
  trash: {
    not_found: "record.error.trash.not_found",
    io: "record.error.trash.io",
  },
};

const GENERAL: Partial<Record<ErrorCode, Key>> = {
  permission_denied: "record.error.permission_denied",
  busy: "record.error.busy",
  io: "record.error.io",
  capture: "record.error.capture",
  not_implemented: "record.error.not_implemented",
  not_found: "record.error.not_found",
  invalid_argument: "record.error.invalid_argument",
};

/** The code of a failure; anything that is not an `ApiError` counts as `internal`. */
export function codeOf(error: unknown): ErrorCode {
  return error instanceof ApiError ? error.code : "internal";
}

/** The sentence that says what failed and what to do. */
export function errorText(error: unknown, place: ErrorPlace): string {
  const code = codeOf(error);
  return t(BY_PLACE[place][code] ?? GENERAL[code] ?? "record.error.other");
}

/** Whether the user's reduced-motion preference is on. */
export function reducedMotion(): boolean {
  return typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}
