import { describe, expect, it } from "vitest";
import type { Breaker } from "../generated";
import { heldFor, isTripped, pairOf, pausing } from "./breakers";

const b = (subject: string): Breaker => ({ subject, kind: "pair", reason: "r", tripped_at: "2026-10-30T10:00:00Z" });

describe("breakers", () => {
  it("parses pair subjects", () => {
    expect(pairOf("pair:alice|bob")).toEqual(["alice", "bob"]);
    expect(pairOf("chain:123")).toBeNull();
    expect(pairOf("pair:alice")).toBeNull();
    expect(pairOf("pair:a|b|c")).toBeNull();
  });

  it("finds the breakers holding a chat's messages", () => {
    const list = [b("pair:alice|bob"), b("chain:1"), b("pair:carol|alice")];
    expect(heldFor("alice", list).map((h) => h.other)).toEqual(["bob", "carol"]);
    expect(heldFor("bob", list).map((h) => h.other)).toEqual(["alice"]);
    expect(heldFor("dave", list)).toEqual([]);
    expect(isTripped("chain:1", list)).toBe(true);
    expect(isTripped("chain:2", list)).toBe(false);
    expect(pausing("alice", [...list, b("instance:alice"), b("instance:bob")]).map((x) => x.subject)).toEqual([
      "instance:alice",
    ]);
  });
});
