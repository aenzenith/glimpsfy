// Cutting a recording by its text: the speech model, the transcript of a recording and the cuts
// kept with it. Mirrors src-tauri/src/commands/transcript.rs and crates/speech/src/transcript.rs.
//
// The speech runs on this machine. The model is downloaded once, and only when the user presses
// a Download button (the trim window's panel or Settings › Recording). A transcript is kept next
// to the app's data with the recording's fingerprint, so a recording opened again is not
// transcribed again; cuts never change the recording, they are applied to the exported file only.

import { call } from "./core";
import type { RecordingId, TrimRange } from "./record";

/** A model download made progress. Payload: `SpeechModelProgress`. */
export const TRANSCRIPT_MODEL_PROGRESS = "transcript://model-progress";
/** A transcription made progress, with the words found since the last event. Payload: `TranscriptProgress`. */
export const TRANSCRIPT_PROGRESS = "transcript://progress";

/** The speech model of the reviewed table and where it stands on this machine. */
export type SpeechModelInfo = {
  id: string;
  /** A product name, shown as it is. */
  title: string;
  /** The download's size in bytes. */
  bytes: number;
  installed: boolean;
  /** The folder that holds the model, for Settings › Recording. */
  folder: string;
};

export type SpeechModelProgress = { received: number; total: number; remainingMs: number | null };

/** `sound` is a hesitation sound (ııı, eee, um); `word` is a filler word (şey, yani, hani, like). */
export type FillerKind = "sound" | "word";

export type TranscriptWord = {
  /** The word as spoken, with the punctuation the model gave it. */
  text: string;
  startMs: number;
  endMs: number;
  /** Set when the word is a filler that "Cut all" removes. */
  filler?: FillerKind;
  /** The model's confidence, from 0 to 1. */
  probability: number;
};

/** A stretch without speech, found from the sound's level, at least half a second long. */
export type TranscriptSilence = { startMs: number; endMs: number };

export type TranscriptFingerprint = { bytes: number; durationMs: number; modifiedMs: number };

export type Transcript = {
  version: number;
  fingerprint: TranscriptFingerprint;
  /** The language the speech was read in, as the model's code ("tr", "en"). */
  language: string;
  /** Whether the language was detected rather than chosen. */
  languageAuto: boolean;
  /** How sure the detection was, from 0 to 1, when the language was detected. */
  languageProbability: number | null;
  model: string;
  durationMs: number;
  words: TranscriptWord[];
  silences: TranscriptSilence[];
  /** The removed parts of the recording, sorted and apart; the export skips them. */
  cuts: TrimRange[];
  createdAtMs: number;
};

export type TranscriptProgress = {
  recordingId: RecordingId;
  /** How far the speech is read, in milliseconds of the recording. */
  doneMs: number;
  totalMs: number;
  remainingMs: number | null;
  /** The words read since the last event, in order. */
  words: TranscriptWord[];
};

/** What the trim window shows for a recording: the saved transcript, or the job that is reading it. */
export type TranscriptState = {
  transcript: Transcript | null;
  /** Set while this recording is being transcribed. */
  running: TranscriptProgress | null;
};

/** `"auto"` lets the model detect the language. */
export type TranscriptLanguage = "auto" | string;

/** The speech model and whether it is downloaded. */
export function transcript_model(): Promise<SpeechModelInfo> {
  return call("transcript_model");
}

/** Downloads the speech model after the user asked for it; progress arrives as `transcript://model-progress`. Cancel with `transcript_cancel(job)`. */
export function transcript_model_download(job: string | null = null): Promise<SpeechModelInfo> {
  return call("transcript_model_download", { job });
}

/** Deletes the downloaded speech model. */
export function transcript_model_remove(): Promise<void> {
  return call("transcript_model_remove");
}

/** The saved transcript of a recording (when its fingerprint still matches) and the running job. */
export function transcript_get(recordingId: RecordingId): Promise<TranscriptState> {
  return call("transcript_get", { recordingId });
}

/** Transcribes a recording on this machine and saves the transcript; a second job while one runs fails with `busy`. */
export function transcript_start(recordingId: RecordingId, language: TranscriptLanguage): Promise<Transcript> {
  return call("transcript_start", { recordingId, language });
}

/** Stops the transcription or the model download started under `job`, or every transcription when `job` is null. */
export function transcript_cancel(job: string | null = null): Promise<void> {
  return call("transcript_cancel", { job });
}

/** Saves the cuts with the transcript, so the recording opens with them again. */
export function transcript_save_cuts(recordingId: RecordingId, cuts: TrimRange[]): Promise<void> {
  return call("transcript_save_cuts", { recordingId, cuts });
}
