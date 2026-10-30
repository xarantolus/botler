// App state: one reactive object, refreshed from the API and patched by the live stream.

import { computed, reactive } from "vue";
import { call, getToken, onUnauthorized, setToken, Unauthorized } from "./api/client";
import type {
  AgentStatus,
  BotlerEvent,
  Dialog,
  Dialogs,
  HubStatus,
  Instance,
  SessionView,
  Usage,
} from "./generated";
import { currentStatus } from "./lib/phase";
import { notifyError } from "./notify";

export type Connection = "connecting" | "live" | "polling";

export interface AppState {
  authed: boolean;
  connection: Connection;
  status: HubStatus | null;
  usage: Usage | null;
  instances: Instance[];
  dialogs: Dialogs | null;
  events: BotlerEvent[];
  /** Latest known status per chat: from the live stream, or the wake phase of a fetch. */
  liveStatus: Record<string, AgentStatus>;
  /** Messages the user sent that are not in the transcript yet (queued for delivery). */
  outbox: Record<string, { text: string; at: number }[]>;
  /** When we first saw a chat waking (for elapsed time). */
  wakeStarted: Record<string, number>;
  session: SessionView | null;
  sessionError: string | null;
  /** Assistant text and thinking streaming in for the viewed chat. */
  streaming: { text: string; thinking: string };
  showArchived: boolean;
}

// Typed as the plain interface: the data holds no refs, and unwrapping the recursive JSON types
// through reactive() is too deep for the type checker.
const plainReactive = reactive as <T extends object>(target: T) => T;
export const state = plainReactive<AppState>({
  authed: getToken() !== null,
  connection: "connecting",
  status: null,
  usage: null,
  instances: [],
  dialogs: null,
  events: [],
  liveStatus: {},
  outbox: {},
  wakeStarted: {},
  session: null,
  sessionError: null,
  streaming: { text: "", thinking: "" },
  showArchived: false,
});

const MAX_EVENTS = 500;

/** The front-door chat, as the hub names it. */
export const frontDoorName = computed(() => state.status?.front_door ?? null);

/** Open dialogs per chat. */
export const waitingByInstance = computed(() => {
  const m: Record<string, number> = {};
  for (const d of state.dialogs?.open ?? []) m[d.instance] = (m[d.instance] ?? 0) + 1;
  return m;
});

/** Every dialog we know of (open ones first), for linking from transcripts. */
export const allDialogs = computed((): Dialog[] => {
  const dialogs = state.dialogs;
  const open: Dialog[] = dialogs?.open ?? [];
  const ids = new Set(open.map((d) => d.id));
  return [...open, ...(dialogs?.recent ?? []).filter((d) => !ids.has(d.id))];
});

onUnauthorized(() => {
  state.authed = false;
});

/** Run a refresh; report failures (except the logout that a 401 already caused). */
async function guarded(what: string, f: () => Promise<void>): Promise<void> {
  try {
    await f();
  } catch (e) {
    if (e instanceof Unauthorized) return;
    notifyError(what, e);
  }
}

export function refreshStatus() {
  return guarded("Loading hub status", async () => {
    state.status = await call("status", {});
  });
}

export function refreshUsage() {
  return guarded("Loading spend", async () => {
    state.usage = await call("usage", {});
  });
}

export function refreshInstances() {
  return guarded("Loading chats", async () => {
    state.instances = await call("instances", {});
  });
}

export function refreshDialogs() {
  return guarded("Loading requests", async () => {
    state.dialogs = await call("dialogs", {});
  });
}

export function refreshEvents() {
  return guarded("Loading events", async () => {
    const fresh = await call("events", { query: { limit: 200 } });
    const known = new Set(fresh.map((e) => e.id));
    state.events = [...fresh, ...state.events.filter((e) => !known.has(e.id))].slice(0, MAX_EVENTS);
  });
}

let sessionFor: string | null = null;

export function refreshSession(name: string) {
  sessionFor = name;
  return guarded(`Loading chat ${name}`, async () => {
    try {
      const s = await call("session", { params: { instance: name } });
      if (sessionFor !== name) return; // the user moved on
      state.session = s;
      state.sessionError = null;
      // The fetch is newer than any earlier live status.
      const now = currentStatus(s.phase, null);
      if (now) state.liveStatus[s.instance.name] = now;
      else delete state.liveStatus[s.instance.name];
      trackWake(s.instance.name, s.phase?.phase === "waking" || s.instance.state === "starting");
      settleOutbox(s);
    } catch (e) {
      if (sessionFor === name) state.sessionError = e instanceof Error ? e.message : String(e);
      throw e;
    }
  });
}

/** Remember a sent message until the transcript shows it. */
export function queueSent(instance: string, text: string) {
  (state.outbox[instance] ??= []).push({ text, at: Date.now() });
}

function settleOutbox(s: SessionView) {
  const queued = state.outbox[s.instance.name];
  if (!queued?.length) return;
  const texts = s.items.flatMap((it) => (it.kind === "user" ? [it.text] : []));
  state.outbox[s.instance.name] = queued.filter(
    (q) => !texts.some((t) => t.includes(q.text)) && !(s.pending === 0 && Date.now() - q.at > 15_000),
  );
}

export function clearSession() {
  sessionFor = null;
  state.session = null;
  state.sessionError = null;
  state.streaming = { text: "", thinking: "" };
}

export function trackWake(instance: string, waking: boolean) {
  if (waking) state.wakeStarted[instance] ??= Date.now();
  else delete state.wakeStarted[instance];
}

export function addEvent(ev: BotlerEvent) {
  if (state.events.some((e) => e.id === ev.id)) return;
  state.events.unshift(ev);
  if (state.events.length > MAX_EVENTS) state.events.length = MAX_EVENTS;
}

export function refreshAll() {
  return Promise.all([
    refreshStatus(),
    refreshUsage(),
    refreshInstances(),
    refreshDialogs(),
    refreshEvents(),
    sessionFor ? refreshSession(sessionFor) : Promise.resolve(),
  ]);
}

export async function login(token: string): Promise<void> {
  setToken(token);
  // Throws (and clears the token) when it is rejected.
  state.status = await call("status", {});
  state.authed = true;
}

export function logout() {
  setToken(null);
  state.authed = false;
}
