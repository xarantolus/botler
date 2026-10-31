import { describe, expect, it } from "vitest";
import { sessionOptions, sessionStart } from "./sessions";

describe("sessions", () => {
  it("reads when a handover session started from its file name", () => {
    expect(sessionStart("chat-ab12-20261030-170501-123.jsonl")?.toISOString()).toBe("2026-10-30T17:05:01.000Z");
    expect(sessionStart("botler.jsonl")).toBeNull();
  });

  it("lists the current session first, then older ones numbered", () => {
    const fmt = (d: Date) => d.toISOString().slice(0, 16);
    const files = ["botler.jsonl", "botler-20261029-080000-001.jsonl", "botler-20261030-170501-123.jsonl"];
    expect(sessionOptions(files, fmt).map((o) => [o.value, o.label])).toEqual([
      ["botler-20261030-170501-123.jsonl", "Current session · started 2026-10-30T17:05"],
      ["botler-20261029-080000-001.jsonl", "Session 2 · started 2026-10-29T08:00"],
      ["botler.jsonl", "Session 1 · the first"],
    ]);
  });
});
