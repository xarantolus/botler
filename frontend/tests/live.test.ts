// @vitest-environment happy-dom
// The live stream is the only source of changes while it is up: `ready` brings the state,
// patches change it, refreshes go through it, and nothing is fetched or polled.

import { afterEach, describe, expect, it, vi } from "vitest";

const fetched = vi.hoisted(() => [] as string[]);
vi.mock("../src/api/client", async (orig) => ({
  ...(await orig<typeof import("../src/api/client")>()),
  call: vi.fn(async (name: string) => {
    fetched.push(name);
    return name === "events" ? [] : {};
  }),
}));

class FakeSocket {
  static last: FakeSocket | null = null;
  static OPEN = 1;
  readyState = 1;
  sent: Record<string, unknown>[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  constructor() {
    FakeSocket.last = this;
    queueMicrotask(() => this.onopen?.());
  }
  send(text: string) {
    this.sent.push(JSON.parse(text));
  }
  close() {}
  receive(msg: unknown) {
    this.onmessage?.({ data: JSON.stringify(msg) });
  }
}
vi.stubGlobal("WebSocket", FakeSocket);

import { startLive, stopLive } from "../src/live";
import { refreshInstances, state } from "../src/store";

const chat = (name: string, title: string) => ({ name, title }) as never;
const patch = (p: Record<string, unknown>) => ({ instances: [], all_instances: false, removed_instances: [], ...p });

afterEach(() => stopLive());

describe("live stream", () => {
  it("applies the state and its patches, refreshes through the stream, and polls nothing", async () => {
    vi.useFakeTimers();
    startLive();
    await vi.advanceTimersByTimeAsync(0);
    const ws = FakeSocket.last!;
    expect(ws.sent[0]).toMatchObject({ type: "auth" });
    ws.receive({ kind: "ready", state: patch({ status: { commit: "c1" }, instances: [chat("a", "A"), chat("b", "B")], all_instances: true }) });
    expect(state.connection).toBe("live");
    expect(state.instances.map((i) => i.title)).toEqual(["A", "B"]);

    // One row changed, one gone.
    ws.receive({ kind: "state", patch: patch({ instances: [chat("a", "A2")], removed_instances: ["b"] }) });
    expect(state.instances.map((i) => i.title)).toEqual(["A2"]);

    // A refresh asks the stream and resolves with its answer.
    let done = false;
    void refreshInstances().then(() => (done = true));
    const ask = ws.sent.at(-1)!;
    expect(ask).toMatchObject({ type: "refresh" });
    await vi.advanceTimersByTimeAsync(0);
    expect(done).toBe(false);
    ws.receive({ kind: "state", patch: patch({ refresh: ask.id }) });
    await vi.advanceTimersByTimeAsync(0);
    expect(done).toBe(true);

    // Only the latest events were fetched; minutes pass with nothing fetched or polled.
    expect(fetched).toEqual(["events"]);
    await vi.advanceTimersByTimeAsync(5 * 60_000);
    expect(fetched).toEqual(["events"]);
    vi.useRealTimers();
  });
});
