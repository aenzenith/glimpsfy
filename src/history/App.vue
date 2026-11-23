<script setup lang="ts">
// The history window (DESIGN.md › History window; design/round-2/windows ab1–ab6, ab11, ab12).
// It owns the state: the filter, the search, the sort, the view, the loaded entries, the
// selection and the dialogs; the grid, the list and the inspector read it through `HISTORY`.
// Entries arrive in pages of 500 until all are loaded, `history://changed` keeps them current,
// and images come from the `glimpsfy-history` scheme, never over IPC.
import { computed, nextTick, onBeforeUnmount, onMounted, provide, ref, shallowRef, watch } from "vue";

import ConfirmDialog from "../components/ConfirmDialog.vue";
import GIcon from "../components/GIcon.vue";
import { capture_start } from "../lib/api/capture";
import { ApiError, on } from "../lib/api/core";
import {
  history_clear,
  history_copy,
  history_delete,
  history_delete_hosted,
  history_export,
  history_forget_link,
  history_get,
  history_list,
  history_open_in_editor,
  history_rename,
  history_save_as,
  history_search,
  history_set_pinned,
  history_usage,
  onHistoryChanged,
  type DeleteOptions,
  type Entry,
  type EntryId,
  type Facets,
  type HistoryChanged,
  type HistoryItem,
  type HistoryPage,
  type HistoryUsage,
  type SortOrder,
} from "../lib/api/history";
import { output_reveal } from "../lib/api/output";
import { record_open_trim } from "../lib/api/record";
import { send_copy_path, send_list_targets, send_to_app, type SendTargets } from "../lib/api/send";
import { settings_get, settings_set } from "../lib/api/settings";
import { onUploadDone, onUploadFailed, onUploadProgress, upload_cancel, upload_file, upload_providers } from "../lib/api/upload";
import { app_copy_text, app_open_url, pin_open, popover_drag_out, window_open, window_open_settings } from "../lib/api/windows";
import { SETTINGS_CHANGED } from "../lib/events";
import { language, t } from "../lib/i18n";
import { capsText } from "../lib/keys";
import { can } from "../lib/platform";
import type { Settings } from "../lib/settings";
import AppIcon from "./AppIcon.vue";
import { HISTORY, type Actions, type UploadState } from "./context";
import DeleteSheet from "./DeleteSheet.vue";
import EmptyState from "./EmptyState.vue";
import Feed from "./Feed.vue";
import "./history.css";
import Inspector from "./Inspector.vue";
import ListView from "./ListView.vue";
import Menu, { type MenuItem } from "./Menu.vue";
import { sendMenuItems } from "./send";
import {
  formatBytes,
  keepOnly,
  LIBRARY_FILTERS,
  mediaPath,
  neighbour,
  NO_SELECTION,
  passes,
  queryFor,
  selectAll,
  selectOne,
  selectRange,
  selectToggle,
  STATUS_FILTERS,
  type Filter,
  type Selection,
} from "./model";

const FIRST_PAGE = 500;
const PAGE = 2000;

// ------------------------------------------------------------------------------- the state
const remembered = (key: string, fallback: string) => {
  try {
    return localStorage.getItem(`glimpsfy.history.${key}`) ?? fallback;
  } catch {
    return fallback;
  }
};
const remember = (key: string, value: string) => {
  try {
    localStorage.setItem(`glimpsfy.history.${key}`, value);
  } catch {
    // A page without storage forgets the view, which is harmless.
  }
};

const filter = ref<Filter>("all");
const view = ref<"grid" | "list">(remembered("view", "grid") === "list" ? "list" : "grid");
const sort = ref<SortOrder>("newest");
const searchSort = ref<SortOrder>("bestMatch");
const zoom = ref(Math.min(280, Math.max(140, Number(remembered("zoom", "200")) || 200)));
const panelOpen = ref(remembered("panel", "1") === "1");
const searchText = ref("");
const query = ref("");

const items = shallowRef<HistoryItem[]>([]);
const total = ref(0);
const facets = ref<Facets | null>(null);
const status = ref<"loading" | "ready" | "error">("loading");
const errorText = ref("");
const selection = ref<Selection>(NO_SELECTION);
const focusId = ref<EntryId | null>(null);
const uploads = shallowRef<Map<EntryId, UploadState>>(new Map());
const previews = shallowRef<Map<EntryId, string>>(new Map());
const now = ref(Date.now());
const settings = ref<Settings | null>(null);
const usage = ref<HistoryUsage | null>(null);
const full = ref<Entry | null>(null);
const matchIndex = ref(0);
const sheet = ref<{ ids: EntryId[] } | null>(null);
const busy = ref(false);
const confirming = ref<
  | { kind: "clear" }
  | { kind: "deleteHosted"; id: EntryId }
  // The host deletes on its own page, which is open now; the entries keep their link until the
  // user says the upload is gone there.
  | { kind: "forgetLink"; id: EntryId; host: string }
  | { kind: "finishDelete"; ids: EntryId[]; host: string; options: DeleteOptions }
  | null
>(null);
const menu = ref<{ items: MenuItem[]; at: { x: number; y: number } } | null>(null);
const toasts = ref<{ id: number; title: string; body?: string; tone: "ok" | "warn"; link?: boolean }[]>([]);
const width = ref(window.innerWidth);

const feed = ref<InstanceType<typeof Feed> | null>(null);
const list = ref<InstanceType<typeof ListView> | null>(null);
const search = ref<HTMLInputElement | null>(null);

// --------------------------------------------------------------------------------- derived
const narrow = computed(() => width.value <= 680);
const overlay = computed(() => width.value < 900);
const searching = computed(() => query.value !== "");
const effectiveSort = computed<SortOrder>(() => (searching.value ? searchSort.value : sort.value));
const grouped = computed(() => !searching.value && (sort.value === "newest" || sort.value === "oldest"));
const byId = computed(() => new Map(items.value.map((item) => [item.entry.id, item])));
const selected = computed(() => selection.value.ids.map((id) => byId.value.get(id)).filter((i): i is HistoryItem => !!i));
const multi = computed(() => selection.value.ids.length > 1);
const single = computed(() => (selection.value.ids.length === 1 ? (byId.value.get(selection.value.ids[0]) ?? null) : null));
const showPanel = computed(() => panelOpen.value && !!single.value);
const retention = computed(() => settings.value?.output.historyRetention ?? null);
const historyOff = computed(() => retention.value === "off");
const mainWidth = computed(() => width.value - (narrow.value ? 0 : 220) - (showPanel.value && !overlay.value ? 340 : 0));
const compact = computed(() => !narrow.value && mainWidth.value < 860);
const loaded = computed(() => status.value === "ready");

const filterLabel = (f: Filter): string => {
  switch (f) {
    case "all":
      return t("history.filter.all");
    case "screenshots":
      return t("history.filter.screenshots");
    case "recordings":
      return t("history.filter.recordings");
    case "textCopies":
      return t("history.filter.textCopies");
    case "uploaded":
      return t("history.filter.uploaded");
    case "pinned":
      return t("history.filter.pinned");
    default:
      return f.slice(4);
  }
};
const filterIcon: Record<string, string> = { all: "grid", screenshots: "image", recordings: "film", textCopies: "ocr", uploaded: "cloud", pinned: "pin" };
const countOf = (f: Filter): number | null => {
  const c = facets.value;
  if (!c) return null;
  switch (f) {
    case "all":
      return c.all;
    case "screenshots":
      return c.screenshots;
    case "recordings":
      return c.recordings;
    case "textCopies":
      return c.textCopies;
    case "uploaded":
      return c.uploaded;
    case "pinned":
      return c.pinned;
    default:
      return c.apps.find((a) => a.name === f.slice(4))?.count ?? 0;
  }
};
const apps = computed(() => (facets.value?.apps ?? []).slice(0, 10));
const narrowFilters = computed<Filter[]>(() => [...LIBRARY_FILTERS, ...STATUS_FILTERS, ...apps.value.map((a) => `app:${a.name}` as Filter)]);

const sortOptions = computed(() => {
  const base: { value: SortOrder; label: string }[] = [
    { value: "newest", label: t("history.sort.newest") },
    { value: "oldest", label: t("history.sort.oldest") },
    { value: "size", label: t("history.sort.size") },
    { value: "name", label: t("history.sort.name") },
  ];
  return searching.value ? [{ value: "bestMatch" as SortOrder, label: t("history.sort.bestMatch") }, ...base] : base;
});
function setSort(value: SortOrder): void {
  if (searching.value) searchSort.value = value;
  else sort.value = value;
}

const retentionRule = computed(() => {
  switch (retention.value) {
    case "off":
      return t("history.foot.off");
    case "days7":
      return t("history.foot.removed", { days: 7 });
    case "days30":
      return t("history.foot.removed", { days: 30 });
    case "days90":
      return t("history.foot.removed", { days: 90 });
    case "forever":
      return t("history.foot.forever");
    default:
      return "";
  }
});
const footLine = computed(() => {
  const u = usage.value;
  if (!u) return "";
  const n = u.entries === 1 ? t("history.captures.one") : t("history.captures.other", { n: u.entries });
  return `${n} · ${formatBytes(u.captureBytes, language.value)}`;
});

// ------------------------------------------------------------------------------- toasts
let toastId = 0;
function toast(title: string, body?: string, tone: "ok" | "warn" = "ok", link = false): void {
  const id = ++toastId;
  toasts.value = [...toasts.value.slice(-2), { id, title, body, tone, link }];
  setTimeout(() => (toasts.value = toasts.value.filter((x) => x.id !== id)), tone === "warn" ? 6000 : 2600);
}
/** Reports a failed action: one that another area has not shipped yet, one the user cancelled
 * (nothing to say), or a real failure with its reason. */
function failed(error: unknown, what: string): void {
  if (error instanceof ApiError) {
    if (error.code === "cancelled") return;
    if (error.code === "not_implemented") return toast(t("history.toast.unavailable", { what }), undefined, "warn");
    if (error.code === "not_found") return toast(t("history.toast.missing"), error.message, "warn");
    return toast(t("history.toast.failed", { what }), error.message, "warn");
  }
  toast(t("history.toast.failed", { what }), String(error), "warn");
}

// --------------------------------------------------------------------------------- loading
let generation = 0;
async function fetchPage(offset: number, limit: number): Promise<HistoryPage> {
  const q = queryFor(filter.value, effectiveSort.value, offset, limit);
  return searching.value ? history_search(query.value, q) : history_list(q);
}

async function reload(keepScroll = false): Promise<void> {
  const gen = ++generation;
  try {
    const page = await fetchPage(0, FIRST_PAGE);
    if (gen !== generation) return;
    items.value = page.items;
    total.value = page.total;
    if (page.facets && !searching.value) facets.value = page.facets;
    status.value = "ready";
    afterLoad();
    if (!keepScroll) feed.value?.toTop();
    let offset = page.items.length;
    while (offset < page.total && gen === generation) {
      const next = await fetchPage(offset, PAGE);
      if (gen !== generation) return;
      if (!next.items.length) break;
      items.value = [...items.value, ...next.items];
      offset += next.items.length;
    }
  } catch (error) {
    if (gen !== generation) return;
    status.value = "error";
    errorText.value = error instanceof Error ? error.message : String(error);
  }
}

/** Keeps the selection on entries that are still shown; a search selects its best result. */
function afterLoad(): void {
  const present = (id: EntryId) => byId.value.has(id);
  selection.value = keepOnly(selection.value, present);
  if (searching.value && items.value.length && !selection.value.ids.length) {
    selection.value = selectOne(items.value[0].entry.id);
  }
  if (focusId.value && !present(focusId.value)) focusId.value = null;
  if (!focusId.value) focusId.value = selection.value.ids[0] ?? items.value[0]?.entry.id ?? null;
}

let facetsTimer = 0;
function refreshCounts(): void {
  clearTimeout(facetsTimer);
  facetsTimer = window.setTimeout(async () => {
    try {
      const [page, use] = await Promise.all([history_list(queryFor("all", "newest", 0, 1)), history_usage()]);
      if (page.facets) facets.value = page.facets;
      usage.value = use;
    } catch {
      // The next change tries again; the counts stay as they were.
    }
  }, 250);
}

let reloadTimer = 0;
function reloadSoon(): void {
  clearTimeout(reloadTimer);
  reloadTimer = window.setTimeout(() => void reload(true), 300);
}

watch([filter, effectiveSort], () => void reload());
let searchTimer = 0;
watch(searchText, (text) => {
  clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    const next = text.trim();
    if (next === query.value) return;
    if (!next) searchSort.value = "bestMatch";
    query.value = next;
    matchIndex.value = 0;
    void reload();
  }, 90);
});

function applyChange(change: HistoryChanged): void {
  refreshCounts();
  if (searching.value) return reloadSoon();
  if (change.kind === "removed") {
    const gone = new Set(change.ids);
    const before = items.value.length;
    items.value = items.value.filter((i) => !gone.has(i.entry.id));
    total.value = Math.max(0, total.value - (before - items.value.length));
    afterLoad();
    return;
  }
  const entry = change.entry;
  const at = items.value.findIndex((i) => i.entry.id === entry.id);
  if (change.kind === "updated") {
    if (full.value?.id === entry.id) void loadFull(entry.id);
    if (at < 0) {
      if (passes(entry, filter.value)) reloadSoon();
      return;
    }
    if (!passes(entry, filter.value)) {
      items.value = items.value.filter((i) => i.entry.id !== entry.id);
      total.value = Math.max(0, total.value - 1);
      afterLoad();
      return;
    }
    const next = items.value.slice();
    next[at] = { ...next[at], entry };
    items.value = next;
    if (sort.value === "name" || sort.value === "size") reloadSoon();
    return;
  }
  if (at >= 0 || !passes(entry, filter.value)) return;
  if (sort.value === "newest") {
    items.value = [{ entry, textMatch: null }, ...items.value];
    total.value += 1;
    afterLoad();
  } else {
    reloadSoon();
  }
}

async function loadFull(id: EntryId): Promise<void> {
  try {
    const entry = await history_get(id);
    if (single.value?.entry.id === id) full.value = entry;
  } catch {
    // The inspector shows the page's entry without its text.
  }
}
watch(
  () => single.value?.entry.id ?? null,
  (id) => {
    full.value = null;
    matchIndex.value = single.value?.textMatch?.best ?? 0;
    if (id) void loadFull(id);
  },
);

// -------------------------------------------------------------------------------- selection
function order(): EntryId[] {
  return items.value.map((i) => i.entry.id);
}
function pick(id: EntryId, mode: "one" | "toggle" | "range"): void {
  if (mode === "range") selection.value = selectRange(selection.value, id, order());
  else if (mode === "toggle") selection.value = selectToggle(selection.value, id);
  else selection.value = selectOne(id);
  focusId.value = id;
}
async function revealItem(id: EntryId): Promise<void> {
  if (view.value === "grid") await feed.value?.reveal(id);
  else await list.value?.reveal(id);
}
function clearSelection(): void {
  const keep = focusId.value ?? selection.value.anchor;
  selection.value = { ids: [], anchor: keep, base: [] };
}

// ---------------------------------------------------------------------------------- actions
const entryOf = (id: EntryId): Entry | null => (full.value?.id === id ? full.value : (byId.value.get(id)?.entry ?? null));

async function textOf(id: EntryId): Promise<string> {
  const known = full.value?.id === id ? full.value.text : null;
  return known ?? (await history_get(id)).text ?? "";
}

function setUpload(id: EntryId, state: UploadState | null): void {
  const next = new Map(uploads.value);
  if (state) next.set(id, state);
  else next.delete(id);
  uploads.value = next;
}
/** Starts an upload; the upload area answers at once, reports progress, and ends it with
 * `upload://done` (whose `entryId` makes the core record the link on the entry) or
 * `upload://failed`. Without a provider the upload area uses the one Settings › Upload names. */
async function uploadOne(id: EntryId, provider: string | null): Promise<void> {
  const entry = entryOf(id);
  const path = entry ? mediaPath(entry) : null;
  if (!entry || !path) return toast(t("history.toast.missing"), undefined, "warn");
  setUpload(id, { state: "uploading", uploadId: null, sent: 0, total: entry.fileBytes ?? 0, host: provider ?? "" });
  try {
    const started = await upload_file(path, provider, id);
    const now = uploads.value.get(id);
    if (now?.state === "uploading" && !now.uploadId) {
      setUpload(id, { state: "uploading", uploadId: started.uploadId, sent: now.sent, total: started.totalBytes, host: hostName(started.provider) });
    }
  } catch (error) {
    if (error instanceof ApiError && (error.code === "cancelled" || error.code === "not_implemented")) {
      setUpload(id, null);
      failed(error, t("history.action.upload"));
    } else {
      setUpload(id, { state: "failed", reason: error instanceof Error ? error.message : String(error) });
    }
  }
}

/** The targets of Settings › Output for the Send menus, read again when the settings change. */
const sendTargets = shallowRef<SendTargets | null>(null);

function loadTargets(): void {
  if (!can("sendToApp")) return;
  send_list_targets()
    .then((list) => (sendTargets.value = list))
    .catch(() => {});
}

function sendItems(id: EntryId): MenuItem[] {
  const list = sendTargets.value ?? { targets: [], last: null, canPaste: true };
  return sendMenuItems(list, {
    noPath: entryOf(id)?.kind === "textCopy",
    copyPath: () => act.copyPath(id),
    send: (bundleId) => send_to_app({ kind: "entry", entryId: id }, bundleId).catch((e) => failed(e, t("send.send"))),
    edit: () => void window_open_settings("output").catch((e) => failed(e, t("send.editTargets"))),
  });
}

/** Where the keyboard's Send menu opens: under the inspector's Send, or under the card. */
function sendPoint(id: EntryId): { x: number; y: number } {
  const button = document.querySelector<HTMLElement>(".hv-panel [data-send]");
  const card = document.querySelector<HTMLElement>(`[data-id="${CSS.escape(id)}"]`);
  const box = (button?.offsetParent ? button : card)?.getBoundingClientRect();
  return box ? { x: box.left, y: box.bottom + 4 } : { x: 80, y: 80 };
}

const act: Actions = {
  pick,
  open(id) {
    const entry = entryOf(id);
    if (!entry) return;
    if (entry.kind === "textCopy") return act.copyText(id);
    if (entry.kind === "recording") {
      selection.value = selectOne(id);
      panelOpen.value = true;
      return;
    }
    history_open_in_editor(id).catch((e) => failed(e, t("history.action.open")));
  },
  copy(id) {
    const entry = entryOf(id);
    if (entry?.kind === "textCopy") return act.copyText(id);
    history_copy(id)
      .then(() => toast(t("history.toast.copied"), t("history.toast.copiedImage")))
      .catch((e) => failed(e, t("history.action.copy")));
  },
  copyText(id) {
    textOf(id)
      .then(async (text) => {
        if (!text) return toast(t("history.text.none"), undefined, "warn");
        await app_copy_text(text);
        const n = text.split("\n").filter((l) => l.trim()).length;
        toast(n === 1 ? t("history.toast.copiedLine") : t("history.toast.copiedLines", { n }), text.split("\n")[0]);
      })
      .catch((e) => failed(e, t("history.action.copyText")));
  },
  copyPath(id) {
    if (entryOf(id)?.kind === "textCopy") return;
    send_copy_path({ kind: "entry", entryId: id })
      .then((sent) => toast(t("toast.short.pathCopied"), sent.temporary ? t("toast.pathTemporary") : (sent.path ?? undefined)))
      .catch((e) => failed(e, t("send.copyPath")));
  },
  sendMenu(id, at) {
    if (!selection.value.ids.includes(id)) pick(id, "one");
    loadTargets();
    menu.value = { items: sendItems(id), at };
  },
  copyLink(id) {
    const url = entryOf(id)?.hosted?.url;
    if (!url) return;
    app_copy_text(url)
      .then(() => toast(t("history.toast.linkCopied"), url, "ok", true))
      .catch((e) => failed(e, t("history.action.copyLink")));
  },
  upload(ids) {
    const provider = settings.value?.upload.defaultProvider ?? null;
    for (const id of ids) {
      const entry = entryOf(id);
      if (entry && entry.kind !== "textCopy" && uploads.value.get(id)?.state !== "uploading") void uploadOne(id, provider);
    }
  },
  cancelUpload(id) {
    const u = uploads.value.get(id);
    if (u?.state === "uploading" && u.uploadId) upload_cancel(u.uploadId).catch((e) => failed(e, t("history.upload.cancel")));
    else setUpload(id, null);
  },
  menu(id, at) {
    if (!selection.value.ids.includes(id)) pick(id, "one");
    loadTargets();
    menu.value = { items: menuItems(), at };
  },
  dragOut(id) {
    popover_drag_out(id).catch((e) => failed(e, t("history.action.drag")));
  },
  wantPreview(id) {
    if (previews.value.has(id)) return;
    const next = new Map(previews.value);
    next.set(id, "");
    previews.value = next;
    history_get(id)
      .then((entry) => {
        const done = new Map(previews.value);
        done.set(id, entry.text ?? "");
        previews.value = done;
      })
      .catch(() => {});
  },
  saveAs(id) {
    history_save_as(id)
      .then((path) => toast(t("history.toast.saved"), path))
      .catch((e) => failed(e, t("history.action.saveAs")));
  },
  pinToScreen(id) {
    pin_open({ kind: "history", entryId: id }).catch((e) => failed(e, t("history.action.pinToScreen")));
  },
  reveal(id) {
    const entry = entryOf(id);
    if (!entry) return;
    // A place, not the picture: the user's own file first, even when a later Copy made a newer
    // own copy (`mediaPath`), because that is the file they know and look for in Finder.
    const first = entry.savedPath ?? entry.file;
    if (!first) return toast(t("history.toast.missing"), undefined, "warn");
    output_reveal(first).catch((e) => {
      if (entry.savedPath && entry.file && e instanceof ApiError && e.code === "not_found") {
        output_reveal(entry.file).catch((again) => failed(again, t("history.action.reveal")));
      } else {
        failed(e, t("history.action.reveal"));
      }
    });
  },
  remove(ids) {
    const present = ids.filter((id) => byId.value.has(id));
    if (present.length) sheet.value = { ids: present };
  },
  openLink(id) {
    const url = entryOf(id)?.hosted?.url;
    if (url) app_open_url(url).catch((e) => failed(e, t("history.action.openLink")));
  },
  deleteHosted(id) {
    confirming.value = { kind: "deleteHosted", id };
  },
  setPinned(id, pinned) {
    history_set_pinned(id, pinned).catch((e) => failed(e, pinned ? t("history.action.pin") : t("history.action.unpin")));
  },
  rename(id, name) {
    history_rename(id, name).catch((e) => failed(e, t("history.action.rename")));
  },
  trim(id, format) {
    record_open_trim({ entryId: id }, format).catch((e) => failed(e, format === "gif" ? t("history.action.gif") : t("history.action.trim")));
  },
};

function copyLinks(ids: EntryId[]): void {
  const urls = ids.map((id) => entryOf(id)?.hosted?.url).filter((u): u is string => !!u);
  if (!urls.length) return;
  app_copy_text(urls.join("\n"))
    .then(() => toast(urls.length === 1 ? t("history.toast.linkCopied") : t("history.toast.linksCopied", { n: urls.length }), urls[0], "ok", true))
    .catch((e) => failed(e, t("history.action.copyLink")));
}
function exportSelected(ids: EntryId[]): void {
  history_export(ids)
    .then((report) => {
      const title = report.exported === 1 ? t("history.toast.exportedOne") : t("history.toast.exported", { n: report.exported });
      toast(title, report.skipped.length ? t("history.toast.exportSkipped", { n: report.skipped.length }) : report.folder, report.skipped.length ? "warn" : "ok");
    })
    .catch((e) => failed(e, t("history.action.export")));
}

function menuItems(): MenuItem[] {
  const ids = selection.value.ids;
  if (ids.length > 1) {
    const links = ids.filter((id) => entryOf(id)?.hosted).length;
    return [
      { label: links === 1 ? t("history.bulk.copyLink") : t("history.bulk.copyLinks", { n: links }), icon: "link", keys: "⇧⌘L", disabled: !links, run: () => copyLinks(ids) },
      { label: t("history.action.upload"), icon: "upload", keys: "⌘U", run: () => act.upload(ids) },
      { label: t("history.action.export"), icon: "save", keys: "⌘E", run: () => exportSelected(ids) },
      "separator",
      { label: t("history.action.deleteMany"), icon: "trash", keys: "⌘⌫", destructive: true, run: () => act.remove(ids) },
    ];
  }
  const id = ids[0];
  const entry = entryOf(id);
  if (!entry) return [];
  const own: MenuItem[] = [];
  if (entry.kind === "screenshot") own.push({ label: t("history.action.open"), icon: "external-link", keys: "⌘O", run: () => act.open(id) });
  if (entry.kind === "recording") {
    own.push({ label: t("history.action.trim"), icon: "trim", keys: "⌘T", run: () => act.trim(id, "mp4") });
    own.push({ label: t("history.action.gif"), icon: "gif", keys: "⇧⌘E", run: () => act.trim(id, "gif") });
  }
  if (entry.kind === "textCopy") own.push({ label: t("history.action.copyText"), icon: "copy", keys: "⇧⌘C", run: () => act.copyText(id) });
  else own.push({ label: t("history.action.copy"), icon: "copy", keys: "⌘C", run: () => act.copy(id) });
  if (entry.kind !== "textCopy") own.push({ label: t("send.copyPath"), icon: "path", keys: "⌥⌘C", run: () => act.copyPath(id) });
  if (can("sendToApp")) own.push({ label: t("send.send"), icon: "send", children: sendItems(id) });
  if (entry.hosted) own.push({ label: t("history.action.copyLink"), icon: "link", keys: "⇧⌘L", run: () => act.copyLink(id) });
  else if (entry.kind !== "textCopy") own.push({ label: t("history.action.upload"), icon: "upload", keys: "⌘U", run: () => act.upload([id]) });
  own.push("separator");
  own.push({ label: entry.pinned ? t("history.action.unpin") : t("history.action.pin"), icon: "pin", run: () => act.setPinned(id, !entry.pinned) });
  if (entry.kind === "screenshot") own.push({ label: t("history.action.pinToScreen"), icon: "pin", keys: "⌘P", run: () => act.pinToScreen(id) });
  if (entry.kind !== "textCopy") {
    own.push({ label: t("history.action.saveAs"), icon: "save-as", keys: "⇧⌘S", run: () => act.saveAs(id) });
    own.push({ label: t("history.action.reveal"), icon: "folder", keys: "⌥⌘R", run: () => act.reveal(id) });
  }
  own.push("separator");
  own.push({ label: t("history.action.deleteOne"), icon: "trash", keys: "⌘⌫", destructive: true, run: () => act.remove([id]) });
  return own;
}

// ------------------------------------------------------------------------------- the dialogs
const sheetEntries = computed(() => (sheet.value?.ids ?? []).map((id) => entryOf(id)).filter((e): e is Entry => !!e));

async function confirmDelete(options: DeleteOptions): Promise<void> {
  const ids = sheet.value?.ids ?? [];
  if (!ids.length) return;
  busy.value = true;
  const view = order();
  const after = view.slice(view.indexOf(ids[ids.length - 1]) + 1).find((id) => !ids.includes(id)) ?? view.find((id) => !ids.includes(id)) ?? null;
  try {
    const report = await history_delete(ids, options);
    sheet.value = null;
    const removed = report.outcomes.filter((o) => o.removed);
    const kept = report.outcomes.filter((o) => !o.removed);
    if (removed.length) {
      const title = removed.length === 1 ? t("history.toast.deletedOne") : t("history.toast.deleted", { n: removed.length });
      const trashed = removed.filter((o) => o.savedFile.state === "removed").length;
      const hosted = removed.filter((o) => o.hostedCopy.state === "removed").length;
      const body = [trashed ? t("history.toast.trashed", { n: trashed }) : "", hosted ? t("history.toast.hostedDeleted", { n: hosted }) : "", report.freedBytes ? t("history.toast.freed", { size: formatBytes(report.freedBytes, language.value) }) : ""]
        .filter(Boolean)
        .join(" ");
      toast(title, body || undefined);
    }
    const onPage = kept.filter((o) => o.hostedCopy.state === "pageOpened");
    const refused = kept.filter((o) => o.hostedCopy.state !== "pageOpened");
    if (refused.length) {
      const reason = refused.map((o) => (o.hostedCopy.state === "failed" ? o.hostedCopy.reason : o.savedFile.state === "failed" ? o.savedFile.reason : "")).find(Boolean) ?? "";
      toast(refused.length === 1 ? t("history.toast.keptOne") : t("history.toast.kept", { n: refused.length }), reason, "warn");
    }
    if (onPage.length) {
      const host = hostName(entryOf(onPage[0].entryId)?.hosted?.provider ?? "");
      confirming.value = { kind: "finishDelete", ids: onPage.map((o) => o.entryId), host, options: { ...options, deleteHostedCopy: false } };
    }
    if (after && !kept.length) {
      selection.value = selectOne(after);
      focusId.value = after;
      await nextTick();
      void revealItem(after);
    }
  } catch (error) {
    failed(error, t("history.action.delete"));
  } finally {
    busy.value = false;
  }
}

async function confirmDialog(): Promise<void> {
  const c = confirming.value;
  if (!c) return;
  busy.value = true;
  try {
    confirming.value = null;
    if (c.kind === "clear") {
      const report = await history_clear();
      const n = report.outcomes.filter((o) => o.removed).length;
      toast(n === 1 ? t("history.toast.clearedOne") : t("history.toast.cleared", { n }), t("history.toast.freed", { size: formatBytes(report.freedBytes, language.value) }));
    } else if (c.kind === "deleteHosted") {
      const done = await history_delete_hosted(c.id);
      if (done.outcome === "deleted") toast(t("history.toast.hostedDeletedOne"));
      else confirming.value = { kind: "forgetLink", id: c.id, host: hostName(entryOf(c.id)?.hosted?.provider ?? "") };
    } else if (c.kind === "forgetLink") {
      await history_forget_link(c.id);
      toast(t("history.toast.linkForgotten"));
    } else {
      const report = await history_delete(c.ids, c.options);
      const n = report.outcomes.filter((o) => o.removed).length;
      if (n) toast(n === 1 ? t("history.toast.deletedOne") : t("history.toast.deleted", { n }));
      if (n < c.ids.length) toast(c.ids.length - n === 1 ? t("history.toast.keptOne") : t("history.toast.kept", { n: c.ids.length - n }), undefined, "warn");
    }
  } catch (error) {
    confirming.value = null;
    failed(error, c.kind === "clear" ? t("history.action.clear") : c.kind === "finishDelete" ? t("history.action.delete") : t("history.action.deleteHosted"));
  } finally {
    busy.value = false;
  }
}
const confirmText = computed(() => {
  const c = confirming.value;
  if (!c) return { title: "", text: "", label: "" };
  if (c.kind === "clear") {
    const u = usage.value;
    return {
      title: t("history.clear.title"),
      text: t("history.clear.text", { n: u?.clearableEntries ?? 0, size: formatBytes(u?.clearableBytes ?? 0, language.value) }),
      label: t("history.clear.confirm"),
    };
  }
  if (c.kind === "forgetLink") {
    return { title: t("history.onPage.title", { host: c.host }), text: t("history.onPage.forgetText", { host: c.host }), label: t("history.onPage.forget") };
  }
  if (c.kind === "finishDelete") {
    const text = c.ids.length === 1 ? t("history.onPage.deleteTextOne", { host: c.host }) : t("history.onPage.deleteText", { host: c.host, n: c.ids.length });
    return { title: t("history.onPage.title", { host: c.host }), text, label: t("history.onPage.delete") };
  }
  const host = hostName(entryOf(c.id)?.hosted?.provider ?? "");
  return { title: t("history.deleteHosted.title", { host }), text: t("history.delete.hostHelp"), label: t("history.delete.label.host", { host }) };
});

async function turnOn(): Promise<void> {
  try {
    const current = await settings_get();
    await settings_set({ ...current, output: { ...current.output, historyRetention: "days30" } });
    settings.value = { ...current, output: { ...current.output, historyRetention: "days30" } };
  } catch (error) {
    failed(error, t("history.off.turnOn"));
  }
}

// ------------------------------------------------------------------------------ keyboard
function step(by: number): void {
  const n = single.value?.textMatch?.boxes.length ?? 0;
  if (n) matchIndex.value = (matchIndex.value + by + n) % n;
}

function move(key: "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown", extend: boolean): void {
  const from = focusId.value ?? selection.value.ids[0] ?? null;
  let to: EntryId | null = null;
  const ids = order();
  if (!from) to = ids[0] ?? null;
  else if (view.value === "grid" && feed.value) to = neighbour(feed.value.layout, from, key);
  else if (key === "ArrowUp" || key === "ArrowDown") to = ids[Math.min(ids.length - 1, Math.max(0, ids.indexOf(from) + (key === "ArrowDown" ? 1 : -1)))] ?? null;
  if (!to) return;
  pick(to, extend ? "range" : "one");
  void revealItem(to);
}

function key(event: KeyboardEvent): void {
  if (sheet.value || confirming.value || menu.value) return;
  const target = event.target as HTMLElement;
  const typing = target.matches("input:not([type=range]):not([type=checkbox]), textarea, select");
  const k = event.key.toLowerCase();
  const cmd = event.metaKey;
  const ids = selection.value.ids;
  const one = ids.length === 1 ? ids[0] : null;
  const run = (fn: () => void) => {
    event.preventDefault();
    fn();
  };

  if (cmd && !event.shiftKey && !event.altKey && k === "f") return run(() => search.value?.focus());
  if (cmd && !event.altKey && k === "1") return run(() => (view.value = "grid"));
  if (cmd && !event.altKey && k === "2") return run(() => (view.value = "list"));
  if (cmd && event.altKey && event.code === "KeyI") return run(() => (panelOpen.value = !panelOpen.value));
  if (cmd && k === "g" && single.value?.textMatch) return run(() => step(event.shiftKey ? -1 : 1));
  if (event.key === "Escape") {
    if (target === search.value && searchText.value) return run(() => (searchText.value = ""));
    if (target === search.value) return run(() => search.value?.blur());
    if (overlay.value && showPanel.value) return run(() => (panelOpen.value = false));
    if (multi.value) return run(clearSelection);
    if (searchText.value) return run(() => (searchText.value = ""));
    return;
  }
  if (typing) return;
  if (cmd && k === "a") return run(() => (selection.value = selectAll(order())));
  if (event.key.startsWith("Arrow") && !cmd && !event.altKey) return run(() => move(event.key as "ArrowLeft", event.shiftKey));
  // ⌘↩ opens the Send menu (macOS only); ⌥⌘C copies the path. ⌥ turns the key's character into
  // another one ("ç"), so the key is read by its code.
  if (cmd && event.key === "Enter" && !event.shiftKey && !event.altKey && one && can("sendToApp")) return run(() => act.sendMenu(one, sendPoint(one)));
  if (cmd && event.altKey && !event.shiftKey && event.code === "KeyC" && one && entryOf(one)?.kind !== "textCopy") return run(() => act.copyPath(one));
  if ((event.key === " " || event.key === "Enter") && target.dataset.id) return run(() => pick(target.dataset.id as EntryId, event.shiftKey ? "range" : cmd ? "toggle" : "one"));
  if (!ids.length) return;
  if (cmd && !event.shiftKey && k === "o" && one) return run(() => act.open(one));
  if (cmd && !event.shiftKey && !event.altKey && k === "c" && one) {
    if (window.getSelection()?.toString()) return;
    return run(() => act.copy(one));
  }
  if (cmd && event.shiftKey && k === "c" && one) return run(() => act.copyText(one));
  if (cmd && event.shiftKey && k === "l") return run(() => copyLinks(ids));
  if (cmd && !event.shiftKey && k === "u") return run(() => act.upload(ids));
  if (cmd && !event.shiftKey && !event.altKey && k === "p" && one) return run(() => act.pinToScreen(one));
  if (cmd && event.altKey && event.code === "KeyR" && one) return run(() => act.reveal(one));
  if (cmd && event.shiftKey && k === "s" && one) return run(() => act.saveAs(one));
  if (cmd && event.shiftKey && k === "e" && one && entryOf(one)?.kind === "recording") return run(() => act.trim(one, "gif"));
  if (cmd && !event.shiftKey && k === "e") return run(() => exportSelected(ids));
  if (cmd && !event.shiftKey && k === "t" && one && entryOf(one)?.kind === "recording") return run(() => act.trim(one, "mp4"));
  if (cmd && (event.key === "Backspace" || event.key === "Delete")) return run(() => act.remove(ids));
}

// ------------------------------------------------------------------------------- tooltips
const tip = ref<{ text: string; keys: string; x: number; y: number } | null>(null);
const tipEl = ref<HTMLElement | null>(null);
let tipTimer = 0;
let tipFor: HTMLElement | null = null;
function showTip(el: HTMLElement): void {
  const r = el.getBoundingClientRect();
  tip.value = { text: el.dataset.tip ?? "", keys: el.dataset.key ?? "", x: r.left + r.width / 2, y: r.top };
  void nextTick(() => {
    const t = tipEl.value;
    if (!t || !tip.value) return;
    const w = t.offsetWidth;
    const h = t.offsetHeight;
    let y = r.top - 8 - h;
    if (y < 8) y = r.bottom + 8;
    tip.value = { ...tip.value, x: Math.round(Math.min(Math.max(r.left + r.width / 2 - w / 2, 8), window.innerWidth - w - 8)), y: Math.round(y) };
  });
}
function hideTip(): void {
  clearTimeout(tipTimer);
  tip.value = null;
  tipFor = null;
}
function over(event: MouseEvent): void {
  const el = (event.target as HTMLElement).closest<HTMLElement>("[data-tip]");
  if (el === tipFor) return;
  hideTip();
  if (!el) return;
  tipFor = el;
  tipTimer = window.setTimeout(() => showTip(el), 450);
}
function focusIn(event: FocusEvent): void {
  const el = (event.target as HTMLElement).closest<HTMLElement>("[data-tip]");
  hideTip();
  if (el && el.matches(":focus-visible")) {
    tipFor = el;
    showTip(el);
  }
}

/** The entry an upload belongs to: the one that started it, or the only one still waiting for its
 * id when the first progress arrives before `upload_file` answered. */
function uploadEntry(uploadId: string): EntryId | null {
  const all = [...uploads.value].filter(([, u]) => u.state === "uploading");
  const known = all.find(([, u]) => u.state === "uploading" && u.uploadId === uploadId);
  if (known) return known[0];
  const waiting = all.filter(([, u]) => u.state === "uploading" && !u.uploadId);
  return waiting.length === 1 ? waiting[0][0] : null;
}

// ------------------------------------------------------------------------------- lifecycle
const stops: (() => void)[] = [];
function resized(): void {
  width.value = window.innerWidth;
}
onMounted(async () => {
  loadTargets();
  window.addEventListener("resize", resized);
  document.addEventListener("keydown", key);
  document.addEventListener("mouseover", over);
  document.addEventListener("focusin", focusIn);
  document.addEventListener("focusout", hideTip);
  document.addEventListener("scroll", hideTip, true);
  document.addEventListener("mousedown", hideTip);
  const clock = window.setInterval(() => (now.value = Date.now()), 60_000);
  stops.push(() => clearInterval(clock));
  const listen = async (p: Promise<() => void>) => {
    try {
      stops.push(await p);
    } catch {
      // Outside Tauri (a test page) there are no events.
    }
  };
  await Promise.all([
    listen(onHistoryChanged(applyChange)),
    listen(
      on<Settings>(SETTINGS_CHANGED, (s) => {
        settings.value = s;
        loadTargets();
      }),
    ),
    listen(
      onUploadProgress((p) => {
        const id = uploadEntry(p.uploadId);
        if (id) setUpload(id, { state: "uploading", uploadId: p.uploadId, sent: p.sentBytes, total: p.totalBytes, host: hostName(p.provider) });
      }),
    ),
    listen(
      onUploadDone((done) => {
        const id = done.entryId ?? uploadEntry(done.result.uploadId);
        if (!id || !uploads.value.has(id)) return;
        setUpload(id, null);
        toast(done.copied ? t("history.toast.uploadedCopied") : t("history.toast.uploaded"), done.result.url, "ok", true);
      }),
    ),
    listen(
      onUploadFailed((f) => {
        const id = uploadEntry(f.uploadId);
        if (!id) return;
        if (f.kind === "cancelled") setUpload(id, null);
        else setUpload(id, { state: "failed", reason: f.error.message });
      }),
    ),
  ]);
  settings_get()
    .then((s) => (settings.value = s))
    .catch(() => {});
  history_usage()
    .then((u) => (usage.value = u))
    .catch(() => {});
  upload_providers()
    .then((list) => (hostNames.value = new Map(list.map((p) => [p.id, p.name]))))
    .catch(() => {});
  await reload();
});
onBeforeUnmount(() => {
  window.removeEventListener("resize", resized);
  document.removeEventListener("keydown", key);
  document.removeEventListener("mouseover", over);
  document.removeEventListener("focusin", focusIn);
  document.removeEventListener("focusout", hideTip);
  document.removeEventListener("scroll", hideTip, true);
  document.removeEventListener("mousedown", hideTip);
  stops.forEach((stop) => stop());
});

watch(view, (v) => remember("view", v));
watch(zoom, (z) => remember("zoom", String(z)));
watch(panelOpen, (p) => remember("panel", p ? "1" : "0"));
watch(filter, () => {
  if (narrow.value) panelOpen.value = false;
});

const hostNames = ref<ReadonlyMap<string, string>>(new Map());
function hostName(provider: string): string {
  return hostNames.value.get(provider) ?? provider;
}
provide(HISTORY, { lang: language, query, selection, focusId, uploads, previews, now, act, hostName });

const emptyKind = computed<"error" | "first" | "off" | "noResults" | "emptyFilter" | null>(() => {
  if (status.value === "error") return "error";
  if (!loaded.value) return null;
  if (facets.value && facets.value.all === 0 && !searching.value) return historyOff.value ? "off" : "first";
  if (items.value.length) return null;
  return searching.value ? "noResults" : "emptyFilter";
});
const captureKeys = computed(() => capsText(settings.value?.shortcuts.captureArea ?? null) || null);
const emptyFilterText = computed(() => {
  switch (filter.value) {
    case "screenshots":
      return t("history.empty.filter.screenshots");
    case "recordings":
      return t("history.empty.filter.recordings");
    case "textCopies":
      return t("history.empty.filter.textCopies");
    case "uploaded":
      return t("history.empty.filter.uploaded");
    case "pinned":
      return t("history.empty.filter.pinned");
    default:
      return t("history.empty.filter.app");
  }
});
const selectionBytes = computed(() => selected.value.reduce((n, i) => n + (i.entry.fileBytes ?? 0), 0));
const linkCount = computed(() => selected.value.filter((i) => i.entry.hosted).length);
const zoomPercent = computed(() => `${(((zoom.value - 140) / 140) * 100).toFixed(0)}%`);
</script>

<template>
  <div class="hv-window">
    <!-- sidebar -->
    <aside v-if="!narrow" class="g-sidebar">
      <nav class="g-nav" :aria-label="t('history.sidebar.label')">
        <div class="g-nav-section">{{ t("history.sidebar.library") }}</div>
        <button
          v-for="f in LIBRARY_FILTERS"
          :key="f"
          type="button"
          class="g-nav-item"
          :aria-current="filter === f ? 'page' : undefined"
          @click="filter = f"
        >
          <GIcon :name="filterIcon[f]" :size="16" /><span class="hv-nav-label">{{ filterLabel(f) }}</span><span class="g-nav-count">{{ countOf(f) ?? "" }}</span>
        </button>
        <div class="g-nav-section">{{ t("history.sidebar.status") }}</div>
        <button
          v-for="f in STATUS_FILTERS"
          :key="f"
          type="button"
          class="g-nav-item"
          :aria-current="filter === f ? 'page' : undefined"
          @click="filter = f"
        >
          <GIcon :name="filterIcon[f]" :size="16" /><span class="hv-nav-label">{{ filterLabel(f) }}</span><span class="g-nav-count">{{ countOf(f) ?? "" }}</span>
        </button>
        <template v-if="apps.length">
          <div class="g-nav-section">{{ t("history.sidebar.apps") }}</div>
          <button
            v-for="a in apps"
            :key="a.name"
            type="button"
            class="g-nav-item"
            :aria-current="filter === `app:${a.name}` ? 'page' : undefined"
            @click="filter = `app:${a.name}`"
          >
            <AppIcon :name="a.name" /><span class="hv-nav-label">{{ a.name }}</span><span class="g-nav-count">{{ a.count }}</span>
          </button>
        </template>
      </nav>
      <div class="hv-side-foot">
        <b>{{ historyOff ? t("history.foot.offTitle") : footLine }}</b>
        <span>{{ retentionRule }}</span>
        <button v-if="usage && usage.clearableEntries > 0" type="button" class="g-button" data-variant="plain" @click="confirming = { kind: 'clear' }">
          {{ t("history.action.clear") }}
        </button>
      </div>
    </aside>

    <div class="hv-main">
      <!-- toolbar -->
      <header class="g-toolbar">
        <template v-if="narrow">
          <label class="g-select" style="min-width: 0; width: 196px; flex: none">
            <select :aria-label="t('history.toolbar.filter')" :value="filter" @change="filter = ($event.target as HTMLSelectElement).value as Filter">
              <option v-for="f in narrowFilters" :key="f" :value="f">{{ filterLabel(f) }} · {{ countOf(f) ?? 0 }}</option>
            </select>
          </label>
        </template>
        <template v-else>
          <h1 class="hv-title">{{ filterLabel(filter) }}</h1>
          <span class="hv-count">{{ searching ? total : (countOf(filter) ?? "") }}</span>
          <span class="g-spacer" />
        </template>
        <div class="g-seg" role="radiogroup" :aria-label="t('history.toolbar.view')">
          <button type="button" role="radio" :aria-checked="view === 'grid'" :aria-label="t('history.toolbar.grid')" :data-tip="t('history.toolbar.grid')" data-key="⌘1" @click="view = 'grid'">
            <GIcon name="grid" :size="16" />
          </button>
          <button type="button" role="radio" :aria-checked="view === 'list'" :aria-label="t('history.toolbar.list')" :data-tip="t('history.toolbar.list')" data-key="⌘2" @click="view = 'list'">
            <GIcon name="list" :size="16" />
          </button>
        </div>
        <label v-if="!narrow" class="g-select" :style="{ minWidth: 0, width: language === 'en' ? '132px' : '150px', flex: 'none' }">
          <select :aria-label="t('history.toolbar.sort')" :value="effectiveSort" @change="setSort(($event.target as HTMLSelectElement).value as SortOrder)">
            <option v-for="o in sortOptions" :key="o.value" :value="o.value">{{ o.label }}</option>
          </select>
        </label>
        <template v-if="!narrow && !compact">
          <span class="g-divider" role="separator" />
          <div class="g-slider-row hv-zoom">
            <GIcon name="image" :size="14" />
            <input
              v-model.number="zoom"
              type="range"
              class="g-slider"
              min="140"
              max="280"
              step="10"
              :style="{ '--value': zoomPercent }"
              :aria-label="t('history.toolbar.size')"
              :disabled="view === 'list'"
            />
            <GIcon name="image" :size="16" />
          </div>
        </template>
        <span v-if="!narrow" class="g-divider" role="separator" />
        <button
          type="button"
          class="g-btn"
          :aria-label="t('history.toolbar.inspector')"
          :aria-pressed="panelOpen"
          :data-tip="t('history.toolbar.inspector')"
          data-key="⌥⌘I"
          @click="panelOpen = !panelOpen"
        >
          <GIcon name="sidebar" />
        </button>
        <label class="g-search hv-search" :data-wide="narrow ? '' : undefined" :style="narrow ? undefined : { '--search-w': compact ? '240px' : '300px' }">
          <GIcon name="search" :size="16" />
          <input ref="search" v-model="searchText" type="search" :placeholder="t('history.toolbar.search')" :aria-label="t('history.toolbar.search')" aria-keyshortcuts="Meta+F" />
          <button
            v-if="searchText"
            type="button"
            class="g-btn"
            data-size="small"
            :aria-label="t('history.toolbar.clearSearch')"
            :data-tip="t('history.toolbar.clearSearch')"
            data-key="Esc"
            style="margin-right: -6px"
            @click="searchText = ''"
          >
            <GIcon name="close" :size="14" />
          </button>
          <kbd v-else-if="!narrow" class="g-kbd">⌘F</kbd>
        </label>
      </header>

      <!-- history off, with entries: what happens to them -->
      <div v-if="historyOff && emptyKind !== 'off' && loaded && (facets?.all ?? 0) > 0" class="hv-notice" role="status">
        <GIcon name="shield" :size="16" />
        <span
          ><b>{{ t("history.off.title") }}</b> {{ t("history.off.existing", { n: facets?.all ?? 0 }) }}</span
        >
        <button type="button" class="g-button" @click="turnOn">{{ t("history.off.turnOn") }}</button>
      </div>

      <div class="hv-body">
        <!-- empty and error states -->
        <div v-if="emptyKind" class="hv-scroll">
          <EmptyState v-if="emptyKind === 'error'" icon="warning" :title="t('history.error.title')" :text="t('history.error.text', { reason: errorText })">
            <button type="button" class="g-button" data-variant="primary" @click="reload()"><GIcon name="retry" :size="16" />{{ t("history.error.retry") }}</button>
          </EmptyState>
          <EmptyState v-else-if="emptyKind === 'first'" icon="history" :title="t('history.empty.first.title')" :text="captureKeys ? t('history.empty.first.text') : t('history.empty.first.textNoKeys')" :keys="captureKeys">
            <button type="button" class="g-button" data-variant="primary" @click="capture_start('area').catch((e) => failed(e, t('history.empty.first.capture')))">
              <GIcon name="area" :size="16" />{{ t("history.empty.first.capture") }}
            </button>
            <button type="button" class="g-button" @click="window_open('settings').catch(() => {})">{{ t("history.openSettings") }}</button>
          </EmptyState>
          <EmptyState v-else-if="emptyKind === 'off'" icon="shield" :title="t('history.off.emptyTitle')" :text="t('history.off.emptyText')">
            <button type="button" class="g-button" data-variant="primary" @click="turnOn">{{ t("history.off.turnOn") }}</button>
            <button type="button" class="g-button" @click="window_open('settings').catch(() => {})">{{ t("history.openSettings") }}</button>
          </EmptyState>
          <EmptyState
            v-else-if="emptyKind === 'noResults'"
            icon="search"
            :title="filter === 'all' ? t('history.empty.noResults.title', { query }) : t('history.empty.noResults.titleIn', { query, filter: filterLabel(filter) })"
            :text="t('history.empty.noResults.text')"
          >
            <button v-if="filter !== 'all'" type="button" class="g-button" data-variant="primary" @click="filter = 'all'">{{ t("history.empty.noResults.widen") }}</button>
            <button type="button" class="g-button" @click="searchText = ''">{{ t("history.toolbar.clearSearch") }}</button>
          </EmptyState>
          <EmptyState v-else icon="grid" :title="t('history.empty.filter.title', { filter: filterLabel(filter) })" :text="emptyFilterText">
            <button type="button" class="g-button" @click="filter = 'all'">{{ t("history.empty.filter.showAll") }}</button>
          </EmptyState>
        </div>

        <template v-else>
          <Feed v-if="view === 'grid'" ref="feed" :items="items" :total="total" :zoom="narrow ? Math.min(zoom, 180) : zoom" :grouped="grouped" :results="searching ? query : null" :extra-bottom="multi ? 44 : 0" />
          <ListView v-else ref="list" :items="items" :sort="effectiveSort" :extra-bottom="multi ? 44 : 0" @sort="setSort" />
        </template>

        <Inspector
          v-if="showPanel && single && !emptyKind"
          :key="single.entry.id"
          :item="single"
          :full="full"
          :overlay="overlay"
          :current="matchIndex"
          :retention="retention"
          @close="panelOpen = false"
          @step="step"
        />

        <!-- the bulk bar -->
        <div v-if="multi && !emptyKind" class="hv-float">
          <div class="g-bar" role="toolbar" :aria-label="t('history.bulk.label')">
            <span class="hv-bar-text">{{ t("history.bulk.selected", { n: selection.ids.length }) }}</span>
            <span class="g-divider" role="separator" />
            <button type="button" class="g-button" :disabled="!linkCount" :data-tip="t('history.bulk.copyLinksTip')" data-key="⇧⌘L" @click="copyLinks(selection.ids)">
              <GIcon name="link" :size="16" />{{ linkCount === 1 ? t("history.bulk.copyLink") : t("history.bulk.copyLinks", { n: linkCount }) }}
            </button>
            <button type="button" class="g-button" :data-tip="t('history.action.upload')" data-key="⌘U" @click="act.upload(selection.ids)">
              <GIcon name="upload" :size="16" />{{ t("history.action.upload") }}
            </button>
            <button type="button" class="g-button" :data-tip="t('history.bulk.exportTip')" data-key="⌘E" @click="exportSelected(selection.ids)">
              <GIcon name="save" :size="16" />{{ t("history.action.export") }}
            </button>
            <button type="button" class="g-button" data-variant="destructive" :data-tip="t('history.action.delete')" data-key="⌘⌫" @click="act.remove(selection.ids)">
              <GIcon name="trash" :size="16" />{{ t("history.action.deleteMany") }}
            </button>
            <span class="g-divider" role="separator" />
            <button type="button" class="g-btn" :aria-label="t('history.bulk.clear')" :data-tip="t('history.bulk.clear')" data-key="Esc" @click="clearSelection">
              <GIcon name="close" />
            </button>
          </div>
        </div>

        <div class="hv-toasts" aria-live="polite">
          <div v-for="m in toasts" :key="m.id" class="g-toast" role="status" data-state="enter">
            <div class="g-toast-thumb">
              <div style="background: var(--win-raised)" />
              <span class="g-status" :data-status="m.tone === 'warn' ? 'warn' : 'ok'"><GIcon :name="m.tone === 'warn' ? 'bang' : 'check'" :size="12" /></span>
            </div>
            <div class="g-toast-text">
              <div class="g-toast-title">{{ m.title }}</div>
              <div v-if="m.body" class="g-toast-body" :data-kind="m.link ? 'link' : undefined">{{ m.body }}</div>
            </div>
          </div>
        </div>
      </div>

      <footer v-if="view === 'list' && !emptyKind" class="g-statusbar">
        <span>{{
          selection.ids.length
            ? t("history.status.selected", { n: selection.ids.length, total: total, size: formatBytes(selectionBytes, language) })
            : total === 1
              ? t("history.captures.one")
              : t("history.captures.other", { n: total })
        }}</span>
      </footer>
    </div>

    <DeleteSheet v-if="sheet && sheetEntries.length" :entries="sheetEntries" :busy="busy" @confirm="confirmDelete" @cancel="sheet = null" />
    <ConfirmDialog
      v-if="confirming"
      :title="confirmText.title"
      :text="confirmText.text"
      :icon="confirming.kind === 'clear' ? 'history' : 'cloud'"
      tone="danger"
      :confirm-label="confirmText.label"
      :cancel-label="confirming.kind === 'forgetLink' || confirming.kind === 'finishDelete' ? t('history.onPage.notYet') : t('history.delete.cancel')"
      destructive
      :busy="busy"
      @confirm="confirmDialog"
      @cancel="confirming = null"
    />
    <Menu v-if="menu" :items="menu.items" :at="menu.at" :label="t('history.action.more')" @close="menu = null" />
    <div v-if="tip" ref="tipEl" class="g-tip" role="tooltip" :style="{ position: 'fixed', left: 0, top: 0, zIndex: 60, transform: `translate(${tip.x}px, ${tip.y}px)`, pointerEvents: 'none' }">
      {{ tip.text }}<kbd v-if="tip.keys" class="g-kbd">{{ tip.keys }}</kbd>
    </div>
  </div>
</template>
