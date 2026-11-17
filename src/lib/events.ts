// Event names, mirroring src-tauri/src/events.rs. A name changes in both files or in neither.
// Events that one area owns alone (upload, history, record) are declared in that area's wrapper
// in `src/lib/api/`.

export const CAPTURE_STARTED = "capture://started";
export const CAPTURE_CLOSED = "capture://closed";
/** Sent by an overlay page to the others and the core when a selection starts on its display. */
export const OVERLAY_SELECTION_STARTED = "overlay://selection-started";
/** Sent by an overlay page to the others and the core when its display's selection is gone. */
export const OVERLAY_SELECTION_ENDED = "overlay://selection-ended";

export const OUTPUT_COPIED = "output://copied";
export const OUTPUT_SAVED = "output://saved";
export const OUTPUT_FAILED = "output://failed";

export const TOAST_SHOW = "toast://show";

export const PERMISSIONS_CHANGED = "permissions://changed";
export const SETTINGS_CHANGED = "settings://changed";
export const DISPLAYS_CHANGED = "displays://changed";
export const SHORTCUT_FAILED = "shortcut://failed";
