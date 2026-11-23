// The settings object, mirroring src-tauri/src/settings.rs field for field. A field added there is
// added here too. Unknown fields a newer Glimpsfy wrote travel along untouched, which is why every
// section allows extra keys.

type Extra = { [key: string]: unknown };

export type Language = "system" | "en" | "tr" | "de" | "es" | "ru" | "ja" | "zh-Hans";
export type Appearance = "system" | "light" | "dark";
export type IconClick = "openMenu" | "captureArea";
export type AfterSelection = "annotate" | "copyAndClose" | "saveAndClose";

export type General = Extra & {
  language: Language;
  appearance: Appearance;
  startAtLogin: boolean;
  showMenuBarIcon: boolean;
  iconClick: IconClick;
  afterSelection: AfterSelection;
  includePointer: boolean;
  /** 3, 5 or 10 seconds. */
  delaySeconds: number;
  shutterSound: boolean;
  checkForUpdates: boolean;
  /** Whether the user finished the first-run window; until then it opens at every launch. */
  firstRunDone: boolean;
  /** The sizes saved from the selection's size menu, oldest first, at most 12. An entry this
   * version cannot read stays in the list as it is (`Kept` in settings.rs), so the pages read the
   * list through `readSavedSizes` (`src/lib/sizes.ts`) and write it back whole. */
  savedSizes: unknown[];
};

/** A size saved from the size menu, in frame pixels, as the size label counts them. */
export type SavedSize = Extra & { name: string; width: number; height: number };

/** Accelerators the global-shortcut plugin parses, such as "Shift+CommandOrControl+2"; `null` has no key. */
export type Shortcuts = Extra & {
  captureArea: string | null;
  captureWindow: string | null;
  captureDisplay: string | null;
  captureAllDisplays: string | null;
  captureAreaDelayed: string | null;
  repeatLastArea: string | null;
  copyText: string | null;
  recordScreen: string | null;
  recordDisplay: string | null;
  pauseRecording: string | null;
  muteMicrophone: string | null;
  discardRecording: string | null;
  openHistory: string | null;
  /** Drawing mode and the spotlight; registered only while a take records or pauses. */
  drawOnScreen: string | null;
  spotlight: string | null;
};

export type ImageFormat = "png" | "jpeg" | "webp";
export type RetinaScale = "keep2x" | "scaleTo1x";
export type WindowBackground = "transparent" | "white";
export type HistoryRetention = "off" | "days7" | "days30" | "days90" | "forever";

export type Output = Extra & {
  saveFolder: string;
  /** Tokens: {date} {time} {app} {counter} {width} {height}. */
  fileNameTemplate: string;
  format: ImageFormat;
  /** 50 to 100, for JPEG and WebP. */
  quality: number;
  retina: RetinaScale;
  copyAfterSave: boolean;
  windowShadow: boolean;
  windowCorners: boolean;
  windowBackground: WindowBackground;
  historyRetention: HistoryRetention;
  /** The retention sweep also moves an expired entry's saved file to the Trash. */
  trashSavedFiles: boolean;
  /** The apps of Send to app, in the order of the send list, whose digits 1–9 follow it. */
  sendTargets: SendTarget[];
};

/** What a send target gets on the clipboard before Glimpsfy presses ⌘V in it: the image, the
 * file's path as text, or the file itself. A recording goes as its file to an image target. */
export type SendPaste = "image" | "path" | "file";

/** One app of Send to app; its bundle identifier is also its id. */
export type SendTarget = { bundleId: string; name: string; paste: SendPaste };

/** Settings › Beautify (`Beautify` in settings.rs). */
export type Beautify = Extra & {
  /** Every screenshot gets `preset` unless F skips it for that capture. */
  auto: boolean;
  /** The id of the preset `auto` applies, and F turns on when `auto` is off. */
  preset: string;
  /** The presets in the order Settings lists them. An entry this version cannot read stays in the
   * list as it is (`Kept` in settings.rs), so the pages read the list through `readPresets`
   * (`src/annotate/beautify.ts`) and write it back whole. */
  presets: unknown[];
};

export type VideoCodec = "h264" | "hevc";
export type RecordingResolution = "native" | "half";
export type RingColor = "yellow" | "red" | "blue" | "white";
export type Size2 = "small" | "large";
export type Size3 = "small" | "medium" | "large";
export type KeysPosition = "bottomCentre" | "bottomLeft" | "bottomRight" | "topCentre";
export type WebcamShape = "circle" | "roundedSquare";
export type Corner = "bottomRight" | "bottomLeft" | "topRight" | "topLeft";

export type Recording = Extra & {
  codec: VideoCodec;
  /** 30 or 60. */
  fps: number;
  resolution: RecordingResolution;
  systemAudio: boolean;
  microphone: boolean;
  microphoneDevice: string | null;
  separateTracks: boolean;
  /** 0 is off; otherwise 3 or 5. */
  countdownSeconds: number;
  highlightClicks: boolean;
  clickRingColor: RingColor;
  clickRingSize: Size2;
  showKeys: boolean;
  keysPosition: KeysPosition;
  keysSize: Size3;
  keysOnlyShortcuts: boolean;
  webcamDevice: string | null;
  webcamShape: WebcamShape;
  webcamSize: Size3;
  webcamCorner: Corner;
  recordingsFolder: string;
  /** 10, 15 or 20. */
  gifFps: number;
  /** `null` keeps the original width. */
  gifWidth: number | null;
  /** Whether drawings on a take fade 3 s after the last stroke; the Presentation card sets it. */
  inkFades: boolean;
};

export type OcrEngineChoice = "system" | "ppOcr";

export type Ocr = Extra & {
  engine: OcrEngineChoice;
  detectLanguage: boolean;
  /** BCP 47 codes such as "en" and "tr". */
  languages: string[];
  keepLineBreaks: boolean;
  detectCodes: boolean;
};

export type CopyAfterUpload = "link" | "markdown" | "nothing";

export type Upload = Extra & {
  defaultProvider: string | null;
  copyAfterUpload: CopyAfterUpload;
  openInBrowser: boolean;
  confirmedProviders: string[];
  /** Each provider's non-secret settings by provider id. Secrets live in the keychain. */
  providers: Record<string, unknown>;
};

export type Settings = Extra & {
  version: number;
  general: General;
  shortcuts: Shortcuts;
  output: Output;
  beautify: Beautify;
  recording: Recording;
  ocr: Ocr;
  upload: Upload;
};
