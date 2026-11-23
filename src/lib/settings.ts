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
  /** Whether anything may go to an upload host; off hides every Upload button and ⌘U. */
  allowUploads: boolean;
  /** No network connection at all: no upload, model download or update check. It wins over
   * `allowUploads`. */
  offline: boolean;
  /** Whether the user finished the first-run window; until then it opens at every launch. */
  firstRunDone: boolean;
  /** The sizes saved from the selection's size menu, oldest first, at most 12. An entry this
   * version cannot read stays in the list as it is (`Kept` in settings.rs), so the pages read the
   * list through `readSavedSizes` (`src/lib/sizes.ts`) and write it back whole. */
  savedSizes: unknown[];
  /** The last interval capture's plan, which the popover's form starts from. */
  interval: IntervalPlan;
};

export type IntervalTarget = "area" | "window" | "display";
export type IntervalEnd = "frames" | "minutes" | "manual";
/** An interval capture's plan (`IntervalPlan` in settings.rs): `endValue` is frames or minutes as
 * `end` says, and a manual end ignores it. A series holds at most 2000 frames and runs at most
 * 12 hours. */
export type IntervalPlan = { target: IntervalTarget; everySeconds: number; end: IntervalEnd; endValue: number };

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
  measureScreen: string | null;
  pickColor: string | null;
  /** Drawing mode and the spotlight; registered only while a take records or pauses. */
  drawOnScreen: string | null;
  spotlight: string | null;
  intervalCapture: string | null;
  /** Selects an area and goes straight into the scrolling capture; macOS only. */
  scrollCapture: string | null;
  /** The clipboard's image in the editor or pinned to the screen; no default. */
  openClipboardInEditor: string | null;
  pinClipboard: string | null;
};

export type ImageFormat = "png" | "jpeg" | "webp";
export type RetinaScale = "keep2x" | "scaleTo1x";
export type WindowBackground = "transparent" | "white";
export type HistoryRetention = "off" | "days7" | "days30" | "days90" | "forever";

export type Output = Extra & {
  saveFolder: string;
  /** Tokens: {date} {time} {app} {title} (or {window}) {counter} {width} {height} {text}. */
  fileNameTemplate: string;
  format: ImageFormat;
  /** 50 to 100, for JPEG and WebP. */
  quality: number;
  retina: RetinaScale;
  copyAfterSave: boolean;
  /** A saved PNG is compressed again (oxipng) before it is written; the pixels stay the same. */
  optimisePng: boolean;
  /** Leaves the desktop icons, notification banners and widgets out of screenshots (macOS). */
  cleanDesktop: boolean;
  windowShadow: boolean;
  windowCorners: boolean;
  /** Fills a captured window's translucent parts with the system's window colour (on by default). */
  windowBacking: boolean;
  /** Draws a thin line along a captured window's edge and rounded corners (off by default). */
  windowBorder: boolean;
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
  /** Leaves the desktop icons, notification banners and widgets out of display and area takes
   * (macOS). */
  cleanDesktop: boolean;
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
  /** A take stops and saves itself after this many minutes (1 to 240), pauses not counted;
   * `null` records until the user stops. */
  timeLimitMinutes: number | null;
  /** The language the trim window reads a recording's speech in: a model code ("tr", "en") or
   * "auto" to detect it. The Text panel's language pop-up sets it. */
  transcriptLanguage: string;
  /** A silence at least this long counts as long for "Shorten": 500, 1000 or 2000. */
  silenceThresholdMs: number;
  /** Whether "Shorten" leaves 0.3 s of a long silence (true) or cuts it whole (false). */
  silenceShorten: boolean;
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

/** Settings › Agents: the local MCP server (macOS only), off until the user turns it on. */
export type Agents = Extra & {
  /** Agents may connect and send requests, each of which the user approves. */
  enabled: boolean;
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
  agents: Agents;
};
