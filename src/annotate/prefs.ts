// What the annotation tools remember between captures: the last ink, width, size and choice of
// every tool, the crop ratio, the magnifier's factor and pixel mode, whether the redaction notice was dismissed, and whether smart
// redaction (D28) blurs or pixelates. It lives in the
// settings file as the top-level `annotate` section. settings.rs keeps a section it does not know
// and writes it back unchanged, so this needs no field on the Rust side; if Settings ever gains a
// typed `annotate` section, it must keep these names.

import { MAGNIFY_FACTORS, type Ink, type MagnifyFactor } from "../lib/annotate";
import { settings_get, settings_set } from "../lib/api/settings";
import type { Settings } from "../lib/settings";
import { CROP_RATIOS, HIGHLIGHTER_INKS, INKS, type CropRatio } from "./style";

export type ToolId = "arrow" | "line" | "rect" | "ellipse" | "pen" | "highlighter" | "text" | "step" | "magnify" | "blur" | "pixelate" | "spotlight" | "crop";

export const TOOL_IDS: readonly ToolId[] = ["arrow", "line", "rect", "ellipse", "pen", "highlighter", "text", "step", "magnify", "blur", "pixelate", "spotlight", "crop"];

/** One tool's choices. `width`, `size` and `level` are indexes into the strip's three choices;
 * `factor` and `smooth` are the magnifier's (2×, Sharp by default). */
export type ToolOptions = { ink: Ink; width: number; size: number; background: boolean; level: number; ratio: CropRatio; factor: MagnifyFactor; smooth: boolean };

/** How smart redaction hides what it found; the strength is the chosen tool's level. */
export type SensitiveMethod = "blur" | "pixelate";

export type AnnotatePrefs = { tools: Record<ToolId, ToolOptions>; redactionNoticeSeen: boolean; sensitiveMethod: SensitiveMethod };

function defaults(tool: ToolId): ToolOptions {
  return { ink: tool === "highlighter" ? "yellow" : "red", width: 1, size: 1, background: true, level: 1, ratio: "free", factor: 2, smooth: false };
}

export function defaultPrefs(): AnnotatePrefs {
  return {
    tools: Object.fromEntries(TOOL_IDS.map((t) => [t, defaults(t)])) as Record<ToolId, ToolOptions>,
    redactionNoticeSeen: false,
    sensitiveMethod: "blur",
  };
}

const index = (value: unknown, fallback: number) => (Number.isInteger(value) && (value as number) >= 0 && (value as number) <= 2 ? (value as number) : fallback);

/** Reads the section leniently: a value that no longer fits takes its default alone. */
export function readPrefs(raw: unknown): AnnotatePrefs {
  const prefs = defaultPrefs();
  if (typeof raw !== "object" || raw === null) return prefs;
  const r = raw as { tools?: Record<string, Partial<Record<keyof ToolOptions, unknown>>>; redactionNoticeSeen?: unknown; sensitiveMethod?: unknown };
  prefs.redactionNoticeSeen = r.redactionNoticeSeen === true;
  if (r.sensitiveMethod === "blur" || r.sensitiveMethod === "pixelate") prefs.sensitiveMethod = r.sensitiveMethod;
  for (const tool of TOOL_IDS) {
    const saved = r.tools?.[tool];
    if (!saved || typeof saved !== "object") continue;
    const d = prefs.tools[tool];
    const inks: readonly string[] = tool === "highlighter" ? HIGHLIGHTER_INKS : INKS;
    prefs.tools[tool] = {
      ink: typeof saved.ink === "string" && inks.includes(saved.ink) ? (saved.ink as Ink) : d.ink,
      width: index(saved.width, d.width),
      size: index(saved.size, d.size),
      background: typeof saved.background === "boolean" ? saved.background : d.background,
      level: index(saved.level, d.level),
      ratio: typeof saved.ratio === "string" && (CROP_RATIOS as readonly string[]).includes(saved.ratio) ? (saved.ratio as CropRatio) : d.ratio,
      factor: (MAGNIFY_FACTORS as readonly unknown[]).includes(saved.factor) ? (saved.factor as MagnifyFactor) : d.factor,
      smooth: typeof saved.smooth === "boolean" ? saved.smooth : d.smooth,
    };
  }
  return prefs;
}

export async function loadPrefs(): Promise<AnnotatePrefs> {
  try {
    return readPrefs(((await settings_get()) as Settings & { annotate?: unknown }).annotate);
  } catch {
    return defaultPrefs();
  }
}

let timer = 0;
let pending: AnnotatePrefs | null = null;

/** Saves the choices half a second after the last change, reading the settings fresh first so a
 * change made elsewhere in the meantime is kept. */
export function savePrefs(prefs: AnnotatePrefs): void {
  pending = structuredClone(prefs);
  clearTimeout(timer);
  timer = window.setTimeout(() => void flushPrefs(), 500);
}

export async function flushPrefs(): Promise<void> {
  clearTimeout(timer);
  const next = pending;
  pending = null;
  if (!next) return;
  try {
    const settings = await settings_get();
    await settings_set({ ...settings, annotate: next } as Settings);
  } catch (error) {
    console.warn("[glimpsfy] the annotation choices were not saved", error);
  }
}
