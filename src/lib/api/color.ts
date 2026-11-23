// The colour picker: what it remembers between captures, and copying a colour or a contrast
// line. Mirrors src-tauri/src/commands/color.rs and src-tauri/src/colors.rs. The page reads the
// frame and computes every value (`lib/color.ts`); the core keeps the recent colours, writes the
// clipboard and shows the toast.

import type { DisplayId } from "./capture";
import { call } from "./core";

export type ColorFormat = "hex" | "rgb" | "hsl" | "p3";

export type ColorState = {
  /** The format a click copies in; `p3` shows as HEX on an sRGB frame. */
  format: ColorFormat;
  /** Newest first, at most eight, each `#RRGGBB` or `color(display-p3 r g b)`. */
  recent: string[];
};

export const color_state = () => call<ColorState>("color_state");

/** Puts `text` on the clipboard and shows the toast with `swatch`. With a `format` the text is one
 * colour and `swatch` joins the recent colours; without one it is a contrast line. */
export const color_copy = (text: string, swatch: string | null, format: ColorFormat | null, displayId: DisplayId | null) =>
  call<ColorState>("color_copy", { text, swatch, format, displayId });

/** Remembers the format a click copies in, for the next time the picker opens. */
export const color_set_format = (format: ColorFormat) => call<void>("color_set_format", { format });
