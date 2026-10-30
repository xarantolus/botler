import { describe, expect, it } from "vitest";
import type { SearchHit } from "../generated";
import { groupHits, highlight, hitLocation } from "./search";

const hit = (source: SearchHit["source"], ref: string, owner = ""): SearchHit => ({
  source,
  ref,
  owner,
  at: null,
  title: ref,
  snippet: "",
});

describe("search", () => {
  it("splits snippets into marked parts", () => {
    expect(highlight("fix the **antenna** now")).toEqual([
      { text: "fix the ", mark: false },
      { text: "antenna", mark: true },
      { text: " now", mark: false },
    ]);
    expect(highlight("**a** b")).toEqual([
      { text: "a", mark: true },
      { text: " b", mark: false },
    ]);
  });

  it("groups by source in a fixed order, keeping the ranking", () => {
    const g = groupHits([hit("repo", "r"), hit("session", "s1"), hit("session", "s2"), hit("event", "e")]);
    expect(g.map((x) => [x.source, x.hits.map((h) => h.ref)])).toEqual([
      ["session", ["s1", "s2"]],
      ["event", ["e"]],
      ["repo", ["r"]],
    ]);
  });

  it("links sessions to the chat entry and events to their trace", () => {
    expect(hitLocation(hit("session", "abc", "dev-1"))).toEqual({ path: "/chat/dev-1", query: { entry: "abc" } });
    expect(hitLocation(hit("event", "e1"))).toEqual({ path: "/events", query: { event: "e1" } });
    expect(hitLocation(hit("repo", "a.md"))).toBeNull();
  });
});
