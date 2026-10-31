import { describe, expect, it } from "vitest";
import { currentStatus, phaseBadge, stepLabel } from "./phase";

describe("phaseBadge", () => {
  it("shows archived above everything", () => {
    expect(phaseBadge({ state: "archived", status: { status: "running" } }).label).toBe("archived");
  });

  it("shows waking with its step and a spinner", () => {
    const b = phaseBadge({ state: "starting", status: { status: "waking", step: "building_image" } });
    expect(b).toEqual({ label: "starting", type: "info", busy: true, detail: "Building image" });
    expect(phaseBadge({ state: "starting", status: null }).busy).toBe(true);
  });

  it("shows failures and pauses as errors with the reason", () => {
    const f = phaseBadge({ state: "dormant", status: { status: "failed", error: "could not start: boom" } });
    expect([f.label, f.type, f.detail]).toEqual(["crashed", "error", "could not start: boom"]);
    expect(phaseBadge({ state: "running", status: { status: "paused", reason: "budget" } }).type).toBe("error");
  });

  it("shows a scheduled retry with when and why", () => {
    const at = new Date(Date.now() + 125_000).toISOString();
    const b = phaseBadge({ state: "dormant", status: { status: "retrying", at, error: "Connection error." } });
    expect([b.label, b.type, b.detail]).toEqual(["retrying", "warning", "retrying in 2m: Connection error."]);
    expect(currentStatus({ phase: "retrying", at, error: "x" }, null)).toEqual({ status: "retrying", at, error: "x" });
  });

  it("distinguishes working, running, queued and sleeping", () => {
    expect(phaseBadge({ state: "running", status: null, busy: true }).label).toBe("working");
    expect(phaseBadge({ state: "running", status: null }).label).toBe("running");
    expect(phaseBadge({ state: "dormant", status: null, pending: 2 }).label).toBe("queued");
    expect(phaseBadge({ state: "dormant", status: null }).label).toBe("sleeping");
    // A live "dormant" overrides a stale stored "running".
    expect(phaseBadge({ state: "running", status: { status: "dormant" } }).label).toBe("sleeping");
  });

  it("says when a chat waits on the user, asleep or not", () => {
    const asleep = phaseBadge({ state: "dormant", status: null, waitingOnUser: 1 });
    expect([asleep.label, asleep.detail]).toEqual(["waiting for you", "Sleeping, 1 request waiting for your answer"]);
    // It counts them, so answering one of several does not look like it did nothing.
    expect(phaseBadge({ state: "running", status: null, waitingOnUser: 2 }).detail).toBe(
      "2 requests waiting for your answer",
    );
    // Working beats waiting: the agent is busy right now.
    expect(phaseBadge({ state: "running", status: null, busy: true, waitingOnUser: 1 }).label).toBe("working");
  });

  it("shows a pending handover", () => {
    expect(phaseBadge({ state: "running", status: null, archiveRequested: true }).label).toBe("handing over");
  });
});

describe("currentStatus", () => {
  it("prefers the live status and converts the fetched phase", () => {
    expect(currentStatus({ phase: "waking", step: "starting_container" }, null)).toEqual({
      status: "waking",
      step: "starting_container",
    });
    expect(currentStatus({ phase: "failed", error: "x" }, { status: "running" })).toEqual({ status: "running" });
    expect(currentStatus(null, null)).toBeNull();
    expect(stepLabel("preparing_workspace")).toBe("Preparing workspace");
  });
});
