// The update's state for a page: read once, then followed through `update://changed`, and the
// one sentence Settings and the popover use for each state.

import { listen } from "@tauri-apps/api/event";
import { onMounted, onUnmounted, ref } from "vue";

import { t } from "./i18n";
import { update_state, UPDATE_CHANGED, type UpdateView } from "./api/update";

/** The update's state, kept current while the component lives. */
export function useUpdate() {
  const view = ref<UpdateView | null>(null);
  let stop: (() => void) | null = null;
  onMounted(async () => {
    stop = await listen<UpdateView>(UPDATE_CHANGED, (event) => (view.value = event.payload));
    try {
      view.value = await update_state();
    } catch {
      view.value = null;
    }
  });
  onUnmounted(() => stop?.());
  return view;
}

/** What the state means, as a sentence. */
export function describeUpdate(view: UpdateView): string {
  const version = view.version ?? "";
  switch (view.status) {
    case "off":
      return t("update.status.off");
    case "checking":
      return t("update.status.checking");
    case "downloading": {
      const percent = view.total ? Math.min(100, Math.round((view.downloaded / view.total) * 100)) : 0;
      return t("update.status.downloading", { version, percent });
    }
    case "ready":
      return t("update.status.ready", { version });
    case "available":
      return t("update.status.available", { version });
    case "installing":
      return t("update.status.installing");
    case "failed":
      if (view.error === "signature") return t("update.status.failedSignature", { version });
      if (view.error === "install") return t("update.status.failedInstall", { version });
      return t("update.status.failedDownload", { version });
    default:
      return view.offline ? t("update.status.offline") : t("update.status.idle");
  }
}
