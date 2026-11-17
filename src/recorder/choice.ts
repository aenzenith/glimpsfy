// What the export panel holds, and how it becomes the `ExportOptions` of `record_export`: the
// format with its options, which audio the MP4 keeps, and the trim.

import type { ExportFormat, ExportOptions, ExportPurpose, Mp4Quality, RecordingInfo, Resolution, TrimRange } from "../lib/api/record";
import type { Settings } from "../lib/settings";
import { GIF_WIDTHS } from "./estimate";

export type ExportChoice = {
  format: "mp4" | "gif";
  resolution: Resolution;
  quality: Mp4Quality;
  /** The MP4's audio: each source on its own, or both together when the file holds one. */
  microphone: boolean;
  system: boolean;
  gifFps: number;
  /** 0 keeps the recording's own width. */
  gifWidth: number;
  loopForever: boolean;
};

// The same rates as Settings › Recording › GIF export. A GIF's delay is whole centiseconds, so
// 10, 15 and 20 frames per second hold their timing, where 24 would jitter between 4 and 5 cs.
export const GIF_FPS = [10, 15, 20] as const;

/** The shortest part a trim keeps. */
export const MIN_KEPT_MS = 500;

/** The trim with one end moved to `ms`, on a tenth of a second, keeping at least half a second
 * (or the whole recording when it is shorter). */
export function withEdge(range: TrimRange, edge: "start" | "end", ms: number, totalMs: number): TrimRange {
  const min = Math.min(MIN_KEPT_MS, totalMs);
  const at = Math.round(ms / 100) * 100;
  if (edge === "start") return { startMs: Math.max(0, Math.min(at, range.endMs - min)), endMs: range.endMs };
  return { startMs: range.startMs, endMs: Math.min(totalMs, Math.max(at, range.startMs + min)) };
}

/** How the file's audio shows in the panel: two switches, one "Sound" switch, or none. */
export type AudioLayout = "two" | "one" | "none";

export function audioLayout(info: RecordingInfo): AudioLayout {
  const a = info.audio;
  if (a.microphone && a.system) return "two";
  return a.microphone || a.system || a.mixed ? "one" : "none";
}

/** The GIF widths below the recording's own width; 0 is the original. */
export function gifWidths(info: RecordingInfo): number[] {
  return [...GIF_WIDTHS.filter((w) => w < info.result.size.width), 0];
}

/** The panel's first state for a recording: the format the window opened with, the GIF choices
 * of Settings › Recording when the panel offers them, and every audio source kept. */
export function initialChoice(info: RecordingInfo, settings: Settings | null): ExportChoice {
  const r = settings?.recording;
  const widths = gifWidths(info);
  // `null` in the settings keeps the original width, which the panel writes as 0.
  const wanted = r ? (r.gifWidth ?? 0) : 640;
  return {
    format: info.startFormat,
    resolution: "native",
    quality: "medium",
    microphone: true,
    system: true,
    gifFps: GIF_FPS.find((f) => f === r?.gifFps) ?? 15,
    gifWidth: widths.includes(wanted) ? wanted : widths.includes(640) ? 640 : 0,
    loopForever: true,
  };
}

export function exportFormat(choice: ExportChoice): ExportFormat {
  return choice.format === "gif"
    ? { kind: "gif", fps: choice.gifFps, width: choice.gifWidth || null, loopForever: choice.loopForever }
    : { kind: "mp4", resolution: choice.resolution, quality: choice.quality };
}

/** The audio flags the export keeps; with one source in the file, one switch drives both. */
export function exportAudio(choice: ExportChoice, info: RecordingInfo): { microphone: boolean; system: boolean } {
  if (choice.format === "gif") return { microphone: false, system: false };
  const layout = audioLayout(info);
  if (layout === "none") return { microphone: false, system: false };
  if (layout === "one") {
    const sound = choice.microphone || choice.system;
    return { microphone: sound, system: sound };
  }
  return { microphone: choice.microphone, system: choice.system };
}

/** How many audio sources the MP4 keeps, for the estimate. */
export function keptSources(choice: ExportChoice, info: RecordingInfo): number {
  const audio = exportAudio(choice, info);
  if (audioLayout(info) === "one") return audio.microphone ? 1 : 0;
  return Number(audio.microphone) + Number(audio.system);
}

/** `null` when the whole recording is kept. */
export function trimOf(range: TrimRange, totalMs: number): TrimRange | null {
  return range.startMs <= 0 && range.endMs >= totalMs ? null : { startMs: range.startMs, endMs: range.endMs };
}

export function exportOptions(choice: ExportChoice, info: RecordingInfo, range: TrimRange, purpose: ExportPurpose): ExportOptions {
  return {
    format: exportFormat(choice),
    trim: trimOf(range, info.result.durationMs),
    destination: null,
    audio: exportAudio(choice, info),
    purpose,
  };
}
