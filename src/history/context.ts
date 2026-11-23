// What the history window's components share: the state they read and the actions they call.
// App.vue provides it once; the cards, rows and the inspector inject it instead of passing a
// dozen events up through the virtual grid.

import type { InjectionKey, Ref } from "vue";

import type { EntryId } from "../lib/api/history";
import type { Language } from "../lib/i18n";
import type { Selection } from "./model";

export type UploadState =
  | { state: "uploading"; uploadId: string | null; sent: number; total: number; host: string }
  | { state: "failed"; reason: string };

export type Actions = {
  /** A click or a key on an item: one, ⌘ toggle or ⇧ range. */
  pick(id: EntryId, mode: "one" | "toggle" | "range"): void;
  open(id: EntryId): void;
  copy(id: EntryId): void;
  copyLink(id: EntryId): void;
  /** Copies the path of the entry's file, quoted for a shell (⌥⌘C). */
  copyPath(id: EntryId): void;
  /** Opens the Send menu at a point: Copy path, the targets of Settings › Output, Edit targets…. */
  sendMenu(id: EntryId, at: { x: number; y: number }): void;
  upload(ids: EntryId[]): void;
  cancelUpload(id: EntryId): void;
  menu(id: EntryId, at: { x: number; y: number }): void;
  /** The pointer left a card with the button down: the system drag of the file starts. */
  dragOut(id: EntryId): void;
  /** Asks for the text of a text copy, which pages leave out, for its card. */
  wantPreview(id: EntryId): void;
  copyText(id: EntryId): void;
  saveAs(id: EntryId): void;
  pinToScreen(id: EntryId): void;
  reveal(id: EntryId): void;
  /** Opens the delete sheet for these entries. */
  remove(ids: EntryId[]): void;
  openLink(id: EntryId): void;
  deleteHosted(id: EntryId): void;
  setPinned(id: EntryId, pinned: boolean): void;
  rename(id: EntryId, name: string): void;
  /** Opens a recording in the trim window, set to export an MP4 (⌘T) or a GIF (⇧⌘E). */
  trim(id: EntryId, format: "mp4" | "gif"): void;
};

export type HistoryContext = {
  lang: Ref<Language>;
  /** The search as the store ran it, for marking matches. */
  query: Ref<string>;
  selection: Ref<Selection>;
  focusId: Ref<EntryId | null>;
  uploads: Ref<ReadonlyMap<EntryId, UploadState>>;
  previews: Ref<ReadonlyMap<EntryId, string>>;
  now: Ref<number>;
  act: Actions;
  /** A host's product name ("ImgBB") for its provider id, or the id while the names load. */
  hostName(provider: string): string;
};

export const HISTORY: InjectionKey<HistoryContext> = Symbol("history");
