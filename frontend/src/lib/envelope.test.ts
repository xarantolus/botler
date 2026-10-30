import { describe, expect, it } from "vitest";
import { envelopeOf, isUserMessage, parseEnvelope, prettyJsonBody } from "./envelope";

describe("parseEnvelope", () => {
  it("parses the full header", () => {
    const e = parseEnvelope(
      "[from agent:triage | provenance: external | runnable.failed | delivery d1 | event e1]\nhello\nworld",
    );
    expect(e).toEqual({
      from: "agent:triage",
      provenance: "external",
      kind: "runnable.failed",
      delivery: "d1",
      event: "e1",
      body: "hello\nworld",
      malformed: false,
    });
  });

  it("accepts the short form without kind and event", () => {
    const e = parseEnvelope("[from user | provenance: trusted | delivery d2]\nhi");
    expect(e?.from).toBe("user");
    expect(e?.kind).toBeNull();
    expect(e?.delivery).toBe("d2");
    expect(e?.event).toBeNull();
    expect(e?.body).toBe("hi");
  });

  it("handles a header without body", () => {
    expect(parseEnvelope("[from hub | provenance: trusted | agent.idle | delivery d | event e]")?.body).toBe("");
  });

  it("rejects plain text and look-alikes", () => {
    expect(parseEnvelope("hello")).toBeNull();
    expect(parseEnvelope("[from you]\nno provenance")).toBeNull();
    expect(parseEnvelope("[from a | provenance: trusted | two words | delivery d]")).toBeNull();
    expect(parseEnvelope("[note] something")).toBeNull();
  });

  it("flags malformed deliveries", () => {
    const e = parseEnvelope("[botler: malformed delivery]\nbotler:delivery {oops");
    expect(e?.malformed).toBe(true);
    expect(e?.body).toBe("botler:delivery {oops");
  });
});

describe("envelopeOf", () => {
  it("prefers the hub's parse", () => {
    const e = envelopeOf({
      text: "body",
      from: "user",
      provenance: "trusted",
      event_kind: "message",
      delivery: "d",
      event: "e",
    });
    expect(e?.body).toBe("body");
    expect(isUserMessage(e!)).toBe(true);
  });

  it("falls back to parsing the text", () => {
    const e = envelopeOf({
      text: "[from agent:x | provenance: trusted | message | delivery d | event e]\nhey",
      from: null,
      provenance: null,
      event_kind: null,
      delivery: null,
      event: null,
    });
    expect(e?.from).toBe("agent:x");
    expect(isUserMessage(e!)).toBe(false);
  });

  it("is null for a typed user message without header", () => {
    expect(
      envelopeOf({ text: "hi", from: null, provenance: null, event_kind: null, delivery: null, event: null }),
    ).toBeNull();
  });
});

describe("prettyJsonBody", () => {
  it("pretty-prints JSON and leaves prose alone", () => {
    expect(prettyJsonBody('{"a":1}')).toBe('{\n  "a": 1\n}');
    expect(prettyJsonBody("{not json")).toBeNull();
    expect(prettyJsonBody("plain")).toBeNull();
  });
});
