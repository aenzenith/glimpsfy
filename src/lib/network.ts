// The network switches of Settings › General as every page sees them: whether Upload shows at
// all, and whether a model download or an update check may start. `main.ts` keeps them current
// from the settings; the core enforces the same switches before any connection
// (`src-tauri/src/network.rs`), so a page that shows a stale value still sends nothing.

import { ref } from "vue";

import type { General } from "./settings";

/** Upload buttons, ⌘U and hosted-copy deletions show only while this holds: Allow uploads is on
 * and the app is not offline. */
export const uploadsAllowed = ref(true);

/** False while Work offline is on: model downloads and update checks are greyed out. */
export const online = ref(true);

export function followNetwork(general: Pick<General, "allowUploads" | "offline">): void {
  online.value = general.offline !== true;
  uploadsAllowed.value = general.allowUploads !== false && general.offline !== true;
}
