// An upload cannot be called back: once the history sends a file to a public host, it stays
// there. An entry saved first and copied after more drawing (a blurred password) holds both the
// user's older saved file and the history's newer own copy, and the card shows the newer one.
// Upload, drag-out and the format label must take that same file, as `Entry::media_path` does in
// the core, or the unblurred picture leaves the machine.

import { describe, expect, it } from "vitest";

import type { Entry } from "../lib/api/history";
import { extensionOf, mediaPath } from "./model";

const revised = {
  kind: "screenshot",
  savedPath: "/Users/someone/Pictures/Glimpsfy/Invoice.jpg",
  file: "/Users/someone/Library/Application Support/Glimpsfy/history/captures/0190-ab12.png",
} as Entry;

describe("the file an entry shows", () => {
  it("is the newer own copy when a saved file exists too", () => {
    expect(mediaPath(revised)).toBe(revised.file);
    expect(extensionOf(revised)).toBe("PNG");
    expect(mediaPath({ ...revised, file: null })).toBe(revised.savedPath);
  });
});
