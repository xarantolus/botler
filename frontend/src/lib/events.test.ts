import { describe, expect, it } from "vitest";
import type { BotlerEvent, Dialog } from "../generated";
import { asBuiltin, chatTitle, eventIcon, eventSummary, linkedDialog, redirectChat } from "./events";

const ev = (type: string, payload: unknown): BotlerEvent => ({
  id: "e",
  type,
  source: { kind: "hub" },
  provenance: "trusted",
  hops: 0,
  urgent: false,
  at: "2026-10-30T10:00:00Z",
  payload: payload as BotlerEvent["payload"],
});

describe("eventIcon", () => {
  it("has an icon for built-in types and a generic one for the rest", () => {
    expect(eventIcon("timer")).toBe("i-lucide-alarm-clock");
    expect(eventIcon(null)).toBe("i-lucide-mail");
    expect(eventIcon("mail.received")).toBe("i-lucide-radio");
  });
});

describe("events", () => {
  it("narrows built-in events and leaves custom ones alone", () => {
    expect(asBuiltin(ev("timer", { timer_id: "t" }))?.type).toBe("timer");
    expect(asBuiltin(ev("mail.received", {}))).toBeNull();
    expect(eventSummary(ev("runnable.failed", { runnable: "poll", kind: "exit", exit_code: 1, count: 2 }))).toBe(
      "poll failed (exit, exit 1) ×2",
    );
    expect(eventSummary(ev("mail.received", { text: "hi" }))).toBe("hi");
  });

  it("sends a remembered main chat to the front door", () => {
    expect(redirectChat("main", [{ name: "botler" }], "botler")).toBe("botler");
    expect(redirectChat("main", [{ name: "main" }, { name: "botler" }], "botler")).toBeNull();
    expect(redirectChat("x", [{ name: "botler" }], "botler")).toBeNull();
    expect(redirectChat("main", [], null)).toBeNull();
  });

  it("names the front door", () => {
    expect(chatTitle({ name: "botler", title: null, slug: null }, "botler")).toBe("Botler");
    expect(chatTitle({ name: "x", title: null, slug: "my-x" }, "botler")).toBe("my-x");
  });

  it("summarises breakers and schedules", () => {
    expect(eventSummary(ev("breaker.tripped", { subject: "pair:a|b", breaker: "pair", reason: "ping-pong" }))).toBe(
      "pair breaker on pair:a|b: ping-pong",
    );
    expect(eventSummary(ev("schedule", { trigger: "digest", scheduled_for: "x", skipped: 0 }))).toBe("schedule digest");
  });

  it("links tool results to dialogs directly or by proposal", () => {
    const d = { id: "d1", proposal_id: "p1" } as Dialog;
    expect(linkedDialog({ dialog_id: "d1", proposal_id: null }, [d])).toBe(d);
    expect(linkedDialog({ dialog_id: null, proposal_id: "p1" }, [d])).toBe(d);
    expect(linkedDialog({ dialog_id: null, proposal_id: null }, [d])).toBeNull();
  });
});
