// The live stream (`/api/v1/live`, see crates/botler-hub/src/live.rs) with polling as the
// fallback: while the socket is down the app polls, and it reconnects with exponential backoff.

import { getToken } from "./api/client";
import type { ClientMessage, ServerMessage } from "./generated";
import { noLive } from "./lib/transcript";
import {
  addEvent,
  onSessionLoaded,
  pushedSession,
  refreshAll,
  refreshDialogs,
  refreshEvents,
  refreshInstances,
  refreshSession,
  refreshStatus,
  refreshUsage,
  shownLast,
  state,
  trackWake,
  upsertProposal,
} from "./store";

let socket: WebSocket | null = null;
let backoff = 1000;
let reconnectTimer: number | undefined;
let pollTimers: number[] = [];
let watched: string | null = null;
let stopped = true;

const debounced = new Map<string, number>();
function soon(key: string, f: () => unknown, ms = 300) {
  window.clearTimeout(debounced.get(key));
  debounced.set(
    key,
    window.setTimeout(() => {
      debounced.delete(key);
      f();
    }, ms),
  );
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

function startPolling() {
  stopPolling();
  const every = (ms: number, f: () => unknown) =>
    pollTimers.push(
      window.setInterval(() => {
        if (!document.hidden) f();
      }, ms),
    );
  if (state.connection === "live") {
    // Slow refresh for what the stream does not carry (spend, queued deliveries).
    every(30_000, () => Promise.all([refreshStatus(), refreshUsage(), refreshInstances()]));
  } else {
    every(2_500, () => watched && refreshSession(watched));
    every(4_000, () => Promise.all([refreshStatus(), refreshInstances(), refreshDialogs(), refreshEvents()]));
    every(30_000, refreshUsage);
  }
}

function stopPolling() {
  for (const t of pollTimers) window.clearInterval(t);
  pollTimers = [];
}

function setConnection(c: typeof state.connection) {
  if (state.connection === c && pollTimers.length) return;
  state.connection = c;
  startPolling();
}

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
      setConnection("live");
      follow();
      // Catch up on whatever happened while disconnected.
      void refreshAll();
      return;
    case "error":
      // Authentication failed: the next API call logs out if the token is bad.
      void refreshStatus();
      return;
    case "lagged":
      void refreshAll();
      return;
    case "event": {
      addEvent(msg.event);
      const t = msg.event.type;
      if (t.startsWith("dialog.") || t === "proposal.updated") soon("dialogs", refreshDialogs);
      if (t === "proposal.updated" || t === "breaker.tripped" || t.startsWith("runnable.") || t === "manual") {
        soon("status", refreshStatus);
      }
      // New chats, titles and activity show up in the list.
      soon("instances", refreshInstances, 1000);
      return;
    }
    case "instance":
      state.liveStatus[msg.instance] = msg.status;
      trackWake(msg.instance, msg.status.status === "waking");
      soon("instances", refreshInstances, 500);
      return;
    case "dialog":
      soon("dialogs", refreshDialogs, 100);
      soon("status", refreshStatus);
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
    setConnection("polling");
    if (stopped) return;
    reconnectTimer = window.setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 30_000);
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
  startPolling();
  connect();
  document.addEventListener("visibilitychange", wakeUp);
  window.addEventListener("online", wakeUp);
}

export function stopLive() {
  stopped = true;
  document.removeEventListener("visibilitychange", wakeUp);
  window.removeEventListener("online", wakeUp);
  window.clearTimeout(reconnectTimer);
  stopPolling();
  socket?.close();
  socket = null;
}
