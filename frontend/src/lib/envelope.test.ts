import { describe, expect, it } from "vitest";
import {
  envelopeOf,
  isUserMessage,
  prettyJsonBody,
  previewOf,
} from "./envelope";

describe("envelopeOf", () => {
  it("takes the hub's parse", () => {
    const e = envelopeOf({
      text: "body",
      from: "user",
      provenance: "trusted",
      event_kind: "message",
      delivery: "d",
      event: "e",
      local_time: "Fri 2026-10-30 17:05 UTC",
      gap: null,
    });
    expect(e?.body).toBe("body");
    expect(e?.localTime).toBe("Fri 2026-10-30 17:05 UTC");
    expect(isUserMessage(e!)).toBe(true);
  });

  it("is null for a typed user message without header", () => {
    expect(
      envelopeOf({
        text: "hi",
        from: null,
        provenance: null,
        event_kind: null,
        delivery: null,
        event: null,
      }),
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

describe("previewOf", () => {
  const json = (p: unknown) =>
    "Proposal moved.\n```json\n" + JSON.stringify(p) + "\n```";
  it("says what the event is about from its payload", () => {
    expect(
      previewOf(json({ id: "x", last_reply: "Gate passed.\nMore." })),
    ).toBe("Gate passed.");
    expect(previewOf(json({ detail: "main moved", status: "failed" }))).toBe(
      "main moved",
    );
    expect(previewOf(json({ changes: ["new image", "skills updated"] }))).toBe(
      "new image; skills updated",
    );
  });
  it("falls back to the text around the payload, or plain Markdown", () => {
    expect(previewOf(json({ id: "x" }))).toBe("Proposal moved.");
    expect(previewOf("## Heading\nbody")).toBe("Heading");
  });
});
