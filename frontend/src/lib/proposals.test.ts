import { describe, expect, it } from "vitest";
import type { Proposal } from "../generated";
import { gateVerdict, pipeline, rejectable, splitRows, unifiedRows } from "./proposals";

const hunks = [
  { header: "@@ -3,4 +3,5 @@ fn main", old_start: 3, new_start: 3, lines: [" a", "-b", "-c", "+B", "+C", "+D", " e", "\\ No newline at end of file"] },
];

describe("diff rows", () => {
  it("numbers unified lines in the old and new file", () => {
    const rows = unifiedRows(hunks);
    expect(rows.map((r) => [r.kind, r.old, r.new, r.text])).toEqual([
      ["hunk", undefined, undefined, "@@ -3,4 +3,5 @@ fn main"],
      ["context", 3, 3, "a"],
      ["del", 4, undefined, "b"],
      ["del", 5, undefined, "c"],
      ["add", undefined, 4, "B"],
      ["add", undefined, 5, "C"],
      ["add", undefined, 6, "D"],
      ["context", 6, 7, "e"],
      ["note", undefined, undefined, "No newline at end of file"],
    ]);
  });

  it("pairs removed with added lines side by side", () => {
    const pairs = splitRows(unifiedRows(hunks));
    expect(pairs.map((p) => (p.kind === "hunk" ? "hunk" : `${p.left?.text ?? "_"}|${p.right?.text ?? "_"}`))).toEqual([
      "hunk",
      "a|a",
      "b|B",
      "c|C",
      "_|D",
      "e|e",
      "No newline at end of file|No newline at end of file",
    ]);
  });

  it("does not pair added lines with a later removal", () => {
    const pairs = splitRows(unifiedRows([{ header: "@@ -1 +1 @@", old_start: 1, new_start: 1, lines: ["+x", "-y"] }]));
    expect(pairs.slice(1).map((p) => `${p.left?.text ?? "_"}|${p.right?.text ?? "_"}`)).toEqual(["_|x", "y|_"]);
  });
});

const proposal = (extra: Partial<Proposal>): Proposal => ({
  id: "p1",
  kind: "change",
  instance: "botler",
  title: "Digest",
  branch: "agents/botler/digest",
  base_commit: "a",
  head_commit: "b",
  action: null,
  args: null,
  args_hash: null,
  code_version: null,
  summary: null,
  gate_report: null,
  verdict: null,
  review_notes: null,
  status: "gating",
  merged_commit: null,
  reviewer: null,
  attempts: 1,
  stage: "rebase",
  stage_at: "2026-10-30T10:00:00Z",
  decision: null,
  decided_by: null,
  decided_at: null,
  decision_reason: null,
  created_at: "2026-10-30T10:00:00Z",
  updated_at: "2026-10-30T10:00:00Z",
  ...extra,
});

describe("pipeline", () => {
  const now = new Date("2026-10-30T10:02:05Z").getTime();

  it("shows the gate's sub-state with elapsed time", () => {
    const { steps, current } = pipeline(proposal({ stage: "image" }), now);
    expect(current).toBe(1);
    expect(steps[1]!.description).toBe("building the image · 2m 05s");
    expect(pipeline(proposal({ stage: "gate" }), now).steps[1]!.description).toBe("running checks · 2m 05s");
    expect(gateVerdict(proposal({ stage: "gate" }))).toEqual({ label: "running", color: "info" });
  });

  it("explains an auto-merge: approval was not needed", () => {
    const p = proposal({ status: "merged", stage: "merge", verdict: "approve", merged_commit: "0123456789abc", gate_report: "ok" });
    const { steps, current } = pipeline(p, now);
    expect(current).toBe(5);
    expect(steps.map((s) => s.description)).toEqual(["onto main", "passed", "approve", "not needed", "0123456789"]);
    expect(rejectable(p)).toBe(false);
  });

  it("can be rejected until the merge starts", () => {
    expect(rejectable(proposal({ status: "gating", stage: "image" }))).toBe(true);
    expect(rejectable(proposal({ status: "reviewing", stage: "review" }))).toBe(true);
    expect(rejectable(proposal({ status: "awaiting_user", stage: "merge" }))).toBe(false);
    const r = proposal({ status: "rejected", stage: "gate", decided_by: "user" });
    expect(rejectable(r)).toBe(false);
    expect(gateVerdict(r)).toBeNull();
    expect(pipeline(r, now).steps[3]!.description).toBe("rejected by you");
  });

  it("reports a failed gate", () => {
    expect(gateVerdict(proposal({ status: "failed", stage: "gate", gate_report: "E501" }))).toEqual({ label: "failed", color: "error" });
  });
});
