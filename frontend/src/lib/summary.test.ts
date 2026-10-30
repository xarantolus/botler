import { describe, expect, it } from "vitest";
import { looksLikeSummary, parseSummary, sectionKind } from "./summary";

// Shaped exactly like `ChangeSummary::to_markdown` plus the approval dialog's trailer.
const SAMPLE = `**Check this (likely mistakes):**
- trigger \`digest\` fires at 03:00, while you are probably asleep

**Needs the user's approval because it:**
- gives agent \`main\` network access
- declares new container secret \`GH_TOKEN\` in group \`github\`

**Will start happening:**
- at 07:00 on weekdays: message the \`main\` agent with "Morning digest" *(\`digest\`)*
  - next: Mon 2026-11-02 07:00 CET, Tue 2026-11-03 07:00 CET
- service \`poller\` runs continuously

**Will stop happening:**
- every 5 minutes: run job \`old\` *(\`old\`)*

**Will change:**
- \`nightly\`: was "at 02:00: run job \`a\`", becomes "at 03:00: run job \`a\`"
  - next: Sat 2026-10-31 03:00 CET

**Definitions:**
- added: agent \`helper\`, job \`poller\`
- removed: job \`old\`
- changed: agent \`main\`


**Reviewer:** looks fine,
but double-check the schedule

**Gate:** passed`;

describe("parseSummary", () => {
  const p = parseSummary(SAMPLE);

  it("finds every section in order", () => {
    expect(p.sections.map((s) => s.kind)).toEqual([
      "check",
      "escalations",
      "start",
      "stop",
      "change",
      "definitions",
      "reviewer",
      "gate",
    ]);
    expect(p.preamble).toBe("");
  });

  it("collects items and nested lines", () => {
    const start = p.sections[2]!;
    expect(start.items).toHaveLength(2);
    expect(start.items[0]!.sub).toEqual(["next: Mon 2026-11-02 07:00 CET, Tue 2026-11-03 07:00 CET"]);
    expect(p.sections[1]!.items).toHaveLength(2);
    expect(p.sections[0]!.items[0]!.text).toContain("03:00");
  });

  it("splits changes into before and after", () => {
    const c = p.sections[4]!.items[0]!;
    expect(c.change).toEqual({ id: "nightly", before: "at 02:00: run job `a`", after: "at 03:00: run job `a`" });
    expect(c.sub).toEqual(["next: Sat 2026-10-31 03:00 CET"]);
  });

  it("tags definition lines", () => {
    expect(p.sections[5]!.items.map((i) => [i.status, i.text])).toEqual([
      ["added", "agent `helper`, job `poller`"],
      ["removed", "job `old`"],
      ["changed", "agent `main`"],
    ]);
  });

  it("keeps inline and multi-line text", () => {
    expect(p.sections[6]!.inline).toBe("looks fine,");
    expect(p.sections[6]!.text).toBe("but double-check the schedule");
    expect(p.sections[7]!.inline).toBe("passed");
  });

  it("handles the no-escalation line and an empty summary", () => {
    const q = parseSummary("**Approval:** no capability changes; the reviewer can approve this alone.\n\n");
    expect(q.sections[0]!.kind).toBe("approval");
    expect(q.sections[0]!.inline).toMatch(/^no capability/);
    const empty = parseSummary("No effective changes.");
    expect(empty.sections).toHaveLength(0);
    expect(empty.preamble).toBe("No effective changes.");
    expect(looksLikeSummary("No effective changes.")).toBe(false);
    expect(looksLikeSummary(SAMPLE)).toBe(true);
  });

  it("maps unknown headings to other", () => {
    expect(sectionKind("Something else")).toBe("other");
    expect(sectionKind("Check this (likely mistakes)")).toBe("check");
  });
});
