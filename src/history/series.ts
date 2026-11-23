// The words of an interval series in the history: its frame count, interval and the line under
// its card, shared by the card, the frame band, the inspector and the list.

import type { Entry } from "../lib/api/history";
import { t, type Language } from "../lib/i18n";
import { seriesSpan } from "./model";

export function framesText(n: number): string {
  return n === 1 ? t("history.series.frames.one") : t("history.series.frames.other", { n });
}

/** "every 5 s", or minutes once the interval is a whole number of them. */
export function everyText(ms: number): string {
  if (ms >= 60_000 && ms % 60_000 === 0) return t("history.series.every.minutes", { n: ms / 60_000 });
  return t("history.series.every.seconds", { n: Math.max(1, Math.round(ms / 1000)) });
}

export function skippedMark(n: number): string {
  return n === 1 ? t("history.series.skippedMark.one") : t("history.series.skippedMark.other", { n });
}

/** The card's line: "14:32–14:37 · 60 frames · every 5 s". */
export function seriesSub(entry: Entry, lang: Language): string {
  const s = entry.series;
  if (!s) return seriesSpan(entry, lang);
  return [seriesSpan(entry, lang), framesText(s.count), everyText(s.intervalMs)].join(" · ");
}

/** The band's line: "60 frames · 1 skipped · every 5 s". */
export function seriesMeta(entry: Entry): string {
  const s = entry.series;
  if (!s) return "";
  return [framesText(s.count), s.skipped > 0 ? t("history.series.skipped", { n: s.skipped }) : "", everyText(s.intervalMs)].filter(Boolean).join(" · ");
}
