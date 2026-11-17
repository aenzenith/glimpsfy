// The capture history: list, search, delete, pin, rename, clear and the disk usage. Mirrors
// src-tauri/src/commands/history.rs and crates/history/src/lib.rs.
//
// An entry owns `file` and `thumbnail` inside the history's folder and loses them with the entry.
// `savedPath` is the user's own file: retention and clearing never touch it, and only a delete
// with `trashSavedFile` moves it to the Trash. Images never cross IPC: a page loads
// `thumbUrl(entry)` and `fileUrl(id)` from the `glimpsfy-history` scheme.

import { t } from "../i18n";
import { call, on, type EpochMs } from "./core";
import type { DeletionHandle, ProviderId, UploadResult } from "./upload";

export type EntryId = string;

export type EntryKind = "screenshot" | "recording" | "textCopy";

export type PixelSize = { width: number; height: number };

export type EntrySource = {
  appName: string | null;
  bundleId: string | null;
  windowTitle: string | null;
};

export type EntryDisplay = { id: number; name: string | null };

/** A box in pixels of the image the text was read from, origin at the top left. */
export type TextBox = { x: number; y: number; width: number; height: number };

export type TextLine = { text: string; bounds: TextBox; words: { text: string; bounds: TextBox }[] };

export type RecognizedText = { imageSize: PixelSize; lines: TextLine[] };

export type TextState = "none" | "pending" | "done" | "failed";

export type HostedCopy = {
  provider: ProviderId;
  url: string;
  deletion: DeletionHandle | null;
  uploadedAtMs: EpochMs;
  expiresAtMs: EpochMs | null;
};

export type Entry = {
  id: EntryId;
  kind: EntryKind;
  name: string;
  createdAtMs: EpochMs;
  /** The history's own copy, for a capture that only went to the clipboard. */
  file: string | null;
  thumbnail: string | null;
  /** The annotation document saved beside the image, so the drawings stay editable. */
  annotations: string | null;
  savedPath: string | null;
  size: PixelSize | null;
  scale: number | null;
  durationMs: number | null;
  fileBytes: number | null;
  source: EntrySource;
  display: EntryDisplay | null;
  /** The recognised or copied text; pages and events leave it out, `history_get` fills it in. */
  text: string | null;
  textLines: RecognizedText | null;
  textLineCount: number;
  textState: TextState;
  hosted: HostedCopy | null;
  pinned: boolean;
};

/** The name an entry shows. The core stores no made-up words ("Text from Safari", "Capture"), so
 * an entry without a name of its own is named here, in the language the window speaks. */
export function entryName(entry: Pick<Entry, "name" | "kind" | "source">): string {
  const own = entry.name.trim();
  if (own) return own;
  const app = entry.source.appName?.trim();
  if (entry.kind === "textCopy") return app ? t("history.name.textFrom", { app }) : t("history.name.text");
  return entry.kind === "recording" ? t("history.name.recording") : t("history.name.capture");
}

export type SortOrder = "newest" | "oldest" | "name" | "size" | "bestMatch";

export type HistoryQuery = {
  /** An empty list lets every kind through. */
  kinds: EntryKind[];
  uploaded: boolean | null;
  pinned: boolean | null;
  app: string | null;
  sort: SortOrder;
  offset: number;
  limit: number;
};

export const DEFAULT_HISTORY_QUERY: HistoryQuery = {
  kinds: [],
  uploaded: null,
  pinned: null,
  app: null,
  sort: "newest",
  offset: 0,
  limit: 100,
};

/** A span of a snippet in UTF-16 code units, so `snippet.slice(start, end)` is the match. */
export type TextRange = { start: number; end: number };

export type TextMatch = {
  /** The longest matching line, cut around its first match, or the name when only it matched. */
  snippet: string;
  highlights: TextRange[];
  inName: boolean;
  /** How many matches the recognised text holds. */
  count: number;
  /** One box per match in reading order, in pixels of `imageSize`. */
  boxes: TextBox[];
  /** The box in the quoted line, which a card zooms onto. */
  best: number | null;
  imageSize: PixelSize | null;
};

export type HistoryItem = {
  entry: Entry;
  /** A search fills it in, and a plain list leaves it `null`. */
  textMatch: TextMatch | null;
};

export type AppCount = { name: string; count: number };

export type Facets = {
  all: number;
  screenshots: number;
  recordings: number;
  textCopies: number;
  uploaded: number;
  pinned: number;
  apps: AppCount[];
};

export type HistoryPage = {
  items: HistoryItem[];
  total: number;
  offset: number;
  /** The sidebar's counts, on the first page only. */
  facets: Facets | null;
};

export type DeleteOptions = {
  /** Moves the file the user saved to the Trash as well. */
  trashSavedFile: boolean;
  /** Deletes the copy on the upload host first; if that fails, the entry stays with its link. */
  deleteHostedCopy: boolean;
};

export type StepOutcome =
  | { state: "untouched" }
  | { state: "removed" }
  | { state: "missing" }
  | { state: "failed"; reason: string }
  | { state: "skipped" }
  /** The host deletes on its own page, which is open in the browser; the entry keeps its link
   * until the user confirms with `history_forget_link`. */
  | { state: "pageOpened"; url: string };

export type DeleteOutcome = {
  entryId: EntryId;
  /** The entry left the history. */
  removed: boolean;
  ownFiles: StepOutcome;
  savedFile: StepOutcome;
  hostedCopy: StepOutcome;
  freedBytes: number;
};

export type DeleteReport = { outcomes: DeleteOutcome[]; freedBytes: number };

/** The values match Settings › Output › Keep captures. */
export type Retention = "off" | "days7" | "days30" | "days90" | "forever";

export type HistoryUsage = {
  entries: number;
  pinned: number;
  /** The bytes of the captures, wherever they are stored. */
  captureBytes: number;
  /** The bytes of the history's own copies and thumbnails. */
  ownBytes: number;
  /** What clearing would free, and how many entries it would remove. */
  clearableBytes: number;
  clearableEntries: number;
  oldestAtMs: EpochMs | null;
};

export type HistoryChanged =
  | { kind: "added"; entry: Entry }
  | { kind: "updated"; entry: Entry }
  | { kind: "removed"; ids: EntryId[] };

export const HISTORY_CHANGED = "history://changed";

const SCHEME = "glimpsfy-history";
const onWindows = typeof navigator !== "undefined" && /Windows/.test(navigator.userAgent);

function mediaUrl(kind: "thumb" | "file", id: EntryId): string {
  return onWindows ? `http://${SCHEME}.localhost/${kind}/${id}` : `${SCHEME}://${kind}/${id}`;
}

/** The entry's thumbnail, or `null` while it is being made. The core serves thumbnails as
 * immutable, and a revised entry gets a thumbnail file of another name, so the name rides along
 * and a new thumbnail never shows the old one from the cache. */
export function thumbUrl(entry: Pick<Entry, "id" | "thumbnail">): string | null {
  if (!entry.thumbnail) return null;
  const version = entry.thumbnail.split(/[\\/]/).pop() ?? "";
  return `${mediaUrl("thumb", entry.id)}?v=${encodeURIComponent(version)}`;
}

/** The capture itself: an image, or a recording the inspector plays. */
export function fileUrl(id: EntryId): string {
  return mediaUrl("file", id);
}

export function history_list(query: HistoryQuery): Promise<HistoryPage> {
  return call("history_list", { query });
}

export function history_get(entryId: EntryId): Promise<Entry> {
  return call("history_get", { entryId });
}

export function history_search(text: string, query: HistoryQuery): Promise<HistoryPage> {
  return call("history_search", { text, query });
}

export function history_delete(entryIds: EntryId[], options: DeleteOptions): Promise<DeleteReport> {
  return call("history_delete", { entryIds, options });
}

export function history_set_pinned(entryId: EntryId, pinned: boolean): Promise<Entry> {
  return call("history_set_pinned", { entryId, pinned });
}

export function history_rename(entryId: EntryId, name: string): Promise<Entry> {
  return call("history_rename", { entryId, name });
}

/** Removes every entry that is not pinned; files the user saved and hosted copies stay. */
export function history_clear(): Promise<DeleteReport> {
  return call("history_clear");
}

export function history_usage(): Promise<HistoryUsage> {
  return call("history_usage");
}

/** Records the link of an upload the history started with `upload_file`. */
export function history_record_upload(entryId: EntryId, result: UploadResult): Promise<Entry> {
  return call("history_record_upload", { entryId, result });
}

/** Puts a screenshot's image on the clipboard. */
export function history_copy(entryId: EntryId): Promise<void> {
  return call("history_copy", { entryId });
}

/** Opens the entry's image in the editor (through `editor_open`). */
export function history_open_in_editor(entryId: EntryId): Promise<void> {
  return call("history_open_in_editor", { entryId });
}

/** Saves a copy where the user picks in the save panel and answers the path; a cancelled panel
 * fails with `cancelled`. */
export function history_save_as(entryId: EntryId): Promise<string> {
  return call("history_save_as", { entryId });
}

export type HostedDeletion = { outcome: "deleted"; entry: Entry } | { outcome: "pageOpened"; url: string };

/** Deletes the hosted copy and forgets the link; the entry stays. A refusal keeps the link and
 * fails with `provider`; a host that deletes on its own page has it opened and answers
 * `pageOpened`, and the link stays until `history_forget_link`. */
export function history_delete_hosted(entryId: EntryId): Promise<HostedDeletion> {
  return call("history_delete_hosted", { entryId });
}

/** Forgets an entry's link without asking the host, after the user deleted it on the host's page. */
export function history_forget_link(entryId: EntryId): Promise<Entry> {
  return call("history_forget_link", { entryId });
}

export type ExportReport = { folder: string; exported: number; skipped: EntryId[] };

/** Copies the captures into a folder the user picks, never replacing a file that exists. */
export function history_export(entryIds: EntryId[]): Promise<ExportReport> {
  return call("history_export", { entryIds });
}

export function onHistoryChanged(handler: (change: HistoryChanged) => void) {
  return on<HistoryChanged>(HISTORY_CHANGED, handler);
}
