import { describe, expect, it } from "vitest";
import type { SystemView, TriggerPreview } from "../generated";
import { fireLabel, lastFailed, localLabel, nextFire, plain, schedules, serviceStateColor, untilLabel } from "./system";

const now = Date.parse("2026-10-30T21:00:00Z"); // a Friday

function trig(id: string, upcoming: string[]): TriggerPreview {
  return { id, status: "unchanged", when: "", then: "", upcoming, shadow: false, before: null };
}

describe("fire times", () => {
  it("counts down in readable units", () => {
    expect(untilLabel("2026-10-30T20:59:00Z", now)).toBe("now");
    expect(untilLabel("2026-10-30T21:00:40Z", now)).toBe("in 40 s");
    expect(untilLabel("2026-10-30T21:12:00Z", now)).toBe("in 12 min");
    expect(untilLabel("2026-10-31T06:23:00Z", now)).toBe("in 9 h");
    expect(untilLabel("2026-11-03T21:00:00Z", now)).toBe("in 4 d");
  });

  it("shows the instance's wall clock", () => {
    // 05:23 UTC is 06:23 in Berlin (CET) on Saturday.
    expect(localLabel("2026-10-31T05:23:00Z", "Europe/Berlin")).toBe("Sat 06:23");
    expect(fireLabel("2026-10-31T05:23:00Z", "Europe/Berlin", now)).toBe("in 8 h · Sat 06:23");
  });
});

describe("schedules", () => {
  it("lists each upcoming trigger once, soonest first", () => {
    const shared = trig("triggers/a.toml#x", ["2026-10-31T09:00:00Z"]);
    const v = {
      jobs: [{ name: "web-health", triggers: [trig("jobs/web-health/job.toml#daily", ["2026-10-31T05:23:00Z"]), shared] }],
      agents: [{ triggers: [trig("agents/b/agent.toml#on", [])] }],
      triggers: [shared, trig("triggers/a.toml#early", ["2026-10-30T22:00:00Z"])],
    } as unknown as SystemView;
    const s = schedules(v);
    expect(s.map((x) => x.trigger.id)).toEqual(["triggers/a.toml#early", "jobs/web-health/job.toml#daily", "triggers/a.toml#x"]);
    expect(s[1]?.job).toBe("web-health");
    expect(nextFire(v.jobs[0]!.triggers)).toBe("2026-10-31T05:23:00Z");
    expect(nextFire([])).toBeNull();
    expect(plain("run job `web-health`")).toBe("run job web-health");
  });
});

describe("states", () => {
  it("colors services and failed runs", () => {
    expect(serviceStateColor("failed")).toBe("error");
    expect(serviceStateColor("backoff")).toBe("warning");
    expect(serviceStateColor("updating")).toBe("info");
    expect(serviceStateColor("running")).toBe("success");
    expect(lastFailed({ recent_runs: [] })).toBe(false);
    expect(lastFailed({ recent_runs: [{ status: "timeout" }] } as never)).toBe(true);
  });
});
