// Deleting a history entry forgets its hosted copy's link and deletion key, and nothing brings them
// back. While Settings › General keeps uploads off (or the app offline) the sheet cannot delete the
// copy, so it must still show that the copy exists, that it stays online, and that the delete
// leaves Glimpsfy no way to remove it, instead of reading as if the capture lived only here.

import { describe, expect, it } from "vitest";

import type { Entry } from "../lib/api/history";
import { t, tp } from "../lib/i18n";
import { deletePlan } from "./deletePlan";

const hosted = (id: string) =>
  ({
    id,
    kind: "screenshot",
    savedPath: null,
    hosted: { provider: "imgur", url: `https://i.example/${id}`, deletion: { token: "secret" }, uploadedAtMs: 0, expiresAtMs: null },
  }) as unknown as Entry;

const base = { file: true, hosted: true, hostName: () => "Imgur" };

describe("the delete sheet with a hosted copy it cannot delete", () => {
  it.each([
    { name: "uploads off", uploadsAllowed: false, online: true, lock: "history.delete.hostLocked" as const },
    { name: "offline", uploadsAllowed: false, online: false, lock: "history.delete.hostLockedOffline" as const },
  ])("still shows the copy and says it stays online with $name", ({ uploadsAllowed, online, lock }) => {
    const one = deletePlan({ ...base, entries: [hosted("a")], uploadsAllowed, online });
    expect(one.uploaded).toHaveLength(1);
    expect(one.options.deleteHostedCopy).toBe(false);
    expect(one.locked).toBe(t(lock, { host: "Imgur" }));
    expect(one.summary).toBe(`${tp("history.delete.ownKeepHost", { host: "Imgur" })} ${t("history.delete.linkLost")}`);

    const many = deletePlan({ ...base, entries: [hosted("a"), hosted("b")], uploadsAllowed, online });
    expect(many.uploaded).toHaveLength(2);
    expect(many.options.deleteHostedCopy).toBe(false);
    expect(many.summary).toBe(`${t("history.delete.many.none", { n: 2 })} ${t("history.delete.many.linkLost")}`);
  });

  it("deletes the copy again once uploads are allowed", () => {
    const plan = deletePlan({ ...base, entries: [hosted("a")], uploadsAllowed: true, online: true });
    expect(plan.locked).toBeNull();
    expect(plan.options.deleteHostedCopy).toBe(true);
    expect(plan.summary).toBe(tp("history.delete.ownAndHost", { host: "Imgur" }));
  });
});
