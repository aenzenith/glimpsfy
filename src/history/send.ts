// The Send menu of the popover and the history window (A2): Copy path first, then the targets of
// Settings › Output in their order with their digits, then Edit targets…. A target that is not on
// this Mac shows dimmed with the reason in place of its digit.

import type { SendTargets } from "../lib/api/send";
import { t } from "../lib/i18n";
import type { MenuItem } from "./Menu.vue";

export type SendMenu = {
  /** The entry has no file (a text copy), so Copy path is off. */
  noPath: boolean;
  copyPath(): void;
  send(bundleId: string): void;
  edit(): void;
};

export function sendMenuItems(list: SendTargets, menu: SendMenu): MenuItem[] {
  const items: MenuItem[] = [
    { label: t("send.copyPath"), icon: "path", keys: menu.noPath ? undefined : "⌥⌘C", note: menu.noPath ? t("send.noFile") : undefined, disabled: menu.noPath, run: menu.copyPath },
  ];
  if (list.targets.length) items.push("separator");
  list.targets.forEach((target, i) => {
    items.push({
      label: target.name,
      icon: "send",
      image: target.icon,
      keys: target.installed && i < 9 ? String(i + 1) : undefined,
      note: target.installed ? undefined : t("send.missing"),
      disabled: !target.installed,
      run: () => menu.send(target.bundleId),
    });
  });
  items.push("separator", { label: t("send.editTargets"), icon: "settings", run: menu.edit });
  return items;
}
