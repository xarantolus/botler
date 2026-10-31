// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { onPi, watch } from "./live";
import { persisted } from "./lib/transcript";
import { state } from "./store";

describe("live stream", () => {
  it("hands a message over to the session even when the chat was opened while it streamed", () => {
    // Opened mid-message: no message_start, only the rest of the deltas.
    watch("botler");
    const delta = (d: string) => onPi("botler", { type: "message_update", assistantMessageEvent: { type: "text_delta", delta: d } });
    delta("rest ");
    delta("of it");
    expect(state.streaming.ts).toBeNull();
    onPi("botler", { type: "message_end", message: { role: "assistant", timestamp: 1793447836011 } });
    // The persisted entry replaces it, not shown next to it.
    expect(state.streaming).toMatchObject({ text: "rest of it", ended: true, ts: 1793447836011 });
    const entry = { kind: "assistant", id: "a1", message_ts: 1793447836011 } as const;
    expect(persisted([entry as never], state.streaming)).toBe(true);
  });
});
