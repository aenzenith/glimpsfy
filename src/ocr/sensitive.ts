// Smart redaction (D28): when the user asks for it (D), the area's text is read, the email
// addresses, phone numbers, cards, IBANs, keys, tokens and link passwords in it are found on the
// machine (`ocr_detect_entities`), and the ones the user keeps switched on are redacted in one
// undo step. Nothing is read before the user asks, and what is found is only a suggestion: the
// list never calls an area safe.
//
// The text comes from the page's shared recognition (`TextLayerContext.recognition`), which reads
// `pixels()`, the area with every blur and pixelate already flattened in, so a value that is
// redacted already is never found again. The found boxes are placed in the document's pixels by
// `redactionRects`, which errs on the side of covering more: a wrong placement would export the
// secret it was meant to hide.

import { computed, ref, shallowRef, watch } from "vue";

import type { AnnotationEngine } from "../annotate/engine";
import type { PixelRect } from "../annotate/redact";
import type { TextLayerContext } from "../annotate/textLayer";
import { ocr_detect_entities, type Entity, type EntityKind, type OcrResult } from "../lib/api/ocr";
import { ppOcrWithoutModels } from "./models";

/** The kinds the list offers, in the order its groups take. Names and addresses are not looked
 * for: they would turn every capture's list into noise. */
export const SENSITIVE_KINDS: readonly EntityKind[] = ["email", "phone", "card", "iban", "apiKey", "token", "urlPassword"];

/** A list of more items than this is grouped by kind. */
export const GROUP_AFTER = 12;
/** An opened group shows this many items before its "N more" button. */
export const GROUP_ROWS = 3;

/** Room around a found box, in points: the recognised box hugs the glyphs, and anti-aliasing
 * reaches past it. */
const PAD = 3;

export type Method = "blur" | "pixelate";

/** A side of a box that the area's edge cuts. */
export type Edge = "left" | "right" | "top" | "bottom";

/** One box of a found item in the document's pixels, inside the area. */
export type Mark = { rect: PixelRect; cut: Edge[] };

export type SensitiveItem = { id: string; kind: EntityKind; text: string; marks: Mark[]; cut: boolean };

/** What the list shows: reading, a reading that could not happen, or the items. */
export type SensitivePhase = "reading" | "done" | "failed" | "unavailable" | "missing";

type Word = OcrResult["lines"][number]["words"][number];

/** A glyph's advance in a proportional font, in ems: Helvetica's widths, rounded by the glyph's
 * shape. Only the ratios between glyphs matter. */
function advance(c: string): number {
  if (c === " ") return 0.28;
  if (c === "@") return 1;
  if (/[mwMW%]/.test(c)) return 0.85;
  if (/[iljI.,:;'|!`/\\tf]/.test(c)) return 0.27;
  if (/[r()[\]{}"*^-]/.test(c)) return 0.33;
  if (/[A-Z]/.test(c)) return 0.68;
  return 0.55;
}

/** How a reading's glyphs are spaced: `even` in a monospaced font, `shaped` in a proportional
 * one, `unknown` when its lines do not tell (fewer than three, or several sizes of text). */
type Spacing = "even" | "shaped" | "unknown";

/** Each line's width is divided by its count of characters and by the sum of its glyphs' rough
 * advances; whichever ratio holds clearly steadier across the lines names the font. */
function spacing(result: OcrResult): Spacing {
  const lines = result.lines.filter((l) => [...l.text].length >= 6 && l.bounds.width > 0);
  if (lines.length < 3) return "unknown";
  const spread = (values: number[]) => {
    const mean = values.reduce((a, v) => a + v, 0) / values.length;
    return Math.sqrt(values.reduce((a, v) => a + (v - mean) ** 2, 0) / values.length) / mean;
  };
  const even = spread(lines.map((l) => l.bounds.width / [...l.text].length));
  const shaped = spread(lines.map((l) => l.bounds.width / [...l.text].reduce((a, c) => a + advance(c), 0)));
  if (even < shaped * 0.75) return "even";
  if (shaped < even * 0.75) return "shaped";
  return "unknown";
}

/** Where a boundary the core placed at `at` inside `word` lies in the fonts the reading may be set
 * in. The core splits a word's box evenly by its characters (`spans.rs`), which is right for a
 * monospaced font only: in a proportional one, a wide `m` or `@` puts the glyphs after it further
 * on, by most of a glyph over a long link. */
function boundary(word: Word, at: number, spaced: Spacing): number[] {
  const chars = [...word.text];
  const before = Math.round(((at - word.bounds.x) / word.bounds.width) * chars.length);
  const widths = chars.map(advance);
  const total = widths.reduce((a, w) => a + w, 0);
  const shaped = word.bounds.x + (word.bounds.width * widths.slice(0, before).reduce((a, w) => a + w, 0)) / total;
  return spaced === "even" ? [at] : spaced === "shaped" ? [shaped] : [at, shaped];
}

/**
 * Where a found piece goes in the document: one rectangle per box, in document pixels, padded by
 * 3 points and kept inside the area. Where a box starts or ends inside a word (the password inside
 * a link, a card after `CARD=`), that end was cut out of the word's box by its share of the
 * characters, so it is placed again by the font the reading's lines fit (both places when they
 * fit neither) and reaches half an average character further, for what the rough glyph widths
 * miss. A box whose padding meets an edge of the area is cut by it, and the list says so: a glyph
 * halved by the edge leaves the reading's box a pixel or two short of it, never on it.
 *
 * @param area the area the text was read from, in document pixels
 * @param scale document pixels per point
 */
export function redactionRects(entity: Entity, result: OcrResult, area: PixelRect, scale: number): Mark[] {
  const sx = area.width / Math.max(1, result.imageSize.width);
  const sy = area.height / Math.max(1, result.imageSize.height);
  const pad = PAD * scale;
  const right = area.x + area.width;
  const bottom = area.y + area.height;
  const spaced = spacing(result);
  const inside = (w: Word, x: number) => x - w.bounds.x > 0.5 && w.bounds.x + w.bounds.width - x > 0.5;
  const half = (w: Word) => w.bounds.width / Math.max(1, [...w.text].length) / 2;
  const marks: Mark[] = [];
  for (const { line, bounds: b } of entity.boxes) {
    let x0 = b.x;
    let x1 = b.x + b.width;
    const words = result.lines[line]?.words ?? [];
    const first = words.find((w) => inside(w, b.x));
    if (first) x0 = Math.max(first.bounds.x, Math.min(...boundary(first, b.x, spaced)) - half(first));
    const last = words.find((w) => inside(w, b.x + b.width));
    if (last) x1 = Math.min(last.bounds.x + last.bounds.width, Math.max(...boundary(last, b.x + b.width, spaced)) + half(last));
    const box = { x0: area.x + x0 * sx, y0: area.y + b.y * sy, x1: area.x + x1 * sx, y1: area.y + (b.y + b.height) * sy };
    const cut: Edge[] = [];
    if (box.x0 - area.x < pad) cut.push("left");
    if (right - box.x1 < pad) cut.push("right");
    if (box.y0 - area.y < pad) cut.push("top");
    if (bottom - box.y1 < pad) cut.push("bottom");
    // Whole pixels, rounded outwards, so no fraction of a glyph stays outside.
    const x = Math.max(area.x, Math.floor(box.x0 - pad));
    const y = Math.max(area.y, Math.floor(box.y0 - pad));
    const w = Math.min(right, Math.ceil(box.x1 + pad)) - x;
    const h = Math.min(bottom, Math.ceil(box.y1 + pad)) - y;
    if (w > 0 && h > 0) marks.push({ rect: { x, y, width: w, height: h }, cut });
  }
  return marks;
}

/** A row of the list: one item, or a group of every item of a kind. */
export type SensitiveRow =
  | { type: "item"; item: SensitiveItem }
  | { type: "group"; kind: EntityKind; items: SensitiveItem[]; open: boolean; shown: SensitiveItem[]; more: number };

/** Waits until the recognition is no longer reading. */
function settled(recognition: TextLayerContext["recognition"]): Promise<void> {
  if (recognition.status.value !== "reading") return Promise.resolve();
  return new Promise((resolve) => {
    const stop = watch(recognition.status, (status) => {
      if (status === "reading") return;
      stop();
      resolve();
    });
  });
}

const areaKey = (r: PixelRect | null | undefined) => (r ? `${r.x},${r.y},${r.width},${r.height}` : "");

export function useSensitive(context: TextLayerContext, engine: AnnotationEngine) {
  const { recognition } = context;
  const open = ref(false);
  const phase = ref<SensitivePhase>("reading");
  const items = shallowRef<SensitiveItem[]>([]);
  /** The items switched off, which Redact leaves alone. */
  const off = ref(new Set<string>());
  /** The item under the pointer in the list, which the image highlights. */
  const hovered = ref<string | null>(null);
  /** The groups opened, and those whose every item shows. */
  const opened = ref(new Set<EntityKind>());
  const whole = ref(new Set<EntityKind>());
  let generation = 0;
  let pending = 0;

  const method = computed<Method>(() => (engine.rev.value, engine.prefs.sensitiveMethod));
  const chosen = computed(() => items.value.filter((i) => !off.value.has(i.id)));
  const cutCount = computed(() => items.value.filter((i) => i.cut).length);

  const rows = computed<SensitiveRow[]>(() => {
    const list = items.value;
    if (list.length <= GROUP_AFTER) return list.map((item) => ({ type: "item", item }));
    return SENSITIVE_KINDS.flatMap((kind): SensitiveRow[] => {
      const of = list.filter((i) => i.kind === kind);
      if (!of.length) return [];
      const isOpen = opened.value.has(kind);
      const shown = !isOpen ? [] : whole.value.has(kind) ? of : of.slice(0, GROUP_ROWS);
      return [{ type: "group", kind, items: of, open: isOpen, shown, more: isOpen ? of.length - shown.length : 0 }];
    });
  });

  /** Reads the area and finds what to suggest. */
  async function scan(): Promise<void> {
    const mine = ++generation;
    const area = context.area.value ? { ...context.area.value } : null;
    phase.value = "reading";
    items.value = [];
    hovered.value = null;
    if (!area) return;
    const pixels = context.pixels();
    if (!pixels) return;
    // The same pixels are not read twice: a reading the text layer or a snapping tool made of
    // this area is used as it is.
    void recognition.run({ ...pixels, colorSpace: context.colorSpace.value === "display-p3" ? "display-p3" : "srgb" });
    await settled(recognition);
    if (mine !== generation || !open.value) return;
    if (recognition.status.value === "failed") {
      const missing = recognition.problem.value === "unavailable" && (await ppOcrWithoutModels());
      if (mine !== generation) return;
      phase.value = missing ? "missing" : recognition.problem.value === "unavailable" ? "unavailable" : "failed";
      return;
    }
    const result = recognition.result.value;
    if (!result || !result.lines.length) {
      phase.value = "done";
      return;
    }
    let found: Entity[];
    try {
      found = await ocr_detect_entities(result, [...SENSITIVE_KINDS]);
    } catch (error) {
      console.warn("[glimpsfy] the sensitive information could not be looked for", error);
      if (mine === generation) phase.value = "failed";
      return;
    }
    if (mine !== generation || !open.value) return;
    items.value = found.flatMap((entity, i) => {
      const marks = redactionRects(entity, result, area, context.scale.value);
      return marks.length ? [{ id: `s${mine}-${i}`, kind: entity.kind, text: entity.text, marks, cut: marks.some((m) => m.cut.length > 0) }] : [];
    });
    off.value = new Set();
    opened.value = new Set();
    whole.value = new Set();
    phase.value = "done";
  }

  function start(): void {
    open.value = true;
    void scan();
  }

  function close(): void {
    open.value = false;
    generation += 1;
    clearTimeout(pending);
    items.value = [];
    hovered.value = null;
  }

  function toggle(id: string): void {
    const next = new Set(off.value);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    off.value = next;
  }

  /** A group's switch turns every item of it on, or off when all are on. */
  function toggleGroup(kind: EntityKind): void {
    const of = items.value.filter((i) => i.kind === kind);
    const allOn = of.every((i) => !off.value.has(i.id));
    const next = new Set(off.value);
    for (const i of of) {
      if (allOn) next.add(i.id);
      else next.delete(i.id);
    }
    off.value = next;
  }

  function expand(kind: EntityKind): void {
    const next = new Set(opened.value);
    if (next.has(kind)) next.delete(kind);
    else next.add(kind);
    opened.value = next;
  }

  function showAll(kind: EntityKind): void {
    whole.value = new Set(whole.value).add(kind);
  }

  function setMethod(next: Method): void {
    if (engine.prefs.sensitiveMethod === next) return;
    engine.prefs = { ...engine.prefs, sensitiveMethod: next };
    engine.onPrefs(engine.prefs);
    engine.rev.value += 1;
  }

  /** Redacts the items switched on as one undo step and closes the list; the method's tool is
   * what the page picks next, so its strength shows. `null` when nothing was switched on. */
  function apply(): Method | null {
    const rects = chosen.value.flatMap((i) => i.marks.map((m) => m.rect));
    if (!rects.length) return null;
    const kind = method.value;
    engine.addRedactions(rects, kind);
    close();
    return kind;
  }

  // A moved or resized area is read again once it rests; the marks belong to the old one.
  watch(
    () => areaKey(context.area.value),
    (key, before) => {
      if (!open.value || key === before) return;
      clearTimeout(pending);
      if (!key) return close();
      phase.value = "reading";
      items.value = [];
      pending = window.setTimeout(() => void scan(), 120);
    },
  );

  return { open, phase, items, rows, off, hovered, chosen, cutCount, method, start, close, toggle, toggleGroup, expand, showAll, setMethod, apply };
}

export type Sensitive = ReturnType<typeof useSensitive>;
