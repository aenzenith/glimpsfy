// Keeps src/lib/annotate.ts and crates/annotate in step, and keeps old documents readable. Both
// sides read the same fixture, which has an object of every kind, and both must write it back
// unchanged; the Rust half is crates/annotate/tests/fixture.rs. A field renamed on one side only
// fails that side's test, before a saved document silently loses the field.
//
// fixtures/annotate/versions/ holds one document per older format version. Each must load through
// the migration with every object, every field and the same order, because drawings that vanish
// from a saved capture on the next launch cannot be brought back.

import { describe, expect, it } from "vitest";

import fixture from "../../fixtures/annotate/document.json";
import { CURRENT_VERSION, OBJECT_KINDS, migrateDocument, parseDocument, serializeDocument } from "./annotate";

const versions = import.meta.glob<{ default: Record<string, unknown> }>("../../fixtures/annotate/versions/*.json", { eager: true });

describe("the annotation document", () => {
  it("round-trips the shared fixture unchanged and covers every kind", () => {
    expect(fixture.version).toEqual(CURRENT_VERSION);
    const document = parseDocument(fixture);

    expect(serializeDocument(document)).toEqual(fixture);

    const kinds = new Set(document.objects.map((object) => object.kind));
    for (const kind of OBJECT_KINDS) expect(kinds, `the fixture has no ${kind} object`).toContain(kind);
  });

  it("loads every older version through the migration without losing an object", () => {
    const entries = Object.entries(versions);
    expect(entries.length, "no fixture of an older version was found").toBeGreaterThan(0);
    for (const [name, module] of entries) {
      const original = module.default as { version: { major: number; minor: number }; image: unknown; scale: number; objects: Record<string, unknown>[] };
      const older = original.version.major < CURRENT_VERSION.major || (original.version.major === CURRENT_VERSION.major && original.version.minor < CURRENT_VERSION.minor);
      expect(older, `${name} is not an older version`).toBe(true);

      const document = parseDocument(original);
      expect(document.version, name).toEqual(CURRENT_VERSION);
      expect(document.objects.length, `${name} lost objects`).toBe(original.objects.length);

      const written = serializeDocument(document) as { image: unknown; scale: number; objects: Record<string, unknown>[] };
      original.objects.forEach((old, i) => {
        for (const [key, value] of Object.entries(old)) expect(written.objects[i][key], `${name}: ${key} of ${String(old.id)}`).toEqual(value);
      });
      expect(written.image, name).toEqual(original.image);
      expect(written.scale, name).toEqual(original.scale);

      // The input is left alone, and migrating twice changes nothing.
      const once = migrateDocument(original);
      expect(migrateDocument(once), name).toEqual(once);
      expect(original.version, `${name} was changed in place`).not.toEqual(CURRENT_VERSION);
    }
  });
});
