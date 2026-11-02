import { describe, expect, it } from "vitest";
import { swipe } from "./swipe";

describe("swipe", () => {
  it("tells a sideways swipe's direction", () => {
    expect(swipe(10, 300, 150, 320)).toBe("right");
    expect(swipe(300, 300, 100, 290)).toBe("left");
  });
  it("ignores short or mostly vertical moves", () => {
    expect(swipe(10, 300, 50, 300)).toBeNull();
    expect(swipe(10, 100, 100, 300)).toBeNull();
  });
});
