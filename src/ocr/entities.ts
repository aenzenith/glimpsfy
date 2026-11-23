// Pure helpers of the text actions (A3): what a found piece offers, the address its main action
// opens, the event a date becomes and how a date reads in the user's language. The layer
// (`TextLayer.vue`) and its menu (`EntityMenu.vue`) use them; they touch no DOM and no bridge.

import type { Entity, EntityDate, EntityKind, OcrResult } from "../lib/api/ocr";
import type { CalendarEvent } from "../lib/api/windows";
import type { Key } from "../lib/i18n";

/** The kinds the layer underlines; smart redaction asks for the others. */
export const ACTION_KINDS: EntityKind[] = ["phone", "email", "link", "date"];

/** What a piece's menu offers. */
export type EntityAction = {
  /** The main action, run with Return. */
  icon: string;
  label: Key;
  /** The address `app_open_url` opens; `null` for a date, which goes to the calendar. */
  url: string | null;
  /** What the menu says when no app on the computer opens `url`. */
  noApp: Key;
  /** The copy row, run with ⌘C. */
  copyLabel: Key;
  copyText: string;
  /** The value is an address or a link, shown in the monospaced face and wrapped anywhere. */
  mono: boolean;
};

export function actionOf(entity: Entity): EntityAction {
  switch (entity.kind) {
    case "phone":
      return {
        icon: "phone",
        label: "ocr.action.call",
        url: `tel:${entity.value}`,
        noApp: "ocr.action.noPhoneApp",
        copyLabel: "ocr.action.copyNumber",
        copyText: entity.text.trim(),
        mono: false,
      };
    case "email":
      return {
        icon: "mail",
        label: "ocr.action.email",
        url: `mailto:${entity.value}`,
        noApp: "ocr.action.noMailApp",
        copyLabel: "ocr.action.copyAddress",
        copyText: entity.value,
        mono: true,
      };
    case "link":
      return {
        icon: "link",
        label: "ocr.code.open",
        url: entity.value,
        noApp: "ocr.action.failed",
        copyLabel: "ocr.action.copyLink",
        copyText: entity.value,
        mono: true,
      };
    default:
      return {
        icon: "calendar",
        label: "ocr.action.calendar",
        url: null,
        noApp: "ocr.action.failed",
        copyLabel: "ocr.action.copyDate",
        copyText: entity.text.trim(),
        mono: false,
      };
  }
}

/** A local date and time without a zone ("2026-11-27T14:30:00") as a `Date` in local time. */
export function localDate(text: string): Date {
  const [day, time = "00:00:00"] = text.split("T");
  const [y, m, d] = day.split("-").map(Number);
  const [h, min, s] = time.split(":").map((part) => Number.parseFloat(part) || 0);
  return new Date(y, (m || 1) - 1, d || 1, h, min, Math.floor(s));
}

const DAY: Intl.DateTimeFormatOptions = { weekday: "long", day: "numeric", month: "long", year: "numeric" };
const CLOCK: Intl.DateTimeFormatOptions = { hour: "2-digit", minute: "2-digit" };

function format(locale: string, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  try {
    return new Intl.DateTimeFormat(locale, options);
  } catch {
    return new Intl.DateTimeFormat(undefined, options);
  }
}

/** How a found date reads at the top of its menu, in the user's language: "Cuma 27 Kasım 2026 ·
 * 14:30–15:30", a day alone for an all-day date, and a range over days with both ends. */
export function dateTitle(date: EntityDate, locale: string): string {
  const start = localDate(date.start);
  const end = date.end ? localDate(date.end) : null;
  if (date.allDay) {
    return end && end.getTime() !== start.getTime() ? format(locale, DAY).formatRange(start, end) : format(locale, DAY).format(start);
  }
  const day = format(locale, DAY).format(start);
  if (!end) return `${day} · ${format(locale, CLOCK).format(start)}`;
  if (end.toDateString() === start.toDateString()) return `${day} · ${format(locale, CLOCK).formatRange(start, end)}`;
  return format(locale, { ...DAY, ...CLOCK }).formatRange(start, end);
}

/** The event a date becomes: its line as the title (tabs between table cells as spaces), and the
 * date's own start, end and all-day flag. */
export function eventOf(entity: Entity, result: OcrResult): CalendarEvent | null {
  if (!entity.date) return null;
  const line = result.lines[entity.boxes[0]?.line ?? -1]?.text ?? "";
  const title = line.replace(/\s+/g, " ").trim() || entity.text.trim();
  return { title, start: entity.date.start, end: entity.date.end, allDay: entity.date.allDay };
}
