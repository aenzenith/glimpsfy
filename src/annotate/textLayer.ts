// The mount point for the text recognition layer (`src/ocr/TextLayer.vue`, owned by the text
// recognition area). The overlay and the editor each render the component, when it exists, inside
// a positioned box that covers their surface, and provide this context to it with `TEXT_LAYER`.
//
// The layer reads pixels only through `pixels()`, which returns the area with every blur and
// pixelate already flattened in, so a redacted region can never be recognised.

import { defineAsyncComponent, type Component, type InjectionKey, type Ref } from "vue";

import type { PixelRect, Pixels } from "./redact";

/** Screen position of the image: CSS pixels of the mount box per image pixel, and where the
 * image's origin sits in the box. */
export type View = { k: number; ox: number; oy: number };

export type TextLayerContext = {
  /** The part of the image the layer covers (the selection in the overlay, the crop or the whole
   * image in the editor), in image pixels; frame pixels in the overlay. `null` while there is none. */
  area: Readonly<Ref<PixelRect | null>>;
  view: Readonly<Ref<View>>;
  /** The pixels of `area` with the redactions flattened in, never the originals under them. */
  pixels: () => Pixels | null;
  colorSpace: Readonly<Ref<PredefinedColorSpace>>;
  /** Image pixels per point (2 for a Retina capture). */
  scale: Readonly<Ref<number>>;
  /** Whether the layer should show: Copy text (⇧⌘C) toggles it, Esc hides it. */
  visible: Ref<boolean>;
};

export const TEXT_LAYER: InjectionKey<TextLayerContext> = Symbol("glimpsfy.textLayer");

// The component may not exist yet; a glob finds it when it does, without a failing import.
const found = import.meta.glob<{ default: Component }>("../ocr/TextLayer.vue");
const loader = Object.values(found)[0];

/** The text recognition layer's component, or `null` until the text recognition area adds it. */
export const textLayerComponent: Component | null = loader ? defineAsyncComponent(loader) : null;
