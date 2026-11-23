<script setup lang="ts">
// The Text panel of the trim window (design row 22, Y1): the speech of the recording as paragraphs
// with their time, the word at the playhead underlined in iris, filler words marked and cut words
// left in place struck through, and two clean-ups over it (every filler word, the long silences).
// The speech model downloads only from this panel's button or Settings › Recording; once it is on
// this computer, a recording without text is read on its own. The panel never changes the cuts
// itself: it asks the window with `edit`, which keeps the undo stack, the timeline, the playback
// and the export in step. A long recording holds thousands of words, so each paragraph renders
// again only when something in it changed (`v-memo`), and the playhead is followed through one
// computed index instead of the clock.
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, shallowRef, watch } from "vue";

import FormRow from "../components/FormRow.vue";
import GIcon from "../components/GIcon.vue";
import SelectControl from "../components/SelectControl.vue";
import { ApiError, on } from "../lib/api/core";
import type { RecordingInfo, TrimRange } from "../lib/api/record";
import { settings_get, settings_set } from "../lib/api/settings";
import {
  TRANSCRIPT_MODEL_PROGRESS,
  TRANSCRIPT_PROGRESS,
  transcript_cancel,
  transcript_get,
  transcript_model,
  transcript_model_download,
  transcript_start,
  type SpeechModelInfo,
  type SpeechModelProgress,
  type Transcript,
  type TranscriptLanguage,
  type TranscriptProgress,
  type TranscriptSilence,
  type TranscriptState,
  type TranscriptWord,
} from "../lib/api/transcript";
import { language, t, tp, type Key } from "../lib/i18n";
import type { Recording, Settings } from "../lib/settings";
import { audioLayout } from "./choice";
import {
  addRanges,
  cutAt,
  DEFAULT_SILENCE_THRESHOLD_MS,
  fillerCuts,
  isLong,
  normalize,
  restoreFillers,
  restoreSilences,
  selectionRange,
  SILENCE_KEEP_MS,
  SILENCE_THRESHOLDS,
  silenceCuts,
  silenceStates,
  withoutCut,
  wordBefore,
  wordsCut,
} from "./cuts";
import { clock, reducedMotion, tenths } from "./format";
import TextMenu, { type TextMenuItem } from "./TextMenu.vue";

const props = defineProps<{
  info: RecordingInfo;
  /** The removed ranges, sorted and apart. */
  cuts: readonly TrimRange[];
  playMs: number;
  /** What the export keeps once the trim and the cuts are applied. */
  keptMs: number;
  settings: Settings | null;
  /** An export runs, and nothing changes until it ends. */
  locked: boolean;
}>();

const emit = defineEmits<{
  /** The cuts the user asked for; the window applies them as one undoable step. */
  edit: [cuts: TrimRange[]];
  /** The cuts saved with the transcript, for a recording that opened. */
  loaded: [recordingId: string, cuts: TrimRange[]];
  seek: [ms: number];
  /** Play from `ms`; playback goes through `exempt` (a cut) instead of skipping it. */
  play: [ms: number, exempt: TrimRange | null];
  /** How far the speech is read while it is read, for the timeline; `null` otherwise. */
  transcribed: [ms: number | null];
  /** The cut whose menu is open, outlined on the timeline. */
  pick: [cut: TrimRange | null];
  settings: [settings: Settings];
}>();

// ---------------------------------------------------------------------------------- state

type Phase =
  | { kind: "loading" }
  | { kind: "noAudio" }
  /** This build has no speech engine yet. */
  | { kind: "unavailable" }
  | { kind: "prompt"; problem: string | null }
  /** `adopted`: a job this page did not start (the window opened while it ran). */
  | { kind: "transcribing"; doneMs: number; totalMs: number; adopted: boolean }
  | { kind: "failed"; problem: string }
  | { kind: "ready" };

type Download = { job: string | null; received: number; total: number; remainingMs: number | null };
type Meta = { language: string; auto: boolean; probability: number | null };
type Selection = { from: number; to: number };

const root = ref<HTMLElement | null>(null);
const textEl = ref<HTMLElement | null>(null);
const searchInput = ref<HTMLInputElement | null>(null);

const phase = shallowRef<Phase>({ kind: "loading" });
const model = shallowRef<SpeechModelInfo | null>(null);
const download = ref<Download | null>(null);
const words = shallowRef<TranscriptWord[]>([]);
const silences = shallowRef<TranscriptSilence[]>([]);
const meta = shallowRef<Meta | null>(null);
const selection = shallowRef<Selection | null>(null);
const hideCut = ref(false);
/** The text took the focus from a press, which shows no focus ring; Tab still does. */
const pointerFocus = ref(false);
const threshold = ref<number>(DEFAULT_SILENCE_THRESHOLD_MS);
const shorten = ref(true);
/** The language the next or the running job reads in. */
const requested = ref<TranscriptLanguage>("auto");
/** Re-renders the language pop-up after a pick, so it shows the transcript's language again. */
const langTurn = ref(0);

/** The recording the panel shows; every answer for another one is dropped. */
let current = "";
let loadTurn = 0;
let jobTurn = 0;
/** The word a ⇧-click extends the selection from. */
let anchor = -1;
let press: { from: number; moved: boolean } | null = null;
let userScrolledAt = 0;
let watchdog = 0;
let foreignTimer = 0;
/** A language chosen in this window, which wins over the settings until the window closes. */
let pickedLanguage: string | null = null;
let settingsRead = false;
const stops: (() => void)[] = [];

const editable = computed(() => !props.locked && phase.value.kind === "ready");

// ------------------------------------------------------------------------------- wording

const num1 = (v: number) => new Intl.NumberFormat(language.value, { minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(v);
const sec = (ms: number) => t("record.transcript.seconds", { n: num1(ms / 1000) });
const keepText = computed(() => sec(SILENCE_KEEP_MS));

function megabytes(bytes: number): string {
  const n = (v: number, digits: number) => new Intl.NumberFormat(language.value, { maximumFractionDigits: digits }).format(v);
  return bytes >= 1e9 ? `${n(bytes / 1e9, 1)} GB` : `${n(Math.max(0, bytes) / 1e6, 0)} MB`;
}

function leftText(ms: number | null): string {
  if (ms == null) return "";
  if (ms < 2000) return t("record.transcript.almost");
  const s = Math.ceil(ms / 1000);
  return s < 60 ? t("record.transcript.leftSeconds", { n: s }) : t("record.transcript.leftMinutes", { n: Math.round(s / 60) });
}

/** The many or the one sentence for a count. */
const counted = (many: Key, one: Key, n: number, params: Record<string, string | number> = {}) =>
  t(n === 1 ? one : many, { n, ...params });

const percent = (fraction: number) => t("record.export.percent", { n: Math.round(Math.min(1, Math.max(0, fraction)) * 100) });

function safeLocale(tag: string): string | undefined {
  try {
    return Intl.getCanonicalLocales(tag)[0];
  } catch {
    return undefined;
  }
}

/** A word as search and the filler examples compare it: lower case, without punctuation. */
function bare(text: string, locale: string | undefined): string {
  return text.toLocaleLowerCase(locale).replace(/[\p{P}\p{S}]+/gu, "");
}

const textLocale = computed(() => safeLocale(meta.value?.language ?? language.value));

const LANGUAGES = ["tr", "en", "de", "es", "ru", "ja", "zh"] as const;

function languageName(code: string): string {
  try {
    const name = new Intl.DisplayNames([language.value], { type: "language" }).of(code);
    return name ? name.charAt(0).toLocaleUpperCase(language.value) + name.slice(1) : code;
  } catch {
    return code;
  }
}

const languageValue = computed(() => meta.value?.language ?? requested.value);
const languageOptions = computed(() => {
  const m = meta.value;
  const codes: string[] = [...LANGUAGES];
  for (const code of [m?.language, requested.value]) if (code && code !== "auto" && !codes.includes(code)) codes.push(code);
  return [
    { value: "auto", label: t("record.transcript.auto") },
    // "· detected" says where a doubtful language came from; a sure detection shows the name alone.
    ...codes.map((code) => ({
      value: code,
      label: unsure.value && m?.language === code ? t("record.transcript.detected", { language: languageName(code) }) : languageName(code),
    })),
  ];
});
/** A detection the model was unsure of: the text may be read in the wrong language, so the panel
 * asks for the language and leaves the clean-ups out until the text reads right. */
const unsure = computed(() => !!meta.value?.auto && (meta.value.probability ?? 1) < 0.6);

// ------------------------------------------------------------------------------ the text

const cutFlags = computed(() => wordsCut(words.value, props.cuts));
const silenceState = computed(() => silenceStates(silences.value, props.cuts));

const PAUSE_MS = 2000;
const SENTENCE_END = /[.!?…。！？]["'”’»)\]]*$/;

/** Whether the sentence that starts at `i` is a short one (a few words) followed by a long pause
 * or the end of the text: a paragraph break waits for that pause instead of leaving it alone. */
function shortTail(list: readonly TranscriptWord[], i: number): boolean {
  for (let j = i; j < list.length && j - i < 6; j++) {
    if (!SENTENCE_END.test(list[j]!.text)) continue;
    const next = list[j + 1];
    return !next || next.startMs - list[j]!.endMs >= PAUSE_MS;
  }
  return false;
}

/** The first word of each paragraph, so the text reads in pieces of a few sentences: a sentence
 * end starts one after a long pause once the paragraph holds ten words, or after a breath once it
 * holds fifteen; a very long pause or 140 words start one anywhere. */
const breaks = computed(() => {
  const list = words.value;
  const out: number[] = [];
  let count = 0;
  for (let i = 0; i < list.length; i++) {
    const prev = list[i - 1];
    const gap = prev ? list[i]!.startMs - prev.endMs : Infinity;
    let start = !prev || count >= 140 || (gap >= 1.5 * PAUSE_MS && count >= 10);
    if (!start && SENTENCE_END.test(prev!.text)) {
      start = (gap >= PAUSE_MS && count >= 10) || (count >= 15 && gap >= 300 && !shortTail(list, i));
    }
    if (start) {
      out.push(i);
      count = 0;
    }
    count++;
  }
  return out;
});

type Item = { k: "w"; i: number } | { k: "s"; i: number } | { k: "m"; from: number; to: number };
type Para = { key: number; first: number; last: number; startMs: number; items: Item[]; sig: string };

/** The paragraphs with their words, the silences worth showing between them (the long ones and
 * every one a cut touches) and, when cut words are hidden, one scissors mark per hidden run. */
const paras = computed<Para[]>(() => {
  const list = words.value;
  const starts = breaks.value;
  const cut = cutFlags.value;
  const sil = silences.value;
  const states = silenceState.value;
  const hide = hideCut.value;
  const th = threshold.value;
  const out: Para[] = [];
  let s = 0;
  for (let p = 0; p < starts.length; p++) {
    const first = starts[p]!;
    const last = (starts[p + 1] ?? list.length) - 1;
    const items: Item[] = [];
    let sig = "";
    const pushWord = (i: number) => {
      if (hide && cut[i]) {
        const prev = items[items.length - 1];
        if (prev?.k === "m") prev.to = i;
        else items.push({ k: "m", from: i, to: i });
        return;
      }
      items.push({ k: "w", i });
      sig += cut[i] ? "x" : list[i]!.filler ? "f" : "w";
    };
    const pushSilence = (k: number) => {
      const st = states[k]!;
      if (st.state === "none" && !isLong(sil[k]!, th)) return;
      if (hide && st.state === "cut") return;
      items.push({ k: "s", i: k });
      sig += `s${k}${st.state[0]}${st.keptMs};`;
    };
    for (let i = first; i <= last; i++) {
      while (s < sil.length && sil[s]!.startMs < list[i]!.startMs) pushSilence(s++);
      pushWord(i);
    }
    const nextStart = list[last + 1]?.startMs ?? Infinity;
    while (s < sil.length && sil[s]!.startMs < nextStart) pushSilence(s++);
    for (const it of items) if (it.k === "m") sig += `m${it.from}-${it.to};`;
    out.push({ key: first, first, last, startMs: list[first]!.startMs, items, sig });
  }
  return out;
});

/** The word under the playhead, underlined; none while the playhead is in a pause. */
const followIndex = computed(() => wordBefore(words.value, props.playMs));
const nowIndex = computed(() => {
  const i = followIndex.value;
  const w = words.value[i];
  return w && props.playMs < w.endMs + 150 ? i : -1;
});

const selKey = (p: Para) => {
  const s = selection.value;
  return !s || s.to < p.first || s.from > p.last ? "" : `${Math.max(s.from, p.first)}-${Math.min(s.to, p.last)}`;
};
const nowKey = (p: Para) => (nowIndex.value >= p.first && nowIndex.value <= p.last ? nowIndex.value : -1);
type Run = { key: string; sel: boolean; items: Item[] };

/** A paragraph's items in up to three runs, the selected one in the middle: the selection is one
 * wrapping band, as the design draws it, so its tint does not pile up where words meet. */
function runsOf(p: Para): Run[] {
  const s = selection.value;
  const inside = (it: Item) => it.k === "w" && !!s && it.i >= s.from && it.i <= s.to;
  const a = s && s.to >= p.first && s.from <= p.last ? p.items.findIndex(inside) : -1;
  if (a < 0) return [{ key: "a", sel: false, items: p.items }];
  let b = p.items.length - 1;
  while (b > a && !inside(p.items[b]!)) b--;
  const out: Run[] = [];
  if (a > 0) out.push({ key: "a", sel: false, items: p.items.slice(0, a) });
  out.push({ key: "s", sel: true, items: p.items.slice(a, b + 1) });
  if (b + 1 < p.items.length) out.push({ key: "b", sel: false, items: p.items.slice(b + 1) });
  return out;
}

function silenceLabel(k: number): string {
  const s = silences.value[k]!;
  const st = silenceState.value[k]!;
  const length = s.endMs - s.startMs;
  return st.state === "short" ? `${sec(length)} → ${sec(st.keptMs)}` : sec(length);
}

function markLabel(it: { from: number; to: number }): string {
  return t("record.transcript.cutMark", { text: words.value.slice(it.from, it.to + 1).map((w) => w.text.trim()).join(" ") });
}

// ----------------------------------------------------------------------------- clean-ups

const fillers = computed(() => {
  const list = words.value;
  const cut = cutFlags.value;
  const locale = textLocale.value;
  const seen = new Map<string, { n: number; sound: boolean }>();
  let total = 0;
  let done = 0;
  list.forEach((w, i) => {
    if (!w.filler) return;
    total++;
    if (cut[i]) done++;
    const key = bare(w.text, locale);
    if (!key) return;
    const entry = seen.get(key) ?? { n: 0, sound: w.filler === "sound" };
    entry.n++;
    seen.set(key, entry);
  });
  // The hesitation sounds first, as the design lists them (ııı, eee, şey, yani, hani).
  const examples = [...seen.entries()]
    .sort((a, b) => Number(b[1].sound) - Number(a[1].sound) || b[1].n - a[1].n)
    .slice(0, 5)
    .map(([k]) => k)
    .join(", ");
  return { total, done, all: total > 0 && done === total, examples };
});

const fillerHelp = computed(() => {
  const f = fillers.value;
  if (!f.total) return t("record.transcript.fillersNone");
  if (f.all) return counted("record.transcript.fillersDone", "record.transcript.fillersDoneOne", f.total);
  return counted("record.transcript.fillersCount", "record.transcript.fillersCountOne", f.total, { examples: f.examples });
});

const longSilences = computed(() => {
  const th = threshold.value;
  const states = silenceState.value;
  let total = 0;
  let short = 0;
  let cut = 0;
  silences.value.forEach((s, i) => {
    if (!isLong(s, th)) return;
    total++;
    if (states[i]!.state === "short") short++;
    else if (states[i]!.state === "cut") cut++;
  });
  return { total, short, cut, all: total > 0 && short + cut === total };
});

const silenceHelp = computed(() => {
  const s = longSilences.value;
  const th = sec(threshold.value);
  if (!s.total) return t("record.transcript.silencesNone", { threshold: th });
  if (s.all && s.cut === s.total) return counted("record.transcript.silencesCut", "record.transcript.silencesCutOne", s.total);
  if (s.all) return counted("record.transcript.silencesShortened", "record.transcript.silencesShortenedOne", s.total, { keep: keepText.value });
  return counted("record.transcript.silencesCount", "record.transcript.silencesCountOne", s.total, { threshold: th });
});

function edit(next: TrimRange[]): void {
  if (props.locked) return;
  emit("edit", next);
}

function cutFillers(): void {
  edit(fillerCuts(words.value, props.cuts));
}

function putFillersBack(): void {
  edit(restoreFillers(words.value, props.cuts));
}

function applySilences(shortenThem: boolean): void {
  if (shorten.value !== shortenThem) {
    shorten.value = shortenThem;
    void saveSettings({ silenceShorten: shortenThem });
  }
  edit(silenceCuts(silences.value, props.cuts, threshold.value, shortenThem));
}

function putSilencesBack(): void {
  edit(restoreSilences(words.value, silences.value, props.cuts, threshold.value));
}

function setThreshold(ms: number): void {
  if (threshold.value === ms) return;
  threshold.value = ms;
  void saveSettings({ silenceThresholdMs: ms });
}

/** Writes the panel's choices into Settings › Recording, reading the settings fresh first so a
 * change another window made in the meantime is kept. */
async function saveSettings(change: Partial<Pick<Recording, "transcriptLanguage" | "silenceThresholdMs" | "silenceShorten">>): Promise<void> {
  try {
    const now = await settings_get();
    const saved = await settings_set({ ...now, recording: { ...now.recording, ...change } });
    emit("settings", saved.settings);
  } catch (error) {
    console.warn("The text panel's choice was not saved.", error);
  }
}

function readSettings(s: Settings | null): void {
  if (!s || settingsRead) return;
  settingsRead = true;
  const r = s.recording as Partial<Recording>;
  threshold.value = SILENCE_THRESHOLDS.find((ms) => ms === r.silenceThresholdMs) ?? DEFAULT_SILENCE_THRESHOLD_MS;
  shorten.value = r.silenceShorten ?? true;
}

const wantedLanguage = (): TranscriptLanguage =>
  pickedLanguage ?? ((props.settings?.recording as Partial<Recording> | undefined)?.transcriptLanguage || "auto");

// ----------------------------------------------------------------------- loading, jobs

const codeOf = (error: unknown) => (error instanceof ApiError ? error.code : "internal");

function clearText(): void {
  words.value = [];
  silences.value = [];
  meta.value = null;
  selection.value = null;
  anchor = -1;
  press = null;
}

async function load(): Promise<void> {
  const mine = ++loadTurn;
  const id = props.info.result.id;
  current = id;
  window.clearTimeout(watchdog);
  closeMenu();
  closeSearch();
  clearText();
  hideCut.value = false;
  requested.value = wantedLanguage();
  emit("transcribed", null);
  if (audioLayout(props.info) === "none") {
    phase.value = { kind: "noAudio" };
    return;
  }
  phase.value = { kind: "loading" };
  let state: TranscriptState | null = null;
  try {
    state = await transcript_get(id);
  } catch (error) {
    if (mine !== loadTurn) return;
    if (codeOf(error) === "not_implemented") {
      phase.value = { kind: "unavailable" };
      return;
    }
    console.warn("The saved text of the recording could not be read.", error);
  }
  if (mine !== loadTurn) return;
  if (state?.transcript && !state.running) return show(id, state.transcript);
  if (state?.running) return adopt(state.running);
  const m = await readModel();
  if (mine !== loadTurn || !m || phase.value.kind !== "loading") return;
  if (m.installed) void transcribe(requested.value);
  else phase.value = { kind: "prompt", problem: null };
}

async function readModel(): Promise<SpeechModelInfo | null> {
  try {
    model.value = await transcript_model();
    return model.value;
  } catch (error) {
    if (codeOf(error) === "not_implemented") phase.value = { kind: "unavailable" };
    else phase.value = { kind: "prompt", problem: t("record.transcript.failed") };
    return null;
  }
}

function show(id: string, transcript: Transcript): void {
  window.clearTimeout(watchdog);
  words.value = transcript.words;
  silences.value = transcript.silences;
  meta.value = { language: transcript.language, auto: transcript.languageAuto, probability: transcript.languageProbability };
  phase.value = { kind: "ready" };
  emit("transcribed", null);
  emit("loaded", id, normalize(transcript.cuts ?? []));
}

/** Shows a job that runs without this page having started it, and asks for its transcript once
 * it stops reporting or reaches the end. */
function adopt(progress: TranscriptProgress): void {
  clearText();
  words.value = [...progress.words];
  phase.value = { kind: "transcribing", doneMs: progress.doneMs, totalMs: progress.totalMs, adopted: true };
  emit("transcribed", progress.doneMs);
  armWatchdog(progress.doneMs >= progress.totalMs ? 600 : 15000);
}

function armWatchdog(ms: number): void {
  window.clearTimeout(watchdog);
  watchdog = window.setTimeout(() => void recheck(), ms);
}

async function recheck(): Promise<void> {
  const id = current;
  const ph = phase.value;
  if (ph.kind !== "transcribing" || !ph.adopted) return;
  try {
    const state = await transcript_get(id);
    if (id !== current || phase.value !== ph) return;
    if (state.running) armWatchdog(2000);
    else if (state.transcript) show(id, state.transcript);
    else fail(t("record.transcript.stopped"));
  } catch {
    armWatchdog(5000);
  }
}

function fail(problem: string): void {
  phase.value = { kind: "failed", problem };
  emit("transcribed", null);
}

/** `transcript_start` fails with `busy` while a stopped job winds down; after a cancel this page
 * asked for, it tries again for a few seconds. */
async function startJob(id: string, lang: TranscriptLanguage, afterCancel: boolean): Promise<Transcript> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await transcript_start(id, lang);
    } catch (error) {
      if (!afterCancel || codeOf(error) !== "busy" || attempt >= 10) throw error;
      await new Promise((resolve) => window.setTimeout(resolve, 300));
    }
  }
}

async function transcribe(lang: TranscriptLanguage, afterCancel = false): Promise<void> {
  const id = current;
  const mine = ++jobTurn;
  requested.value = lang;
  window.clearTimeout(watchdog);
  closeMenu();
  clearText();
  phase.value = { kind: "transcribing", doneMs: 0, totalMs: props.info.result.durationMs, adopted: false };
  emit("transcribed", 0);
  try {
    const transcript = await startJob(id, lang, afterCancel);
    if (mine !== jobTurn || id !== current) return;
    show(id, transcript);
  } catch (error) {
    if (mine !== jobTurn || id !== current) return;
    const code = codeOf(error);
    if (code === "not_implemented") {
      phase.value = { kind: "unavailable" };
      emit("transcribed", null);
    } else if (code === "not_found") {
      // The model went away (Settings deleted it), so the panel offers the download again.
      emit("transcribed", null);
      if (await readModel()) phase.value = { kind: "prompt", problem: null };
    } else {
      console.error(error);
      fail(t(code === "busy" ? "record.transcript.busy" : code === "cancelled" ? "record.transcript.stopped" : "record.transcript.failed"));
    }
  }
}

function retry(): void {
  void transcribe(requested.value);
}

async function pickLanguage(code: string): Promise<void> {
  langTurn.value++;
  const m = meta.value;
  if (m && (code === m.language ? !m.auto : code === "auto" && m.auto)) return;
  pickedLanguage = code;
  void saveSettings({ transcriptLanguage: code });
  const running = phase.value.kind === "transcribing";
  if (running) await transcript_cancel(null).catch((error) => console.warn("The reading could not be stopped.", error));
  void transcribe(code, running);
}

function onProgress(p: TranscriptProgress): void {
  if (p.recordingId !== current) return;
  const ph = phase.value;
  if (ph.kind === "ready" || ph.kind === "noAudio" || ph.kind === "unavailable") return;
  if (ph.kind !== "transcribing") return adopt(p);
  const last = words.value[words.value.length - 1];
  const fresh = last ? p.words.filter((w) => w.startMs > last.startMs) : p.words;
  if (fresh.length) words.value = [...words.value, ...fresh];
  phase.value = { ...ph, doneMs: p.doneMs, totalMs: p.totalMs };
  emit("transcribed", p.doneMs);
  if (ph.adopted) armWatchdog(p.doneMs >= p.totalMs ? 600 : 15000);
}

// --------------------------------------------------------------------------- the model

async function downloadModel(): Promise<void> {
  if (download.value) return;
  const job = `speech-model-${Date.now()}`;
  download.value = { job, received: 0, total: model.value?.bytes ?? 0, remainingMs: null };
  if (phase.value.kind === "prompt") phase.value = { kind: "prompt", problem: null };
  try {
    const m = await transcript_model_download(job);
    model.value = m;
    download.value = null;
    if (m.installed && phase.value.kind === "prompt") void transcribe(requested.value);
  } catch (error) {
    download.value = null;
    const code = codeOf(error);
    if (code === "cancelled") return;
    if (code === "not_implemented") phase.value = { kind: "unavailable" };
    else if (phase.value.kind === "prompt") phase.value = { kind: "prompt", problem: t("record.transcript.downloadFailed") };
  }
}

function cancelDownload(): void {
  const job = download.value?.job;
  if (job) transcript_cancel(job).catch((error) => console.warn("The download could not be stopped.", error));
}

/** A download Settings started shows here too, without Cancel (its job is Settings'); when it
 * ends or stops reporting, the model is read again. */
function onModelProgress(p: SpeechModelProgress): void {
  if (!download.value) download.value = { job: null, received: p.received, total: p.total, remainingMs: p.remainingMs };
  else Object.assign(download.value, { received: p.received, total: p.total, remainingMs: p.remainingMs });
  if (download.value.job) return;
  window.clearTimeout(foreignTimer);
  foreignTimer = window.setTimeout(() => void foreignEnded(), p.total > 0 && p.received >= p.total ? 800 : 10000);
}

async function foreignEnded(): Promise<void> {
  if (!download.value || download.value.job) return;
  download.value = null;
  const was = phase.value.kind;
  const m = await readModel();
  if (m?.installed && was === "prompt" && phase.value.kind === "prompt") void transcribe(requested.value);
  else if (m && was === "prompt") phase.value = { kind: "prompt", problem: null };
}

const downloadText = computed(() => {
  const d = download.value;
  if (!d) return "";
  const total = d.total || model.value?.bytes || 0;
  return [
    t("record.transcript.downloaded", { received: megabytes(d.received), total: megabytes(total) }),
    leftText(d.remainingMs),
    t("record.transcript.afterDownload"),
  ]
    .filter(Boolean)
    .join(" ");
});
const downloadFraction = computed(() => {
  const d = download.value;
  const total = d?.total || model.value?.bytes || 0;
  return d && total > 0 ? Math.min(1, d.received / total) : 0;
});

// ------------------------------------------------------------------- pointer and menus

type Hit = { kind: "w"; i: number } | { kind: "s"; i: number } | { kind: "m"; from: number; to: number; el: HTMLElement };

function hitOf(target: EventTarget | null): Hit | null {
  const el = target instanceof Element ? target.closest<HTMLElement>("[data-i], [data-s], [data-m]") : null;
  if (!el) return null;
  if (el.dataset.i !== undefined) return { kind: "w", i: Number(el.dataset.i) };
  if (el.dataset.s !== undefined) return { kind: "s", i: Number(el.dataset.s) };
  const [from, to] = (el.dataset.m ?? "").split("-").map(Number);
  return { kind: "m", from: from ?? 0, to: to ?? 0, el };
}

const middle = (r: { startMs: number; endMs: number }) => (r.startMs + r.endMs) / 2;

function select(a: number, b: number): void {
  selection.value = { from: Math.min(a, b), to: Math.max(a, b) };
}

function down(event: PointerEvent): void {
  if (event.button !== 0 || phase.value.kind === "loading") return;
  pointerFocus.value = true;
  textEl.value?.focus({ preventScroll: true });
  const hit = hitOf(event.target);
  if (!hit) {
    selection.value = null;
    return;
  }
  event.preventDefault();
  if (hit.kind === "s") {
    selection.value = null;
    emit("seek", silences.value[hit.i]!.startMs);
  } else if (hit.kind === "m") {
    // The mark opens its menu on the click, after the press is over, or the press would close it.
    selection.value = null;
  } else if (event.shiftKey && anchor >= 0) {
    select(anchor, hit.i);
  } else {
    press = { from: hit.i, moved: false };
    textEl.value?.setPointerCapture(event.pointerId);
  }
}

function move(event: PointerEvent): void {
  if (!press) return;
  const box = textEl.value;
  if (box) {
    const r = box.getBoundingClientRect();
    // A drag past the edge scrolls the text, so a selection can run beyond what shows.
    if (event.clientY < r.top + 20) box.scrollTop -= 14;
    else if (event.clientY > r.bottom - 20) box.scrollTop += 14;
  }
  const hit = hitOf(document.elementFromPoint(event.clientX, event.clientY));
  if (hit?.kind !== "w" || (hit.i === press.from && !press.moved)) return;
  press.moved = true;
  select(press.from, hit.i);
}

function clicked(event: MouseEvent): void {
  const hit = hitOf(event.target);
  if (hit?.kind !== "m") return;
  const r = hit.el.getBoundingClientRect();
  openCutMenu(words.value[hit.from]!, r.left, r.bottom + 4);
}

function cancelPress(): void {
  press = null;
}

function up(): void {
  const p = press;
  press = null;
  if (!p) return;
  anchor = p.from;
  if (p.moved) return;
  selection.value = null;
  emit("seek", words.value[p.from]!.startMs);
}

function context(event: MouseEvent): void {
  event.preventDefault();
  const hit = hitOf(event.target);
  if (!hit || (!words.value.length && hit.kind !== "s")) return;
  if (hit.kind === "m") return openCutMenu(words.value[hit.from]!, event.clientX, event.clientY);
  if (hit.kind === "s") {
    const s = silences.value[hit.i]!;
    if (silenceState.value[hit.i]!.state !== "none") return openCutMenu(s, event.clientX, event.clientY);
    menu.value = { kind: "silence", at: { x: event.clientX, y: event.clientY }, silence: hit.i };
    return;
  }
  const w = words.value[hit.i]!;
  if (cutFlags.value[hit.i]) return openCutMenu(w, event.clientX, event.clientY);
  const s = selection.value;
  if (!s || hit.i < s.from || hit.i > s.to) {
    select(hit.i, hit.i);
    anchor = hit.i;
  }
  menu.value = { kind: "selection", at: { x: event.clientX, y: event.clientY } };
}

/** The menu of a cut word, a cut silence or a scissors mark: "Put back" first. */
function openCutMenu(where: { startMs: number; endMs: number }, x: number, y: number): void {
  const cut = cutAt(props.cuts, middle(where));
  if (cut) openRestore(cut, where.startMs, x, y);
}

function openRestore(cut: TrimRange, fromMs: number, x: number, y: number): void {
  menu.value = { kind: "restore", at: { x, y }, cut, fromMs };
  emit("pick", cut);
}

type MenuState =
  | { kind: "selection"; at: { x: number; y: number } }
  | { kind: "restore"; at: { x: number; y: number }; cut: TrimRange; fromMs: number }
  | { kind: "silence"; at: { x: number; y: number }; silence: number }
  | { kind: "shorten"; at: { x: number; y: number } };

const menu = shallowRef<MenuState | null>(null);

let closed = { kind: "", at: 0 };

function closeMenu(): void {
  if (!menu.value) return;
  if (menu.value.kind === "restore") emit("pick", null);
  closed = { kind: menu.value.kind, at: performance.now() };
  menu.value = null;
}

function openShorten(event: MouseEvent): void {
  // The press on the button closed its open menu a moment ago; the click leaves it closed.
  if (closed.kind === "shorten" && performance.now() - closed.at < 300) return;
  const button = event.currentTarget as HTMLElement;
  // The menu lines up with the group's edge, as the design draws it, and opens under the button.
  const edge = (button.closest(".g-group") ?? button).getBoundingClientRect().right;
  menu.value = { kind: "shorten", at: { x: edge, y: button.getBoundingClientRect().bottom + 4 } };
}

const menuItems = computed<TextMenuItem[]>(() => {
  const m = menu.value;
  if (!m) return [];
  if (m.kind === "selection") {
    const s = selection.value;
    return [
      { label: t("record.transcript.cut"), icon: "scissors", keys: "⌫", disabled: !editable.value || !s, run: () => void cutSelection() },
      { label: t("record.transcript.playFrom"), icon: "play", disabled: !s, run: () => s && emit("play", words.value[s.from]!.startMs, null) },
      { label: t("record.transcript.copyText"), icon: "copy", keys: "⌘C", disabled: !s, run: () => void copySelection() },
    ];
  }
  if (m.kind === "restore") {
    return [
      { label: t("record.transcript.restore"), icon: "undo", disabled: props.locked, run: () => edit(withoutCut(props.cuts, m.cut)) },
      { label: t("record.transcript.playFrom"), icon: "play", run: () => emit("play", m.fromMs, m.cut) },
      "separator",
      {
        label: hideCut.value ? t("record.transcript.showCut") : t("record.transcript.hideCut"),
        icon: hideCut.value ? "eye" : "eye-off",
        run: () => (hideCut.value = !hideCut.value),
      },
    ];
  }
  if (m.kind === "silence") {
    const s = silences.value[m.silence]!;
    return [
      {
        label: t("record.transcript.shortenTo", { keep: keepText.value }),
        icon: "scissors",
        disabled: !editable.value || s.endMs - s.startMs <= SILENCE_KEEP_MS,
        run: () => edit(addRanges(props.cuts, [{ startMs: s.startMs + SILENCE_KEEP_MS / 2, endMs: s.endMs - SILENCE_KEEP_MS / 2 }])),
      },
      { label: t("record.transcript.cutWhole"), icon: "scissors", disabled: !editable.value, run: () => edit(addRanges(props.cuts, [{ ...s }])) },
      "separator",
      { label: t("record.transcript.playFrom"), icon: "play", run: () => emit("play", s.startMs, null) },
    ];
  }
  return [
    { label: t("record.transcript.shortenTo", { keep: keepText.value }), checked: shorten.value, run: () => applySilences(true) },
    { label: t("record.transcript.cutWhole"), checked: !shorten.value, run: () => applySilences(false) },
    "separator",
    { heading: t("record.transcript.threshold") },
    ...SILENCE_THRESHOLDS.map((ms) => ({
      label: t("record.transcript.longerThan", { threshold: sec(ms) }),
      checked: threshold.value === ms,
      keep: true,
      run: () => setThreshold(ms),
    })),
  ];
});

// ------------------------------------------------------------------------ the keyboard

/** ⌫ cuts the selected words. */
function cutSelection(): boolean {
  const s = selection.value;
  if (!s || !editable.value) return false;
  const range = selectionRange(words.value, s.from, s.to);
  if (!range) return false;
  edit(addRanges(props.cuts, [range]));
  selection.value = null;
  return true;
}

/** ⌘C copies the selected words that are kept, while the panel has the focus (`target`), and
 * the recording's file otherwise. */
function copySelection(target?: EventTarget | null): boolean {
  const s = selection.value;
  if (!s) return false;
  if (target !== undefined && !(target instanceof Node && root.value?.contains(target))) return false;
  const cut = cutFlags.value;
  const text = words.value
    .slice(s.from, s.to + 1)
    .filter((_, k) => !cut[s.from + k])
    .map((w) => w.text.trim())
    .join(" ");
  navigator.clipboard?.writeText(text).catch((error) => console.warn("The text could not be copied.", error));
  return true;
}

/** Esc closes the menu, then the search, then the selection. */
function escape(): boolean {
  if (menu.value) closeMenu();
  else if (search.open) closeSearch();
  else if (selection.value) selection.value = null;
  else return false;
  return true;
}

// ------------------------------------------------------------------------------ search

const search = reactive({ open: false, query: "", index: 0 });

const normalized = computed(() => {
  const locale = textLocale.value;
  return words.value.map((w) => bare(w.text, locale));
});

/** The runs of words that read as the query: one word holds it, or several words in a row start
 * with its end and end with its start. */
const matches = computed(() => {
  const q = search.open ? search.query.trim() : "";
  if (!q) return [];
  const tokens = q
    .split(/\s+/)
    .map((x) => bare(x, textLocale.value))
    .filter(Boolean);
  const n = tokens.length;
  if (!n) return [];
  const norm = normalized.value;
  const out: Selection[] = [];
  for (let i = 0; i + n <= norm.length && out.length < 5000; i++) {
    if (n === 1) {
      if (norm[i]!.includes(tokens[0]!)) out.push({ from: i, to: i });
      continue;
    }
    if (!norm[i]!.endsWith(tokens[0]!) || !norm[i + n - 1]!.startsWith(tokens[n - 1]!)) continue;
    let whole = true;
    for (let k = 1; k < n - 1 && whole; k++) whole = norm[i + k] === tokens[k];
    if (whole) out.push({ from: i, to: i + n - 1 });
  }
  return out;
});

const currentMatch = computed(() => (matches.value.length ? Math.min(search.index, matches.value.length - 1) : -1));

/** 1 for a word of a match, 2 for a word of the current one, and each paragraph's share of them,
 * so only paragraphs with matches render again. */
const hits = computed(() => {
  const flags = new Uint8Array(words.value.length);
  const sig = new Map<number, string>();
  const starts = breaks.value;
  matches.value.forEach((m, k) => {
    const on = k === currentMatch.value;
    for (let i = m.from; i <= m.to; i++) flags[i] = on ? 2 : 1;
    const p = paragraphStart(starts, m.from);
    sig.set(p, `${sig.get(p) ?? ""}${m.from}-${m.to}${on ? "*" : ""};`);
  });
  return { flags, sig };
});

function paragraphStart(starts: readonly number[], i: number): number {
  let lo = 0;
  let hi = starts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (starts[mid]! <= i) lo = mid + 1;
    else hi = mid;
  }
  return starts[Math.max(0, lo - 1)] ?? 0;
}

const matchText = computed(() => {
  if (!search.query.trim()) return "";
  const n = matches.value.length;
  return n ? t("record.transcript.matches", { n: currentMatch.value + 1, total: n }) : t("record.transcript.noMatch");
});

function openSearch(): void {
  if (!words.value.length) return;
  search.open = true;
  void nextTick(() => {
    searchInput.value?.focus();
    searchInput.value?.select();
  });
}

function closeSearch(): void {
  search.open = false;
  search.query = "";
  search.index = 0;
}

function stepMatch(direction: 1 | -1): void {
  const n = matches.value.length;
  if (!n) return;
  search.index = (currentMatch.value + direction + n) % n;
  const m = matches.value[search.index]!;
  emit("seek", words.value[m.from]!.startMs);
  void nextTick(() => reveal(m.from));
}

function searchKey(event: KeyboardEvent): void {
  if (event.key === "Enter") stepMatch(event.shiftKey ? -1 : 1);
  else if (event.key === "Escape") {
    closeSearch();
    textEl.value?.focus({ preventScroll: true });
  } else return;
  event.preventDefault();
  event.stopPropagation();
}

watch(
  () => search.query,
  () => {
    search.index = 0;
    const m = matches.value[0];
    if (m) void nextTick(() => reveal(m.from));
  },
);

// ------------------------------------------------------------------- following the playhead

/** Scrolls the word into view when it is outside the text, a third down from the top. */
function reveal(i: number): void {
  const box = textEl.value;
  if (!box || i < 0) return;
  const el =
    box.querySelector<HTMLElement>(`[data-i="${i}"]`) ?? box.querySelector<HTMLElement>(`[data-p="${paragraphStart(breaks.value, i)}"]`);
  if (!el) return;
  const b = box.getBoundingClientRect();
  const r = el.getBoundingClientRect();
  if (r.top >= b.top + 24 && r.bottom <= b.bottom - 24) return;
  box.scrollTo({ top: box.scrollTop + (r.top - b.top) - b.height / 3, behavior: reducedMotion() ? "auto" : "smooth" });
}

// A word the user just scrolled away from is not pulled back for a few seconds.
watch(followIndex, (i) => {
  if (press || performance.now() - userScrolledAt < 3000) return;
  void nextTick(() => reveal(i));
});

function scrolled(): void {
  userScrolledAt = performance.now();
}

// --------------------------------------------------------------------------- lifecycle

watch(() => props.settings, readSettings, { immediate: true });
watch(() => props.info.result.id, () => void load(), { immediate: true });

onMounted(async () => {
  try {
    stops.push(await on<TranscriptProgress>(TRANSCRIPT_PROGRESS, onProgress));
    stops.push(await on<SpeechModelProgress>(TRANSCRIPT_MODEL_PROGRESS, onModelProgress));
  } catch (error) {
    console.warn("The text panel cannot hear the speech jobs.", error);
  }
});

onUnmounted(() => {
  window.clearTimeout(watchdog);
  window.clearTimeout(foreignTimer);
  stops.forEach((stop) => stop());
});

defineExpose({
  cutSelection,
  copySelection,
  escape,
  openSearch,
  hasSelection: () => !!selection.value,
  /** A right click on a gap of the timeline. */
  cutMenu: (cut: TrimRange, x: number, y: number) => openRestore(cut, cut.startMs, x, y),
});
</script>

<template>
  <aside ref="root" class="r-tp" :aria-label="t('record.transcript.label')">
    <div class="r-tp-head">
      <h2 class="g-section-title">{{ t("record.transcript.title") }}</h2>
      <span class="g-spacer" />
      <SelectControl
        v-if="phase.kind === 'ready' ? words.length > 0 : phase.kind === 'transcribing'"
        :key="langTurn"
        :model-value="languageValue"
        :options="languageOptions"
        :label="t('record.transcript.language')"
        :disabled="locked"
        @update:model-value="pickLanguage"
      />
    </div>

    <div v-if="search.open" class="r-tp-search">
      <label class="g-search">
        <GIcon name="search" :size="16" />
        <input
          ref="searchInput"
          v-model="search.query"
          type="search"
          :placeholder="t('record.transcript.search')"
          :aria-label="t('record.transcript.search')"
          @keydown="searchKey"
        />
      </label>
      <span class="r-tp-count" aria-live="polite">{{ matchText }}</span>
      <button type="button" class="g-btn" data-size="small" :aria-label="t('record.transcript.searchPrev')" :disabled="!matches.length" @click="stepMatch(-1)">
        <GIcon name="chevron-up" :size="16" />
      </button>
      <button type="button" class="g-btn" data-size="small" :aria-label="t('record.transcript.searchNext')" :disabled="!matches.length" @click="stepMatch(1)">
        <GIcon name="chevron-down" :size="16" />
      </button>
      <button type="button" class="g-btn" data-size="small" :aria-label="t('record.transcript.searchClose')" @click="closeSearch">
        <GIcon name="close" :size="16" />
      </button>
    </div>

    <div v-if="phase.kind === 'loading'" class="r-tp-mid" />

    <div v-else-if="phase.kind === 'noAudio'" class="r-tp-mid">
      <div class="g-empty r-tp-empty">
        <div class="g-empty-icon"><GIcon name="speaker-off" :size="32" /></div>
        <h3 class="g-empty-title">{{ t("record.transcript.noAudioTitle") }}</h3>
        <p class="g-empty-text">{{ t("record.transcript.noAudioText") }}</p>
      </div>
    </div>

    <div v-else-if="phase.kind === 'unavailable'" class="r-tp-mid">
      <div class="g-empty r-tp-empty">
        <div class="g-empty-icon"><GIcon name="microphone" :size="32" /></div>
        <h3 class="g-empty-title">{{ t("record.transcript.promptTitle") }}</h3>
        <p class="g-empty-text">{{ t("record.transcript.unavailable") }}</p>
      </div>
    </div>

    <div v-else-if="phase.kind === 'prompt' && download" class="r-tp-mid">
      <div class="g-empty r-tp-empty">
        <div class="g-empty-icon"><GIcon name="microphone" :size="32" /></div>
        <h3 class="g-empty-title">{{ t("record.transcript.downloadingTitle") }}</h3>
        <p class="g-empty-text">{{ downloadText }}</p>
        <div class="r-tp-dl">
          <div
            class="g-progress"
            role="progressbar"
            :aria-label="t('record.transcript.downloadingTitle')"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-valuenow="Math.round(downloadFraction * 100)"
            :style="{ '--progress': Math.round(downloadFraction * 100) }"
          />
        </div>
        <div v-if="download.job" class="g-empty-actions">
          <button type="button" class="g-button" @click="cancelDownload">{{ t("record.transcript.cancel") }}</button>
        </div>
      </div>
    </div>

    <div v-else-if="phase.kind === 'prompt'" class="r-tp-mid">
      <div class="g-empty r-tp-empty">
        <div class="g-empty-icon"><GIcon name="microphone" :size="32" /></div>
        <h3 class="g-empty-title">{{ t("record.transcript.promptTitle") }}</h3>
        <p class="g-empty-text">{{ tp("record.transcript.promptText", { size: megabytes(model?.bytes ?? 0) }) }}</p>
        <p v-if="phase.problem" class="g-help" data-tone="danger" role="alert"><GIcon name="bang" :size="14" />{{ phase.problem }}</p>
        <div class="g-empty-actions">
          <button type="button" class="g-button" data-variant="primary" :disabled="!model" @click="downloadModel">
            {{ t("record.transcript.download") }}
          </button>
        </div>
      </div>
    </div>

    <div v-else-if="phase.kind === 'failed'" class="r-tp-mid">
      <div class="g-empty r-tp-empty">
        <div class="g-empty-icon"><GIcon name="microphone" :size="32" /></div>
        <h3 class="g-empty-title">{{ t("record.transcript.promptTitle") }}</h3>
        <p class="g-empty-text">{{ phase.problem }}</p>
        <div class="g-empty-actions">
          <button type="button" class="g-button" @click="retry">{{ t("record.transcript.retry") }}</button>
        </div>
      </div>
    </div>

    <div v-else-if="phase.kind === 'ready' && !words.length" class="r-tp-mid">
      <div class="g-empty r-tp-empty">
        <div class="g-empty-icon"><GIcon name="microphone-off" :size="32" /></div>
        <h3 class="g-empty-title">{{ t("record.transcript.noSpeechTitle") }}</h3>
        <p class="g-empty-text">{{ t("record.transcript.noSpeechText") }}</p>
        <p v-if="longSilences.short + longSilences.cut > 0" class="g-help">{{ silenceHelp }}</p>
        <div class="g-empty-actions">
          <button v-if="longSilences.all" type="button" class="g-button" :disabled="!editable" @click="putSilencesBack">
            {{ t("record.transcript.restore") }}
          </button>
          <button v-else type="button" class="g-button" :disabled="!editable || !longSilences.total" @click="applySilences(shorten)">
            {{ t("record.transcript.shortenSilences") }}
          </button>
        </div>
      </div>
    </div>

    <template v-else>
      <div v-if="phase.kind === 'transcribing'" class="r-tp-clean">
        <div class="r-tp-job">
          <div class="r-tp-progress">
            <span>{{ t("record.transcript.transcribing") }}</span>
            <span>{{ percent(phase.totalMs ? phase.doneMs / phase.totalMs : 0) }}</span>
          </div>
          <div
            class="g-progress"
            role="progressbar"
            :aria-label="t('record.transcript.transcribing')"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-valuenow="Math.round((phase.totalMs ? phase.doneMs / phase.totalMs : 0) * 100)"
            :style="{ '--progress': Math.round((phase.totalMs ? phase.doneMs / phase.totalMs : 0) * 100) }"
          />
          <p class="g-help">{{ tp("record.transcript.transcribingText") }}</p>
        </div>
      </div>
      <template v-else>
        <div v-if="unsure" class="r-tp-note"><p class="g-help">{{ t("record.transcript.langNote") }}</p></div>
        <div v-else class="r-tp-clean">
          <div class="g-group">
            <FormRow :label="t('record.transcript.fillers')" :help="fillerHelp">
              <button v-if="fillers.all" type="button" class="g-button" :disabled="!editable" @click="putFillersBack">
                {{ t("record.transcript.restore") }}
              </button>
              <button v-else type="button" class="g-button" :disabled="!editable || !fillers.total" @click="cutFillers">
                {{ t("record.transcript.cutAll") }}
              </button>
            </FormRow>
            <FormRow :label="t('record.transcript.silences')" :help="silenceHelp">
              <button v-if="longSilences.all" type="button" class="g-button" :disabled="!editable" @click="putSilencesBack">
                {{ t("record.transcript.restore") }}
              </button>
              <button
                v-else
                type="button"
                class="g-button"
                aria-haspopup="menu"
                :aria-expanded="menu?.kind === 'shorten'"
                :data-state="menu?.kind === 'shorten' ? 'pressed' : undefined"
                :disabled="!editable || !silences.length"
                @click="openShorten"
              >
                {{ t("record.transcript.shorten") }}
              </button>
            </FormRow>
          </div>
        </div>
      </template>

      <div
        ref="textEl"
        class="r-tp-text"
        tabindex="0"
        :aria-label="t('record.transcript.text')"
        :data-busy="phase.kind === 'transcribing' ? '' : undefined"
        :data-pointer="pointerFocus ? '' : undefined"
        @blur="pointerFocus = false"
        @pointerdown="down"
        @pointermove="move"
        @pointerup="up"
        @pointercancel="cancelPress"
        @click="clicked"
        @contextmenu="context"
        @wheel.passive="scrolled"
      >
        <p
          v-for="p in paras"
          :key="p.key"
          v-memo="[p.sig, selKey(p), nowKey(p), hits.sig.get(p.key) ?? '', language, phase.kind === 'transcribing']"
          class="r-tp-para"
          :data-p="p.key"
        >
          <!-- The paragraphs settle only when the text is whole, so their times wait for it. -->
          <span v-if="phase.kind !== 'transcribing'" class="r-tp-stamp">{{ clock(p.startMs) }}</span>
          <template v-for="run in runsOf(p)" :key="run.key">
            <span class="r-trun" :data-sel="run.sel ? '' : undefined"
              ><template v-for="(it, k) in run.items" :key="it.k === 'm' ? `m${it.from}` : `${it.k}${it.i}`">
                <span
                  v-if="it.k === 'w'"
                  class="r-tw"
                  :data-i="it.i"
                  :data-f="words[it.i]!.filler && !cutFlags[it.i] ? '' : undefined"
                  :data-cut="cutFlags[it.i] ? '' : undefined"
                  :data-now="it.i === nowIndex ? '' : undefined"
                  :data-hit="hits.flags[it.i] === 2 ? 'now' : hits.flags[it.i] === 1 ? '' : undefined"
                  >{{ words[it.i]!.text }}</span
                ><span
                  v-else-if="it.k === 's'"
                  class="r-tsil"
                  :data-s="it.i"
                  :data-cut="silenceState[it.i]!.state === 'cut' ? '' : undefined"
                  :data-short="silenceState[it.i]!.state === 'short' ? '' : undefined"
                  :aria-label="t('record.transcript.silence', { length: sec(silences[it.i]!.endMs - silences[it.i]!.startMs) })"
                  >{{ silenceLabel(it.i) }}</span
                ><button v-else type="button" class="r-tmark" :data-m="`${it.from}-${it.to}`" :aria-label="markLabel(it)">
                  <GIcon name="scissors" :size="12" /></button
                >{{ run.sel && k === run.items.length - 1 ? "" : " " }}
              </template></span
            >{{ run.sel ? " " : "" }}
          </template>
        </p>
        <p v-if="phase.kind === 'transcribing'" class="r-tp-para r-tp-skel" aria-hidden="true">
          <i v-for="(w, k) in [92, 78, 86, 64, 90]" :key="k" :style="{ width: `${w}%` }" />
        </p>
      </div>

      <div v-if="phase.kind === 'ready' && (!unsure || cuts.length)" class="r-tp-foot">
        <span>{{ cuts.length ? t("record.transcript.lasts", { time: tenths(keptMs) }) : t("record.transcript.nothingCut") }}</span>
        <button v-if="cuts.length" type="button" class="g-button" data-variant="plain" :disabled="locked" @click="edit([])">
          {{ t("record.transcript.restoreAll") }}
        </button>
      </div>
    </template>

    <!-- The menu leaves the panel, so a gap's menu still opens while the panel is closed. -->
    <Teleport to="body">
      <TextMenu
        v-if="menu"
        :items="menuItems"
        :at="menu.at"
        :align="menu.kind === 'shorten' ? 'end' : 'start'"
        :label="t('record.transcript.menu')"
        @close="closeMenu"
      />
    </Teleport>
  </aside>
</template>

<style>
/* The Text panel (design `.r22-tp`): 340 points beside the video, its edge a hairline so it stays
   one device pixel on a 1x display. */
/* The design's sizes count the padding in, as the kit's own components do. */
.r-tp, .r-tp *, .r-tp *::before, .r-tp *::after { box-sizing: border-box; }
.r-tp { position: relative; display: flex; flex-direction: column; flex: none; width: 340px; min-height: 0; background: var(--win-content); box-shadow: inset calc(-1 * var(--hairline)) 0 0 var(--win-line); }
.r-tp-head { display: flex; align-items: center; gap: var(--space-8); flex: none; height: 52px; padding: 0 16px 0 20px; }
.r-tp-head > .g-section-title { margin: 0; }
.r-tp-search { display: flex; align-items: center; gap: 4px; flex: none; padding: 0 12px 12px 16px; }
.r-tp-search > .g-search { flex: 1; width: auto; min-width: 0; }
.r-tp-count { flex: none; min-width: 44px; font: var(--type-num-small); color: var(--win-text-3); text-align: right; white-space: nowrap; }
.r-tp-clean { flex: none; padding: 0 16px 14px; }
.r-tp-clean .g-form-row { padding-right: 10px; }
.r-tp-note { flex: none; padding: 0 20px 12px; }
.r-tp-note > .g-help { margin: 0; }
.r-tp-mid { flex: 1; display: grid; place-items: start center; min-height: 0; overflow: auto; padding: 36px 12px 0; }
.r-tp-empty { padding: 28px 20px; }
.r-tp-empty .g-empty-text { max-width: 280px; }
.r-tp-empty > .g-help { max-width: 280px; margin: 0; }
.r-tp-dl { width: 220px; margin: 6px 0 2px; }
.r-tp-job { padding: 2px 0 4px; }
.r-tp-job > .g-progress { margin: 8px 0; }
.r-tp-job > .g-help { margin: 0; }
.r-tp-progress { display: flex; justify-content: space-between; font: var(--type-title); color: var(--win-text); }
.r-tp-progress > span:last-child { font: var(--type-num); color: var(--win-text-2); }
.r-tp-text { position: relative; flex: 1; min-height: 0; overflow-y: auto; padding: 16px 20px 12px; outline: 0; box-shadow: inset 0 var(--hairline) 0 var(--win-line);
  -webkit-user-select: none; user-select: none; cursor: text; overscroll-behavior: contain; }
.r-tp-text:focus-visible:not([data-pointer]) { box-shadow: inset 0 var(--hairline) 0 var(--win-line), inset 0 0 0 2px var(--win-focus); }
/* A paragraph off screen skips layout and paint, so a 48-minute recording scrolls as lightly as a
   short one; its padding gives the selection's spread room inside the containment. */
.r-tp-para { position: relative; margin: 0 -3px 14px; padding: 0 3px; font: 400 13px/22px var(--font); color: var(--win-text);
  content-visibility: auto; contain-intrinsic-size: auto 110px; }
.r-tp-stamp { display: block; margin-bottom: 2px; font: var(--type-num-small); color: var(--win-text-3); }
.r-tw { border-radius: 4px; cursor: pointer; }
/* A filler wears the window's warning tone, a suggestion the user may take; a cut word stays in
   place struck through in the tertiary text colour, so what went can always be read. */
.r-tw[data-f] { padding: 1px 3px; background: var(--win-warn-soft); color: var(--win-warn); }
.r-tw[data-cut], .r-tsil[data-cut] { padding: 0; background: none; color: var(--win-text-3); text-decoration: line-through; text-decoration-thickness: 1.5px; }
.r-tw[data-hit] { background: var(--win-fill); box-shadow: 0 0 0 2px var(--win-fill); }
/* The current match wears a ring, so it never reads as one of the marked fillers. */
.r-tw[data-hit="now"] { background: var(--win-warn-soft); box-shadow: 0 0 0 1.5px var(--win-warn); color: var(--win-text); }
/* The selection is one band around its run, so a filler inside keeps its own tint. */
.r-trun[data-sel] { border-radius: 3px; background: var(--win-selection-soft); box-shadow: 0 0 0 2px var(--win-selection-soft); }
.r-tw[data-now] { border-radius: 0; box-shadow: inset 0 -2px 0 var(--iris); }
.r-tsil { display: inline-flex; align-items: center; height: 18px; padding: 0 6px; border-radius: 9px; background: var(--win-fill); color: var(--win-text-2);
  font: var(--type-num-small); white-space: nowrap; vertical-align: 1px; cursor: pointer; }
.r-tsil[data-short] { background: var(--win-selection-soft); color: var(--win-iris-text); }
.r-tmark { display: inline-grid; place-items: center; width: 22px; height: 18px; margin: 0 1px; padding: 0; border: 0; border-radius: 9px;
  background: var(--win-selection-soft); color: var(--win-iris-text); vertical-align: -3px; cursor: pointer; }
.r-tmark:focus-visible { outline: 2px solid var(--win-focus); outline-offset: 1px; }
.r-tp-skel { content-visibility: visible; }
.r-tp-skel > i { display: block; height: 10px; margin: 8px 0 12px; border-radius: 5px; background: var(--win-fill); }
.r-tp-foot { display: flex; align-items: center; gap: var(--space-8); flex: none; min-height: 48px; padding: 8px 12px 8px 20px; font: var(--type-body); color: var(--win-text-2);
  box-shadow: inset 0 var(--hairline) 0 var(--win-line); }
.r-tp-foot > span { flex: 1; min-width: 0; }
</style>
