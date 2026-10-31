import { describe, expect, it } from "vitest";
import type { Dialog, SessionItem } from "../generated";
import { noLive, buildTranscript, toolRunning, type TranscriptInput } from "./transcript";

type Item<K extends SessionItem["kind"]> = Extract<SessionItem, { kind: K }>;

const user = (id: string, text: string, extra: Partial<Item<"user">> = {}): Item<"user"> => ({
  kind: "user",
  id,
  at: "2026-10-30T10:00:00Z",
  text,
  from: null,
  provenance: null,
  event_kind: null,
  delivery: null,
  event: null,
  local_time: null,
  gap: null,
  message_text: null,
  files: [],
  ...extra,
});

const assistant = (id: string, extra: Partial<Item<"assistant">> = {}): Item<"assistant"> => ({
  kind: "assistant",
  id,
  at: "2026-10-30T10:01:00Z",
  text: "",
  thinking: "",
  tool_calls: [],
  model: null,
  cost_usd: null,
  error: null,
  message_ts: null,
  ...extra,
});

const result = (id: string, callId: string | null, text: string, extra: Partial<Item<"tool_result">> = {}): Item<"tool_result"> => ({
  kind: "tool_result",
  id,
  at: "2026-10-30T10:01:30Z",
  tool: "bash",
  tool_call_id: callId,
  text,
  is_error: false,
  dialog_id: null,
  proposal_id: null,
  ...extra,
});

const dialog = (id: string, extra: Partial<Dialog> = {}): Dialog => ({
  id,
  instance: "botler",
  kind: "question",
  question: "Which one?",
  detail: null,
  ctas: [],
  about: [],
  priority: "normal",
  proposal_id: null,
  spec: null,
  status: "open",
  answer: null,
  abort_reason: null,
  expires_at: null,
  created_at: "2026-10-30T10:01:00Z",
  resolved_at: null,
  ...extra,
});

const input = (items: SessionItem[], over: Partial<TranscriptInput> = {}): TranscriptInput => ({
  items,
  dialogs: [],
  queued: [],
  streaming: noLive(),
  busy: false,
  ...over,
});

describe("buildTranscript", () => {
  it("maps the user's messages to text parts with their delivery metadata", () => {
    const t = buildTranscript(
      input([
        user("u1", "[Fri 17:05 UTC (first message in 3 days) | from user | provenance: trusted | message | delivery d1]\n hi \n", {
          from: "user",
          provenance: "trusted",
          event_kind: "message",
          delivery: "d1",
          local_time: "Fri 17:05 UTC",
          gap: "first message in 3 days",
          message_text: "hi",
        }),
        user("u2", "plain text without a header"),
      ]),
    );
    expect(t.messages).toEqual([
      {
        id: "u1",
        role: "user",
        parts: [{ type: "text", text: "hi" }],
        metadata: { at: "2026-10-30T10:00:00Z", delivery: "d1", localTime: "Fri 17:05 UTC", gap: "first message in 3 days" },
      },
      {
        id: "u2",
        role: "user",
        parts: [{ type: "text", text: "plain text without a header" }],
        metadata: { at: "2026-10-30T10:00:00Z", delivery: null, localTime: null, gap: null },
      },
    ]);
    expect(t.status).toBe("ready");
  });

  it("turns other deliveries into envelope cards", () => {
    const t = buildTranscript(
      input([
        user("u1", "[from agent:helper | provenance: trusted | runnable.failed | delivery d2 | event e2]\n{\"a\":1}"),
      ]),
    );
    const m = t.messages[0]!;
    expect(m.role).toBe("system");
    expect(m.metadata?.delivery).toBe("d2");
    expect(m.parts).toHaveLength(1);
    const p = m.parts[0]!;
    expect(p.type).toBe("data-envelope");
    if (p.type !== "data-envelope") throw new Error("not an envelope");
    expect(p.data.envelope).toMatchObject({ from: "agent:helper", kind: "runnable.failed", event: "e2", body: '{"a":1}' });
  });

  it("collects one agent turn: thinking, text, tools paired with their results, cost summed", () => {
    const t = buildTranscript(
      input([
        user("u1", "go"),
        assistant("a1", {
          thinking: "let me look",
          text: "Looking.",
          model: "m1",
          cost_usd: 0.01,
          tool_calls: [
            { id: "c1", name: "bash", arguments: { command: "ls" } },
            { id: "c2", name: "read", arguments: { path: "x" } },
            { id: "c3", name: "bash", arguments: { command: "sleep 9" } },
          ],
        }),
        result("r1", "c1", "a\nb"),
        result("r2", "c2", "no such file", { is_error: true, tool: "read" }),
        assistant("a2", { text: "Done.", model: "m2", cost_usd: 0.02 }),
      ]),
    );
    expect(t.messages).toHaveLength(2);
    const turn = t.messages[1]!;
    expect(turn.id).toBe("a1");
    expect(turn.role).toBe("assistant");
    expect(turn.metadata?.model).toBe("m2");
    expect(turn.metadata?.costUsd).toBeCloseTo(0.03);
    expect(turn.parts.map((p) => p.type)).toEqual(["reasoning", "text", "dynamic-tool", "dynamic-tool", "dynamic-tool", "text"]);
    expect(turn.parts[2]).toEqual({
      type: "dynamic-tool",
      toolName: "bash",
      toolCallId: "c1",
      input: { command: "ls" },
      state: "output-available",
      output: "a\nb",
    });
    expect(turn.parts[3]).toMatchObject({ state: "output-error", errorText: "no such file", toolName: "read" });
    expect(turn.parts[4]).toMatchObject({ state: "input-available", toolCallId: "c3" });
    // Every entry (results shown with their calls included) is found in the message showing it.
    expect(t.messageOf).toEqual({
      u1: "u1",
      a1: "a1",
      r1: "a1",
      r2: "a1",
      a2: "a1",
      "a1#c1": "a1",
      "a1#c2": "a1",
      "a1#c3": "a1",
    });
  });

  it("shows a result without its call on its own", () => {
    const t = buildTranscript(input([result("r9", null, "orphan", { tool: "bash" })]));
    expect(t.messages[0]?.parts[0]).toMatchObject({ type: "dynamic-tool", toolName: "bash", toolCallId: "r9", input: null, output: "orphan" });
  });

  it("replaces a call that opened a dialog with the dialog's card", () => {
    const q = dialog("q1");
    const ap = dialog("ap", { kind: "approval", proposal_id: "p1" });
    const t = buildTranscript(
      input(
        [
          assistant("a1", {
            tool_calls: [
              { id: "c1", name: "ask_user", arguments: { question: "Which one?" } },
              { id: "c2", name: "propose_change", arguments: {} },
              { id: "c3", name: "ask_user", arguments: {} },
            ],
          }),
          result("r1", "c1", "{}", { dialog_id: "q1", tool: "ask_user" }),
          result("r2", "c2", "{}", { proposal_id: "p1", tool: "propose_change" }),
          result("r3", "c3", "{}", { dialog_id: "gone", tool: "ask_user" }),
        ],
        { dialogs: [q, ap] },
      ),
    );
    const parts = t.messages[0]!.parts;
    expect(parts.map((p) => p.type)).toEqual(["data-dialog", "data-dialog", "dynamic-tool"]);
    expect(parts[0]).toEqual({ type: "data-dialog", id: "q1", data: q });
    expect(parts[1]).toMatchObject({ data: { id: "ap" } });
    expect(t.unresolvedLinks).toBe(1);
  });

  it("keeps provider errors in place", () => {
    const t = buildTranscript(input([assistant("a1", { error: "rate limited" })]));
    expect(t.messages[0]?.parts).toEqual([{ type: "data-error", data: { text: "rate limited" } }]);
  });

  it("maps notes and compactions, and they end a turn", () => {
    const t = buildTranscript(
      input([
        assistant("a1", { text: "one" }),
        {
          kind: "note",
          id: "n1",
          at: "2026-10-30T10:02:00Z",
          text: " fyi ",
          from: "agent:helper",
          provenance: "trusted",
          event_kind: "message",
          delivery: "d3",
          event: "e3",
          local_time: "Fri 17:07",
          gap: null,
        },
        { kind: "compaction", id: "k1", at: null, summary: "earlier stuff" },
        assistant("a2", { text: "two" }),
      ]),
    );
    expect(t.messages.map((m) => [m.id, m.role, m.parts[0]?.type])).toEqual([
      ["a1", "assistant", "text"],
      ["n1", "system", "data-note"],
      ["k1", "system", "data-compaction"],
      ["a2", "assistant", "text"],
    ]);
    expect(t.messages[1]?.parts[0]).toMatchObject({ data: { from: "agent:helper", text: "fyi", kind: "message" } });
    expect(t.messages[1]?.metadata?.delivery).toBe("d3");
  });

  it("appends the messages the hub holds as queued user messages, cancellable by delivery", () => {
    const queued = [
      { delivery: "d7", text: "later", at: "2026-10-30T10:05:00Z", mode: "queue" as const },
      { delivery: "d8", text: "and this", at: "2026-10-30T10:06:00Z", mode: "queue" as const },
    ];
    const t = buildTranscript(input([user("u1", "hi")], { queued, busy: true }));
    expect(t.messages.slice(1)).toEqual([
      {
        id: "queued:d7",
        role: "user",
        parts: [{ type: "text", text: "later" }],
        metadata: { at: "2026-10-30T10:05:00Z", queued: true, delivery: "d7" },
      },
      {
        id: "queued:d8",
        role: "user",
        parts: [{ type: "text", text: "and this" }],
        metadata: { at: "2026-10-30T10:06:00Z", queued: true, delivery: "d8" },
      },
    ]);
    // Delivered: gone from the hub's list, so only the transcript shows it.
    expect(buildTranscript(input([user("u1", "hi")])).messages).toHaveLength(1);
  });

  it("continues the running turn with what streams in", () => {
    const t = buildTranscript(
      input([user("u1", "go"), assistant("a1", { text: "step one" })], {
        busy: true,
        streaming: { ...noLive(), text: "step tw", thinking: "hmm", ts: 7 },
      }),
    );
    expect(t.status).toBe("streaming");
    expect(t.messages).toHaveLength(2);
    expect(t.messages[1]?.metadata?.live).toBe(true);
    expect(t.messages[1]?.parts.slice(1)).toEqual([
      { type: "reasoning", text: "hmm", state: "done" },
      { type: "text", text: "step tw", state: "streaming" },
    ]);
  });

  it("starts a new message when nothing is running yet, and reasons before writing", () => {
    const t = buildTranscript(input([user("u1", "go")], { busy: true, streaming: { ...noLive(), thinking: "hmm" } }));
    expect(t.messages[1]).toEqual({
      id: "live",
      role: "assistant",
      parts: [{ type: "reasoning", text: "hmm", state: "streaming" }],
      metadata: { at: null, live: true, key: "reply:u1" },
    });
  });

  it("is submitted while the agent works without streaming", () => {
    expect(buildTranscript(input([user("u1", "go")], { busy: true })).status).toBe("submitted");
  });
});

describe("toolRunning", () => {
  it("runs only while the agent is busy and has no result", () => {
    const t = buildTranscript(input([assistant("a1", { tool_calls: [{ id: "c", name: "bash", arguments: {} }] })]));
    const part = t.messages[0]!.parts[0]!;
    if (part.type !== "dynamic-tool") throw new Error("not a tool");
    expect(toolRunning(part, true)).toBe(true);
    expect(toolRunning(part, false)).toBe(false);
  });
});

describe("streamed to persisted", () => {
  // One turn: thinking, then a tool call (step 1, persisted), then thinking and the answer (step 2).
  const step1 = assistant("a1", { thinking: "plan", tool_calls: [{ id: "c1", name: "bash", arguments: {} }], message_ts: 100 });
  const done1 = result("r1", "c1", "ok");
  const live = { ...noLive(), thinking: "so", text: "The answer", ts: 200 };
  const view = (items: SessionItem[], streaming = live, busy = true) => buildTranscript(input(items, { streaming, busy }));
  const shape = (t: ReturnType<typeof view>) =>
    t.messages.map((m) => [m.metadata?.key ?? m.id, m.parts.map((p) => `${p.type}:${"text" in p ? p.text : ""}`)]);

  it("keeps the streamed answer until the session has its entry, then shows that in the same place", () => {
    const streaming = view([user("u1", "go"), step1, done1]);
    // The turn ended (message_end, agent settled), the session is not refreshed yet: still shown.
    const ended = view([user("u1", "go"), step1, done1], { ...live, ended: true }, false);
    const persistedView = view(
      [user("u1", "go"), step1, done1, assistant("a2", { thinking: "so", text: "The answer", message_ts: 200 })],
      { ...live, ended: true },
      false,
    );
    const expected = [
      ["u1", ["text:go"]],
      ["reply:u1", ["reasoning:plan", "dynamic-tool:", "reasoning:so", "text:The answer"]],
    ];
    expect(shape(streaming)).toEqual(expected);
    expect(shape(ended)).toEqual(expected);
    expect(shape(persistedView)).toEqual(expected);
    expect([streaming.status, ended.status, persistedView.status]).toEqual(["streaming", "ready", "ready"]);
    // Not shown twice once persisted.
    expect(persistedView.messages[1]!.metadata?.live).toBeUndefined();
  });

  it("keys a turn that starts streaming like the persisted turn", () => {
    const first = view([user("u1", "go")], { ...noLive(), thinking: "hm", ts: 300 });
    const later = view([user("u1", "go"), assistant("a9", { thinking: "hm", message_ts: 300 })], { ...noLive(), thinking: "hm", ts: 300 });
    expect(first.messages[1]!.metadata?.key).toBe("reply:u1");
    expect(later.messages[1]!.metadata?.key).toBe("reply:u1");
    expect(first.messages[1]!.parts[0]).toMatchObject({ type: "reasoning", state: "streaming" });
  });
});

describe("attachments", () => {
  it("carries the files sent with a message, and none otherwise", () => {
    const t = buildTranscript(input([user("u1", "look", { files: ["abc/photo.png"] }), user("u2", "plain")]));
    expect(t.messages.map((m) => m.metadata?.files)).toEqual([["abc/photo.png"], undefined]);
  });
});

describe("search refs", () => {
  it("lead to the tool call: `<entry>#<call>` for the call, the result's entry for its result", () => {
    const t = buildTranscript(
      input([
        user("u1", "go"),
        assistant("a1", { text: "looking", tool_calls: [{ id: "c1", name: "web_search", arguments: {} }, { id: null, name: "bash", arguments: {} }] }),
        result("r1", "c1", "found"),
      ]),
    );
    expect(t.toolOf).toEqual({ "a1#c1": "c1", r1: "c1", "a1#1": "a1:1" });
    expect([t.messageOf["a1#c1"], t.messageOf.r1, t.messageOf.a1]).toEqual(["a1", "a1", "a1"]);
  });
});
