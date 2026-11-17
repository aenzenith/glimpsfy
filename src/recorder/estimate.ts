// The trim window's size estimate: how large the exported file will be for the chosen format and
// trim. It is a pure model with named constants, so the recording engineer can calibrate them
// against real exports without touching the page.

import type { PhysicalSize } from "../lib/api/capture";
import type { ExportFormat, Mp4Quality } from "../lib/api/record";

/** Bits per pixel and frame of an MP4 re-encode, by quality: the encoder's target in
 * `crates/record/src/export.rs` (`bits_per_pixel`). The encoder reaches it on busy screens and
 * stays below it on still ones, so the estimate is an upper bound there. */
export const BPP: Readonly<Record<Mp4Quality, number>> = { high: 0.1, medium: 0.06, low: 0.035 };

/** A re-encode budgets for at least this many frames per second, as the core does, because a
 * still screen lowers a recording's nominal rate. */
export const MIN_ENCODE_FPS = 30;

/** The bytes per second of the one audio track an export keeps: the mix (192 kbit/s AAC) when
 * both sources are kept, a source's own track (160 kbit/s) when one is. */
export const AUDIO_BYTES_PER_SECOND = { one: 20_000, both: 24_000 } as const;

/** The bytes a GIF spends per pixel and frame. Measured: 0.032 on a moving shape over a
 * gradient and 0.006 on a still screen; busy screens with text land higher, so the estimate
 * keeps a margin for the size warning. */
export const GIF_BYTES_PER_PIXEL_FRAME = 0.045;

/** Above this a GIF earns the warning that chat apps may refuse it. */
export const GIF_WARN_BYTES = 10_000_000;

/** The GIF widths the panel offers; only those narrower than the recording are listed. */
export const GIF_WIDTHS = [480, 640, 800, 1080] as const;

export type EstimateInput = {
  format: ExportFormat;
  /** The kept part and the whole recording, in milliseconds. */
  keptMs: number;
  totalMs: number;
  /** The recording's own pixel size, frame rate and file size. */
  size: PhysicalSize;
  fps: number;
  fileBytes: number;
  /** How many audio sources the MP4 keeps (0 for a GIF). */
  audioSources: number;
};

export type Estimate = {
  bytes: number;
  /** The exported file's pixel size and frame rate. */
  width: number;
  height: number;
  fps: number;
  /** True when the MP4 keeps the original video stream and only cuts it. */
  passThrough: boolean;
};

/** Half the size, each side kept even, because the encoders need even sides. */
export function halfSize(size: PhysicalSize): PhysicalSize {
  return { width: Math.max(2, 2 * Math.round(size.width / 4)), height: Math.max(2, 2 * Math.round(size.height / 4)) };
}

/** A GIF of `width` keeps the recording's aspect ratio with an even height. */
export function gifSize(size: PhysicalSize, width: number | null): PhysicalSize {
  if (width === null || width >= size.width) return { width: size.width, height: size.height };
  return { width, height: Math.max(2, 2 * Math.round((width * size.height) / size.width / 2)) };
}

export function estimateExport(input: EstimateInput): Estimate {
  const seconds = Math.max(0, input.keptMs) / 1000;
  const format = input.format;
  if (format.kind === "gif") {
    const out = gifSize(input.size, format.width);
    return {
      bytes: out.width * out.height * format.fps * seconds * GIF_BYTES_PER_PIXEL_FRAME,
      width: out.width,
      height: out.height,
      fps: format.fps,
      passThrough: false,
    };
  }
  const out = format.resolution === "half" ? halfSize(input.size) : input.size;
  if (format.resolution === "native" && format.quality === "high") {
    const share = input.totalMs > 0 ? Math.min(1, Math.max(0, input.keptMs) / input.totalMs) : 0;
    return { bytes: input.fileBytes * share, width: out.width, height: out.height, fps: input.fps, passThrough: true };
  }
  const video = (out.width * out.height * Math.max(input.fps, MIN_ENCODE_FPS) * BPP[format.quality]) / 8;
  const audio =
    input.audioSources <= 0 ? 0 : input.audioSources === 1 ? AUDIO_BYTES_PER_SECOND.one : AUDIO_BYTES_PER_SECOND.both;
  return { bytes: (video + audio) * seconds, width: out.width, height: out.height, fps: input.fps, passThrough: false };
}
