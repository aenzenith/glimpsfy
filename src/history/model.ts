// The history window's logic without Vue: filters and queries, formatting, the virtual grid's
// layout, Finder-like selection and the zoom of a thumbnail onto a match.

import type { Entry, EntryId, EntryKind, HistoryItem, HistoryQuery, SortOrder, TextBox } from "../lib/api/history";
import type { Language } from "../lib/i18n";
import { platform } from "../lib/platform";

// ------------------------------------------------------------------------------------ filters

export type Filter = "all" | "screenshots" | "recordings" | "textCopies" | "uploaded" | "pinned" | `app:${string}`;

export const LIBRARY_FILTERS = ["all", "screenshots", "recordings", "textCopies"] as const;
export const STATUS_FILTERS = ["uploaded", "pinned"] as const;

const KIND_OF: Partial<Record<Filter, EntryKind>> = { screenshots: "screenshot", recordings: "recording", textCopies: "textCopy" };

export function queryFor(filter: Filter, sort: SortOrder, offset: number, limit: number): HistoryQuery {
  const kind = KIND_OF[filter];
  return {
    kinds: kind ? [kind] : [],
    uploaded: filter === "uploaded" ? true : null,
    pinned: filter === "pinned" ? true : null,
    app: filter.startsWith("app:") ? filter.slice(4) : null,
    sort,
    offset,
    limit,
  };
}

/** Whether an entry belongs in the filter, for a live change that arrives without a query. */
export function passes(entry: Entry, filter: Filter): boolean {
  const kind = KIND_OF[filter];
  if (kind) return entry.kind === kind;
  if (filter === "uploaded") return entry.hosted !== null;
  if (filter === "pinned") return entry.pinned;
  if (filter.startsWith("app:")) return entry.source.appName === filter.slice(4);
  return true;
}

// --------------------------------------------------------------------------------- formatting

export function formatBytes(bytes: number | null | undefined, lang: Language): string {
  if (bytes == null) return "—";
  const n = (value: number, digits: number) =>
    new Intl.NumberFormat(lang, { maximumFractionDigits: digits, minimumFractionDigits: digits }).format(value);
  if (bytes >= 1e9) return `${n(bytes / 1e9, 1)} GB`;
  if (bytes >= 1e6) return `${n(bytes / 1e6, 1)} MB`;
  if (bytes >= 1e3) return `${n(Math.round(bytes / 1e3), 0)} KB`;
  return `${bytes} B`;
}

export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return "—";
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

export function formatTime(ms: number, lang: Language): string {
  return new Intl.DateTimeFormat(lang, { hour: "numeric", minute: "2-digit" }).format(ms);
}

/** A time with its seconds, such as 14:32:05, for the frames of a series. */
export function formatClock(ms: number, lang: Language): string {
  return new Intl.DateTimeFormat(lang, { hour: "2-digit", minute: "2-digit", second: "2-digit" }).format(ms);
}

/** When a series ran, "14:32–14:37", or its start alone while it has one frame. */
export function seriesSpan(entry: Pick<Entry, "createdAtMs" | "durationMs">, lang: Language): string {
  const start = formatTime(entry.createdAtMs, lang);
  return entry.durationMs ? `${start}–${formatTime(entry.createdAtMs + entry.durationMs, lang)}` : start;
}

/** A series' folder as the user knows it: the folders under the home folder joined with "›", the
 * middle ones folded into "…" when there are more than three, so the series' own folder stays in
 * view ("Documents › … › Glimpsfy › Interval 2026-11-22 14.32"). */
export function folderTrail(path: string, home: string | null): string {
  const separator = platform.os === "windows" ? /[\\/]/ : /\//;
  const trailing = platform.os === "windows" ? /[\\/]+$/ : /\/+$/;
  let rest = path.replace(trailing, "");
  if (home) {
    const base = home.replace(trailing, "");
    if (rest.startsWith(base) && separator.test(rest.charAt(base.length))) rest = rest.slice(base.length + 1);
  }
  const parts = rest.split(separator).filter(Boolean);
  return (parts.length > 3 ? [parts[0], "…", ...parts.slice(-2)] : parts).join(" › ");
}

/** The path of a series frame's file inside the series' folder. */
export function framePath(folder: string, file: string): string {
  const separator = platform.os === "windows" && folder.includes("\\") ? "\\" : "/";
  return `${folder.replace(/[\\/]+$/, "")}${separator}${file}`;
}

export function dayKey(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
}

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** A day header: "Today" with the date beside it, the weekday within the week, the date before. */
export function dayLabel(ms: number, now: number, lang: Language, words: { today: string; yesterday: string }): { title: string; meta: string } {
  const days = Math.round((startOfDay(now) - startOfDay(ms)) / 86_400_000);
  const sameYear = new Date(ms).getFullYear() === new Date(now).getFullYear();
  const long = new Intl.DateTimeFormat(lang, { weekday: "long", day: "numeric", month: "long", year: sameYear ? undefined : "numeric" }).format(ms);
  if (days === 0) return { title: words.today, meta: long };
  if (days === 1) return { title: words.yesterday, meta: long };
  if (days > 1 && days < 7) {
    return { title: new Intl.DateTimeFormat(lang, { weekday: "long" }).format(ms), meta: new Intl.DateTimeFormat(lang, { day: "numeric", month: "long" }).format(ms) };
  }
  return {
    title: new Intl.DateTimeFormat(lang, { day: "numeric", month: "long", year: sameYear ? undefined : "numeric" }).format(ms),
    meta: new Intl.DateTimeFormat(lang, { weekday: "long" }).format(ms),
  };
}

/** "Today 14:32", "Yesterday 09:10" or "12 Nov 2026 09:10". */
export function formatWhen(ms: number, now: number, lang: Language, words: { today: string; yesterday: string }): string {
  const days = Math.round((startOfDay(now) - startOfDay(ms)) / 86_400_000);
  const time = formatTime(ms, lang);
  if (days === 0) return `${words.today} ${time}`;
  if (days === 1) return `${words.yesterday} ${time}`;
  // The year only when it is not this one, so the column fits "Nov 14 8:10 PM".
  const year = new Date(ms).getFullYear() === new Date(now).getFullYear() ? undefined : "numeric";
  return `${new Intl.DateTimeFormat(lang, { day: "numeric", month: "short", year }).format(ms)} ${time}`;
}

export function extensionOf(entry: Entry): string {
  const path = mediaPath(entry) ?? "";
  const dot = path.lastIndexOf(".");
  return dot > path.lastIndexOf("/") && dot >= 0 ? path.slice(dot + 1).toUpperCase() : entry.kind === "recording" ? "MP4" : "PNG";
}

export function fileName(path: string | null): string {
  if (!path) return "";
  return path.slice(path.lastIndexOf("/") + 1);
}

/** The file that holds the image the entry shows, as `Entry::media_path` in the core picks it: the
 * history's own copy when there is one, else the saved file. An entry has both after a Save
 * followed by a Copy of more drawing, and then the saved file is the older image, which may lack a
 * redaction made since; so uploads, copies and the format read from here. Only actions on the
 * saved file as a place (Show in Finder, the Trash) use `savedPath` itself. */
export function mediaPath(entry: Pick<Entry, "file" | "savedPath">): string | null {
  return entry.file ?? entry.savedPath;
}

// ------------------------------------------------------------------------------ grid layout

export const GRID = {
  padX: 16,
  gap: 8,
  headH: 44,
  resultsH: 44,
  /** The card's chrome around the 16:10 media: 4 padding, 4 gap, 34 meta, 4 padding. */
  chromeH: 46,
  /** The quoted line under a search result: 4 gap, 4 padding and two 16 px lines. */
  snippetH: 40,
  /** An open series' frame band under its row: 4 for the notch, 10 + 28 + 10 + 55 + 17 + 12 for
   * the band (padding, head, gap, tile, time, padding) and the grid's gap. */
  bandH: 144,
  /** A frame tile of the band and the gap between two. */
  tileW: 88,
  tileGap: 8,
  /** The room a series card gives its deck above the thumbnail. */
  deckH: 10,
  bottom: 28,
} as const;

export type Row =
  | { kind: "head"; key: string; y: number; h: number; dayMs: number; count: number; series: number }
  | { kind: "results"; key: string; y: number; h: number }
  | { kind: "cards"; key: string; y: number; h: number; items: HistoryItem[]; start: number }
  /** The frames of the open series, under the row of its card; `col` is the card's column. */
  | { kind: "band"; key: string; y: number; h: number; item: HistoryItem; col: number };

export type GridLayout = {
  rows: Row[];
  height: number;
  cols: number;
  cardH: number;
  /** The card's media box, which the thumbnails are placed in. */
  media: { w: number; h: number };
  /** A card's width, which places the notch of a series' band under its card. */
  cardW: number;
  /** For each entry in view order: its row index and column. */
  where: Map<EntryId, { row: number; col: number; index: number }>;
  order: EntryId[];
};

export function gridLayout(
  items: readonly HistoryItem[],
  o: { width: number; zoom: number; grouped: boolean; results: boolean; extraBottom: number; open?: EntryId | null },
): GridLayout {
  const avail = Math.max(1, o.width - 2 * GRID.padX);
  const cols = Math.max(1, Math.floor((avail + GRID.gap) / (o.zoom + GRID.gap)));
  const cardW = (avail - GRID.gap * (cols - 1)) / cols;
  const mediaH = ((cardW - 8) * 10) / 16;
  const cardH = Math.round(mediaH + GRID.chromeH + (o.results ? GRID.snippetH : 0));
  const rows: Row[] = [];
  const where = new Map<EntryId, { row: number; col: number; index: number }>();
  const order: EntryId[] = [];
  let y = 0;
  if (o.results) {
    rows.push({ kind: "results", key: "results", y, h: GRID.resultsH });
    y += GRID.resultsH;
  } else if (!o.grouped) {
    y += 12;
  }
  const pushCards = (group: HistoryItem[], key: string) => {
    for (let i = 0; i < group.length; i += cols) {
      const slice = group.slice(i, i + cols);
      const row = rows.length;
      slice.forEach((item, col) => {
        where.set(item.entry.id, { row, col, index: order.length });
        order.push(item.entry.id);
      });
      rows.push({ kind: "cards", key: `${key}:${i}`, y, h: cardH + GRID.gap, items: slice, start: order.length - slice.length });
      y += cardH + GRID.gap;
      const open = o.open ? slice.findIndex((item) => item.entry.id === o.open && item.entry.kind === "series") : -1;
      if (open >= 0) {
        rows.push({ kind: "band", key: `band:${o.open}`, y, h: GRID.bandH, item: slice[open], col: open });
        y += GRID.bandH;
      }
    }
  };
  if (o.grouped) {
    let i = 0;
    while (i < items.length) {
      const key = dayKey(items[i].entry.createdAtMs);
      let j = i + 1;
      while (j < items.length && dayKey(items[j].entry.createdAtMs) === key) j++;
      const series = items.slice(i, j).filter((item) => item.entry.kind === "series").length;
      rows.push({ kind: "head", key: `head:${key}`, y, h: GRID.headH, dayMs: items[i].entry.createdAtMs, count: j - i, series });
      y += GRID.headH;
      pushCards(items.slice(i, j) as HistoryItem[], key);
      i = j;
    }
  } else {
    pushCards(items as HistoryItem[], "all");
  }
  return { rows, height: y + GRID.bottom + o.extraBottom, cols, cardH, media: { w: cardW - 8, h: mediaH }, cardW, where, order };
}

/** The first row whose bottom is below `y` (rows are sorted by `y`). */
export function rowAt(rows: readonly Row[], y: number): number {
  let lo = 0;
  let hi = rows.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (rows[mid].y + rows[mid].h <= y) lo = mid + 1;
    else hi = mid;
  }
  return Math.max(0, lo);
}

/** The entry an arrow key moves to: left and right walk the order, up and down keep the column
 * across rows and day headers. */
export function neighbour(layout: GridLayout, id: EntryId, key: "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown"): EntryId | null {
  const at = layout.where.get(id);
  if (!at) return layout.order[0] ?? null;
  if (key === "ArrowLeft") return layout.order[Math.max(0, at.index - 1)] ?? null;
  if (key === "ArrowRight") return layout.order[Math.min(layout.order.length - 1, at.index + 1)] ?? null;
  const step = key === "ArrowDown" ? 1 : -1;
  for (let r = at.row + step; r >= 0 && r < layout.rows.length; r += step) {
    const row = layout.rows[r];
    if (row.kind === "cards") return row.items[Math.min(at.col, row.items.length - 1)].entry.id;
  }
  return id;
}

// -------------------------------------------------------------------------------- selection

/** Selection as in Finder: a click selects one item and sets the anchor, ⌘-click toggles one and
 * moves the anchor, and ⇧-click (or ⇧ with an arrow) selects the run from the anchor in view
 * order. A new ⇧ run replaces the previous one but keeps the items added with ⌘ (`base`). */
export type Selection = { ids: EntryId[]; anchor: EntryId | null; base: EntryId[] };

export const NO_SELECTION: Selection = { ids: [], anchor: null, base: [] };

export function selectOne(id: EntryId | null): Selection {
  return id ? { ids: [id], anchor: id, base: [id] } : NO_SELECTION;
}

export function selectToggle(s: Selection, id: EntryId): Selection {
  const ids = s.ids.includes(id) ? s.ids.filter((x) => x !== id) : [...s.ids, id];
  return { ids, anchor: id, base: ids };
}

export function selectRange(s: Selection, id: EntryId, order: readonly EntryId[]): Selection {
  const a = s.anchor ? order.indexOf(s.anchor) : -1;
  const b = order.indexOf(id);
  if (a < 0 || b < 0) return selectOne(id);
  const run = new Set([...s.base, ...order.slice(Math.min(a, b), Math.max(a, b) + 1)]);
  const inView = order.filter((x) => run.has(x));
  const outside = s.base.filter((x) => !order.includes(x));
  return { ids: [...inView, ...outside], anchor: s.anchor, base: s.base };
}

export function selectAll(order: readonly EntryId[]): Selection {
  return { ids: [...order], anchor: order[0] ?? null, base: [] };
}

/** Drops ids that left the view (deleted, filtered out). */
export function keepOnly(s: Selection, present: (id: EntryId) => boolean): Selection {
  return {
    ids: s.ids.filter(present),
    anchor: s.anchor && present(s.anchor) ? s.anchor : null,
    base: s.base.filter(present),
  };
}

// ------------------------------------------------------------------------------------- zoom

export type Placement = { x: number; y: number; k: number };

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

/** Where an image of `image` pixels sits in a `box`: covering it from the top (a card),
 * contained and centred (the inspector), or zoomed onto a match so the word stays legible at
 * card size, at 3.2 × the match's width and never above 100 %. */
export function place(box: { w: number; h: number }, image: { w: number; h: number }, mode: "cover" | "contain", focus?: TextBox | null, scale = 1): Placement {
  if (!box.w || !box.h || !image.w || !image.h) return { x: 0, y: 0, k: 1 };
  if (mode === "contain") {
    const k = Math.min(box.w / image.w, box.h / image.h);
    return { k, x: (box.w - image.w * k) / 2, y: (box.h - image.h * k) / 2 };
  }
  let k = Math.max(box.w / image.w, box.h / image.h);
  let x = (box.w - image.w * k) / 2;
  let y = 0;
  if (focus) {
    const viewW = Math.max(focus.width * 3.2, 380 * Math.max(1, scale));
    k = clamp(box.w / viewW, k, 1);
    x = clamp(box.w / 2 - (focus.x + focus.width / 2) * k, box.w - image.w * k, 0);
    y = clamp(box.h / 2 - (focus.y + focus.height / 2) * k, box.h - image.h * k, 0);
  }
  return { x, y, k };
}
