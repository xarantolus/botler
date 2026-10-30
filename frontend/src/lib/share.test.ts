import { describe, expect, it } from "vitest";
import { shareText } from "./share";

describe("shareText", () => {
  it("joins title, text and url without repeats", () => {
    expect(shareText("A post", "Look at this https://x.y/1", "https://x.y/1")).toBe(
      "Shared with you:\n\nA post\nLook at this https://x.y/1",
    );
    expect(shareText(null, " ", "https://x.y")).toBe("Shared with you:\n\nhttps://x.y");
  });

  it("is null when nothing was shared", () => {
    expect(shareText(null, "", null)).toBeNull();
  });
});
