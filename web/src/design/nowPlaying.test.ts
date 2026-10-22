import { describe, expect, it } from "vitest";

import { marqueeOffset } from "./components.js";

describe("marqueeOffset", () => {
  it("matches the dock's timing: still, moving, still at the end, again", () => {
    // 80 px too long at 40 px/s with 1 s pauses (as `marquee_offset` in now_playing.rs).
    const at = (s: number) => Math.round(marqueeOffset(s * 1000, 80, 40, 1000));
    expect(at(0.5)).toBe(0);
    expect(at(1.5)).toBe(20);
    expect(at(3)).toBe(80);
    expect(at(3.5)).toBe(80);
    expect(at(4)).toBe(0);
    expect(at(5.5)).toBe(20);
  });
  it("doesn't move text that fits", () => {
    expect(marqueeOffset(5000, -10, 40, 1000)).toBe(0);
  });
});
