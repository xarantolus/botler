// The live stream (`/api/v1/live`, see crates/botler-hub/src/live.rs): the shared state with
// `ready`, then what changes, pushed (`state` patches, events, the chat on screen); nothing is
// polled. While it is down the app shows what it has (the connection badge says so) and
// reconnects with backoff; `ready` brings everything again. Only a first connection that fails
// fetches over HTTP, so the app is not empty behind a proxy that refuses WebSockets.

import { getToken } from "./api/client";
import type { ClientMessage, ServerMessage } from "./generated";
import { noLive } from "./lib/transcript";
import {
  addEvent,
  applyState,
  onSessionLoaded,
  pushedSession,
  refreshAll,
  refreshEvents,
  refreshViaLive,
  shownLast,
  state,
  trackWake,
  upsertProposal,
} from "./store";

let socket: WebSocket | null = null;
let backoff = 1000;
let reconnectTimer: number | undefined;
let watched: string | null = null;
let stopped = true;

/** Whether a connection was ever ready (else a failed one fetches over HTTP once). */
let wasReady = false;
/** Refreshes asked of the stream, waiting for the `state` that answers them. */
let refreshId = 0;
const refreshing = new Map<number, () => void>();
function settleRefreshes(upTo = Infinity) {
  for (const [id, done] of refreshing) {
    if (id > upTo) continue;
    refreshing.delete(id);
    done();
  }
}
function refreshLive(): Promise<void> {
  return new Promise((done) => {
    const id = ++refreshId;
    refreshing.set(id, done);
    send({ type: "refresh", id });
  });
}

function send(msg: ClientMessage) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg));
}

/** Follow one chat (or none): its pi events, and its session's new items once its first page
 * is loaded (`onSessionLoaded`). */
export function watch(instance: string | null) {
  watched = instance;
  state.streaming = noLive();
  state.compacting = null;
  send({ type: "watch", instance });
}

/** Have the stream continue the chat on screen from its last item. */
function follow() {
  if (watched) send({ type: "watch", instance: watched, after: shownLast(watched) ?? undefined });
}
onSessionLoaded((name) => {
  if (name === watched) follow();
});

/** One of pi's events for a chat (exported for tests). */
export function onPi(instance: string, ev: Record<string, unknown>) {
  if (instance !== watched) return;
  const type = ev.type;
  const message = ev.message as { role?: unknown; timestamp?: unknown } | undefined;
  if (type === "message_start" && message?.role === "assistant") {
    // A new message replaces the last one (which the session has by now, or will have).
    state.streaming = { ...noLive(), ts: typeof message.timestamp === "number" ? message.timestamp : null };
    return;
  }
  if (type === "compaction_start") {
    state.compacting = { since: Date.now(), reason: typeof ev.reason === "string" ? ev.reason : "threshold" };
    return;
  }
  if (type === "compaction_end") {
    state.compacting = null;
    return;
  }
  if (type === "message_update") {
    const delta = ev.assistantMessageEvent as { type?: unknown; delta?: unknown } | undefined;
    if (typeof delta?.delta !== "string") return;
    if (delta.type === "text_delta") state.streaming.text += delta.delta;
    else if (delta.type === "thinking_delta") state.streaming.thinking += delta.delta;
    return;
  }
  // Kept until the refreshed session has the message (store.ts hands it over), which it finds by
  // the timestamp: take it from here when the chat was opened after the message started.
  if (type === "message_end" && message?.role === "assistant") {
    state.streaming.ended = true;
    if (typeof message.timestamp === "number") state.streaming.ts ??= message.timestamp;
  }
  // What pi writes to the session arrives as `session`.
}

function onMessage(msg: ServerMessage) {
  switch (msg.kind) {
    case "ready":
      backoff = 1000;
      wasReady = true;
      applyState(msg.state);
      state.connection = "live";
      refreshViaLive(refreshLive);
      follow();
      // The latest events (the stream brings new ones; these may have passed meanwhile).
      void refreshEvents();
      return;
    case "state":
      applyState(msg.patch);
      if (msg.patch.refresh !== undefined) settleRefreshes(msg.patch.refresh);
      return;
    case "error":
      console.warn("live:", msg.error);
      return;
    case "event":
      addEvent(msg.event);
      return;
    case "instance":
      state.liveStatus[msg.instance] = msg.status;
      trackWake(msg.instance, msg.status.status === "waking");
      return;
    case "pi":
      onPi(msg.instance, msg.event);
      return;
    case "session":
      // Not continuing what is shown (an answer overtook it, or one was missed): follow from it.
      if (!pushedSession(msg.after, msg.view)) follow();
      return;
    case "proposal":
      upsertProposal(msg.proposal);
      return;
  }
}

function connect() {
  if (stopped) return;
  // A signed-in device authenticates with its cookie (an empty token); a stored token still works.
  const token = getToken() ?? "";
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/api/v1/live`);
  socket = ws;
  ws.onopen = () => ws.send(JSON.stringify({ type: "auth", token } satisfies ClientMessage));
  ws.onmessage = (e) => {
    let msg: ServerMessage;
    try {
      msg = JSON.parse(String(e.data)) as ServerMessage;
    } catch (err) {
      console.error("live: unreadable message", err);
      return;
    }
    onMessage(msg);
  };
  ws.onclose = () => {
    if (socket !== ws) return;
    socket = null;
    refreshViaLive(null);
    settleRefreshes();
    state.connection = "offline";
    if (stopped) return;
    if (!wasReady) {
      wasReady = true;
      void refreshAll();
    }
    reconnectTimer = window.setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 10_000);
  };
}

/** Back from the background (a phone, the installed app) or online again: reconnect now instead
 * of after the backoff (its `ready` catches up). A socket that stayed open carried everything but
 * maybe a moment of the chat on screen: only its new items are asked for. */
function wakeUp() {
  if (stopped || document.hidden) return;
  if (socket) {
    follow();
    return;
  }
  window.clearTimeout(reconnectTimer);
  backoff = 1000;
  connect();
}

export function startLive() {
  stopped = false;
  state.connection = "connecting";
  connect();
  document.addEventListener("visibilitychange", wakeUp);
  window.addEventListener("online", wakeUp);
}

export function stopLive() {
  stopped = true;
  document.removeEventListener("visibilitychange", wakeUp);
  window.removeEventListener("online", wakeUp);
  window.clearTimeout(reconnectTimer);
  refreshViaLive(null);
  settleRefreshes();
  socket?.close();
  socket = null;
}
