import { describe, expect, it } from "vitest";
import { proposalStatusColor, proposalStatusLabel, provenanceColor, runDuration, runStatusColor } from "./runs";

describe("status colors", () => {
  it("maps run, proposal and provenance states", () => {
    expect(runStatusColor("succeeded")).toBe("success");
    expect(runStatusColor("queued")).toBe("info");
    expect(runStatusColor("timeout")).toBe("error");
    expect(proposalStatusColor("awaiting_user")).toBe("warning");
    expect(proposalStatusLabel("awaiting_user")).toBe("awaiting user");
    expect(provenanceColor("trusted")).toBe("success");
    expect(provenanceColor("external")).toBe("warning");
    expect(provenanceColor("untrusted")).toBe("error");
  });
});

describe("runDuration", () => {
  it("reads seconds, then minutes, and runs on while unfinished", () => {
    expect(runDuration({ started_at: null, finished_at: null })).toBe("");
    expect(runDuration({ started_at: "2026-10-30T10:00:00Z", finished_at: "2026-10-30T10:00:42Z" })).toBe("42s");
    expect(runDuration({ started_at: "2026-10-30T10:00:00Z", finished_at: "2026-10-30T10:05:00Z" })).toBe("5 min");
    expect(runDuration({ started_at: "2026-10-30T10:00:00Z", finished_at: null }, Date.parse("2026-10-30T10:00:10Z"))).toBe("10s");
  });
});
