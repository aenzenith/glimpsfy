// A wrong exponent or threshold here would pass an unreadable colour pair as "AA" without a
// sound, and a wrong matrix would copy a different colour than the one on screen, so the
// contrast measures and the P3 conversion are checked against published values.

import { describe, expect, it } from "vitest";

import { apcaLc, formatted, hex, p3ToSrgb, parseSwatch, read, wcagPasses, wcagRatio, type Rgb, type Sample } from "./color";

const srgb = (h: string): Sample => parseSwatch(h)!;

describe("contrast", () => {
  it("fails AA for #777 on white, though the ratio shows as 4.48", () => {
    const ratio = wcagRatio(srgb("#777777"), srgb("#FFFFFF"));
    expect(ratio).toBeCloseTo(4.478, 3);
    expect(wcagPasses(ratio).normalAA).toBe(false);
    expect(wcagPasses(ratio).largeAA).toBe(true);
  });

  it("gives APCA-W3's sample values for #888 and white, signed by polarity", () => {
    expect(apcaLc(srgb("#888888"), srgb("#FFFFFF"))).toBeCloseTo(63.06, 2);
    expect(apcaLc(srgb("#FFFFFF"), srgb("#888888"))).toBeCloseTo(-68.54, 2);
  });
});

describe("Display P3", () => {
  it("reads the P3 bytes of #6C5CF6 back as #6C5CF6, within one step", () => {
    const reading = read({ space: "display-p3", bytes: [0x69, 0x5d, 0xed] });
    const want: Rgb = [0x6c, 0x5c, 0xf6];
    reading.srgb.forEach((v, i) => expect(Math.abs(v - want[i])).toBeLessThanOrEqual(1));
    expect(reading.out).toBe(false);
  });

  it("marks a P3 orange outside sRGB, and keeps ≈ out of the copied text", () => {
    const { out } = p3ToSrgb([1, 0.25, 0.1]);
    expect(out).toBe(true);
    const reading = read({ space: "display-p3", bytes: [255, 64, 26] });
    expect(reading.out).toBe(true);
    expect(formatted(reading, "hex")).toBe(hex(reading.srgb));
    expect(formatted(reading, "hex")).not.toContain("≈");
  });
});
