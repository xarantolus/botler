// The chat on screen loads in pages: new items after the last one shown, earlier ones before the
// first; an answer that no longer adjoins what is shown (slow, overtaken) is dropped or replaces.

import { describe, expect, it, vi } from "vitest";

type Query = { after?: string; before?: string; last?: number };
/** Session calls waiting for an answer, in order. */
const pending = vi.hoisted(() => [] as { query: Query; answer: (v: unknown) => void }[]);
vi.mock("../src/api/client", async (orig) => ({
  ...(await orig<typeof import("../src/api/client")>()),
  call: vi.fn(
    (name: string, args: { query?: Query }) =>
      new Promise((answer) => {
        if (name !== "session") throw new Error(`unexpected call ${name}`);
        pending.push({ query: args.query ?? {}, answer });
      }),
  ),
}));

import { refreshSession, showEarlier, state } from "../src/store";

const item = (id: string) => ({ kind: "note", id, at: null, text: id, from: "x", provenance: "trusted", event_kind: "message", delivery: "d", event: "e", local_time: null, gap: null });
const view = (ids: string[], earlier: number, continues: boolean) => ({
  instance: { name: "c" },
  busy: false,
  pending: 0,
  phase: null,
  items: ids.map(item),
  earlier,
  continues,
  tool_renders: {},
  queued: [],
  sessions: [],
  file: "s.jsonl",
  handover: null,
});
const ids = () => state.session?.items.map((i) => i.id);
async function answer(i: number, v: unknown) {
  pending.splice(i, 1)[0]!.answer(v);
  await new Promise((r) => setTimeout(r));
}

describe("session pages", () => {
  it("appends new items, prepends earlier ones, and drops what no longer adjoins", async () => {
    void refreshSession("c");
    expect(pending[0]!.query).toMatchObject({ last: 50, after: undefined });
    await answer(0, view(["a", "b"], 2, false));
    expect([ids(), state.session?.earlier]).toEqual([["a", "b"], 2]);

    // New items after the last one shown.
    void refreshSession("c");
    expect(pending[0]!.query.after).toBe("b");
    await answer(0, view(["c"], 2, true));
    expect([ids(), state.session?.earlier]).toEqual([["a", "b", "c"], 2]);

    // Earlier items before the first one shown.
    void showEarlier("c");
    expect(pending[0]!.query).toMatchObject({ before: "a", last: 50 });
    await answer(0, view(["x", "y"], 0, true));
    expect([ids(), state.session?.earlier]).toEqual([["x", "y", "a", "b", "c"], 0]);

    // The branch moved while an earlier page was on its way: the latest page replaces what is
    // shown, and the earlier page (before "x", no longer shown) is dropped.
    state.session!.earlier = 3;
    void showEarlier("c");
    void refreshSession("c");
    await answer(1, view(["n1", "n2"], 9, false));
    expect(ids()).toEqual(["n1", "n2"]);
    await answer(0, view(["w"], 2, true));
    expect([ids(), state.session?.earlier]).toEqual([["n1", "n2"], 9]);

    // A slow refresh overtaken by a newer one is not applied.
    void refreshSession("c");
    void refreshSession("c");
    await answer(1, view(["n3"], 9, true));
    await answer(0, view(["n3"], 9, true));
    expect(ids()).toEqual(["n1", "n2", "n3"]);
  });
});
