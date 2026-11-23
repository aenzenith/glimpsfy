// The size estimates: how large the trim window's export will be for the chosen format and trim,
// and how much a take writes before its time limit. It is a pure model with named constants, so
// the recording engineer can calibrate them against real exports without touching the page.

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

/** Bits per pixel and frame a take is written with: the writers' target (`bit_rate` in
 * `crates/record/src/platform/*`), the same on every platform. A still screen stays below it, so
 * the take estimate is an upper bound. */
export const TAKE_BPP = { h264: 0.08, hevc: 0.05 } as const;
/** The writers keep a take's video bit rate between these, in bits per second. */
export const TAKE_MIN_BITS = 2_000_000;
export const TAKE_MAX_BITS = 60_000_000;
/** A take's audio, in bytes per second: the mix at 192 kbit/s and each source on its own track
 * at 160 kbit/s, as the writers encode them. */
export const TAKE_AUDIO_BYTES_PER_SECOND = { mix: 24_000, source: 20_000 } as const;

export type TakeInput = {
  size: PhysicalSize;
  fps: number;
  codec: "h264" | "hevc";
  /** How many audio sources the take records (the microphone, the system audio). */
  audioSources: number;
  /** Each source on its own track besides the mix (Settings › Recording). */
  separateTracks: boolean;
};

/** H.264 stops at 4096 × 2304 worth of macroblocks, so a larger take is written as HEVC
 * (`codec_for` in `crates/record/src/session.rs`). */
function takeCodec(input: TakeInput): "h264" | "hevc" {
  const blocks = Math.ceil(input.size.width / 16) * Math.ceil(input.size.height / 16);
  const tooLarge = blocks > 36_864 || input.size.width > 4096 || input.size.height > 4096;
  return input.codec === "h264" && tooLarge ? "hevc" : input.codec;
}

/** About how many bytes a take writes in `ms` of recording, for the time limit's disk warning. */
export function estimateTake(input: TakeInput, ms: number): number {
  const bits = input.size.width * input.size.height * Math.max(1, input.fps) * TAKE_BPP[takeCodec(input)];
  const video = Math.min(TAKE_MAX_BITS, Math.max(TAKE_MIN_BITS, bits)) / 8;
  const sources = Math.max(0, input.audioSources);
  const audio =
    sources === 0
      ? 0
      : sources === 1
        ? TAKE_AUDIO_BYTES_PER_SECOND.source
        : TAKE_AUDIO_BYTES_PER_SECOND.mix + (input.separateTracks ? sources * TAKE_AUDIO_BYTES_PER_SECOND.source : 0);
  return ((video + audio) * Math.max(0, ms)) / 1000;
}
