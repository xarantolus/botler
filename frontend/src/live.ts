// The live stream (`/api/v1/live`, see crates/botler-hub/src/live.rs) with polling as the
// fallback: while the socket is down the app polls, and it reconnects with exponential backoff.

import { getToken } from "./api/client";
import type { ClientMessage, ServerMessage } from "./generated";
import {
  addEvent,
  refreshAll,
  refreshDialogs,
  refreshEvents,
  refreshInstances,
  refreshSession,
  refreshStatus,
  refreshUsage,
  state,
  trackWake,
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

/** Follow one chat's pi events (or none). */
export function watch(instance: string | null) {
  watched = instance;
  state.streaming = { text: "", thinking: "" };
  send({ type: "watch", instance });
}

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
    every(15_000, () => watched && refreshSession(watched));
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

function onPi(instance: string, ev: Record<string, unknown>) {
  if (instance !== watched) return;
  const type = ev.type;
  if (type === "message_update") {
    const delta = ev.assistantMessageEvent as { type?: unknown; delta?: unknown } | undefined;
    if (typeof delta?.delta !== "string") return;
    if (delta.type === "text_delta") state.streaming.text += delta.delta;
    else if (delta.type === "thinking_delta") state.streaming.thinking += delta.delta;
    return;
  }
  if (type === "message_end") state.streaming = { text: "", thinking: "" };
  if (
    type === "message_end" ||
    type === "agent_start" ||
    type === "agent_end" ||
    type === "agent_settled" ||
    type === "tool_execution_end"
  ) {
    soon("session", () => refreshSession(instance));
  }
}

function onMessage(msg: ServerMessage) {
  switch (msg.kind) {
    case "ready":
      backoff = 1000;
      setConnection("live");
      if (watched) send({ type: "watch", instance: watched });
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
      if (watched && (msg.event.to === watched || msg.event.source.kind === "user")) {
        soon("session", () => watched && refreshSession(watched), 500);
      }
      return;
    }
    case "instance":
      state.liveStatus[msg.instance] = msg.status;
      trackWake(msg.instance, msg.status.status === "waking");
      soon("instances", refreshInstances, 500);
      if (msg.instance === watched && msg.status.status !== "waking") soon("session", () => refreshSession(msg.instance));
      return;
    case "dialog":
      soon("dialogs", refreshDialogs, 100);
      soon("status", refreshStatus);
      return;
    case "pi":
      onPi(msg.instance, msg.event);
      return;
  }
}

function connect() {
  if (stopped) return;
  const token = getToken();
  if (!token) return;
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

export function startLive() {
  stopped = false;
  state.connection = "connecting";
  startPolling();
  connect();
}

export function stopLive() {
  stopped = true;
  window.clearTimeout(reconnectTimer);
  stopPolling();
  socket?.close();
  socket = null;
}
