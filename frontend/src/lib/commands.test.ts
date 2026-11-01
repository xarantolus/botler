import { describe, expect, it } from "vitest";
import { commandOf, suggestions } from "./commands";

describe("slash commands", () => {
  it("knows a command only when the draft is exactly one", () => {
    expect(commandOf("/compact")?.name).toBe("compact");
    expect(commandOf(" /Clear ")?.name).toBe("clear");
    expect(commandOf("/compact now please")).toBeNull();
    expect(commandOf("/etc/hosts")).toBeNull();
    expect(commandOf("compact")).toBeNull();
  });

  it("suggests while one is being typed", () => {
    expect(suggestions("/c").map((c) => c.name)).toEqual(["compact", "clear"]);
    expect(suggestions("/").length).toBe(3);
    expect(suggestions("/compact x")).toEqual([]);
    expect(suggestions("hi")).toEqual([]);
  });
});
