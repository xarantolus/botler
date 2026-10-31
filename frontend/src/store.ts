// App state: one reactive object, refreshed from the API and patched by the live stream.

import { computed, reactive, shallowRef, watch } from "vue";
import { call, getToken, onUnauthorized, setToken, Unauthorized } from "./api/client";
import type {
  AgentStatus,
  BotlerEvent,
  Dialog,
  Dialogs,
  HubStatus,
  Instance,
  Proposal,
  SessionView,
  Usage,
} from "./generated";
import { currentStatus } from "./lib/phase";
import { noReferences, references, type References } from "./lib/references";
import { noLive, persisted, type LiveMessage } from "./lib/transcript";
import { notifyError } from "./notify";

export type Connection = "connecting" | "live" | "polling";

export interface AppState {
  authed: boolean;
  connection: Connection;
  status: HubStatus | null;
  usage: Usage | null;
  instances: Instance[];
  dialogs: Dialogs | null;
  /** The latest proposals, newest first; patched live as they move. */
  proposals: Proposal[] | null;
  events: BotlerEvent[];
  /** Latest known status per chat: from the live stream, or the wake phase of a fetch. */
  liveStatus: Record<string, AgentStatus>;
  /** When we first saw a chat waking (for elapsed time). */
  wakeStarted: Record<string, number>;
  session: SessionView | null;
  sessionError: string | null;
  /** The assistant message streaming in for the viewed chat. */
  streaming: LiveMessage;
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
  proposals: null,
  events: [],
  liveStatus: {},
  wakeStarted: {},
  session: null,
  sessionError: null,
  streaming: noLive(),
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

/**
 * The names and ids rendered Markdown links (lib/references.ts). Replaced only when that set
 * changes, not on every refresh: each change renders every Markdown text again.
 */
export const knownReferences = shallowRef<References>(noReferences);
watch(
  () => [state.instances, state.proposals, state.dialogs, state.status] as const,
  ([instances, proposals, dialogs, status]) => {
    // Once everything has loaded, not once per part (each would render the open chat again).
    if (!proposals || !dialogs || !status) return;
    const next = references({
      instances,
      proposals,
      dialogs: [...dialogs.open, ...dialogs.recent],
      jobs: status.jobs,
      agents: status.agents,
      secretGroups: status.secret_groups,
    });
    if (next.key !== knownReferences.value.key) knownReferences.value = next;
  },
);

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

// Refreshes overlap (the live stream and an answer both ask): only the newest response is
// applied, so a slow older one cannot bring back a dialog that was just answered.
let dialogsSeq = 0;
let proposalsSeq = 0;

export function refreshDialogs() {
  const seq = ++dialogsSeq;
  return guarded("Loading requests", async () => {
    const dialogs = await call("dialogs", {});
    if (seq === dialogsSeq) state.dialogs = dialogs;
  });
}

export function refreshProposals() {
  const seq = ++proposalsSeq;
  return guarded("Loading proposals", async () => {
    const proposals = await call("proposals", {});
    if (seq === proposalsSeq) state.proposals = proposals;
  });
}

/** A proposal moved (live): replace or add it. */
export function upsertProposal(p: Proposal) {
  const list = state.proposals ?? [];
  const i = list.findIndex((x) => x.id === p.id);
  if (i >= 0) list.splice(i, 1, p);
  else list.unshift(p);
  state.proposals = list;
}

export function refreshEvents() {
  return guarded("Loading events", async () => {
    const fresh = await call("events", { query: { limit: 200 } });
    const known = new Set(fresh.map((e) => e.id));
    state.events = [...fresh, ...state.events.filter((e) => !known.has(e.id))].slice(0, MAX_EVENTS);
  });
}

let sessionFor: string | null = null;
/** An earlier session file of that chat being viewed (from a handover), or null for the current. */
let sessionFile: string | null = null;

/** View one of the chat's session files (null: the current one). */
export function showSessionFile(name: string, file: string | null) {
  sessionFor = name; // so refreshSession keeps the file
  sessionFile = file;
  return refreshSession(name);
}

export function refreshSession(name: string) {
  if (sessionFor !== name) sessionFile = null;
  sessionFor = name;
  return guarded(`Loading chat ${name}`, async () => {
    try {
      const s = await call("session", { params: { instance: name }, query: { file: sessionFile ?? undefined } });
      if (sessionFor !== name) return; // the user moved on
      state.session = s;
      // The session has the streamed message now: show it from there.
      if (persisted(s.items, state.streaming)) state.streaming = noLive();
      state.sessionError = null;
      // The fetch is newer than any earlier live status.
      const now = currentStatus(s.phase, null);
      if (now) state.liveStatus[s.instance.name] = now;
      else delete state.liveStatus[s.instance.name];
      trackWake(s.instance.name, s.phase?.phase === "waking" || s.instance.state === "starting");
    } catch (e) {
      if (sessionFor === name) state.sessionError = e instanceof Error ? e.message : String(e);
      throw e;
    }
  });
}

export function clearSession() {
  sessionFor = null;
  state.session = null;
  state.sessionError = null;
  state.streaming = noLive();
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
    refreshProposals(),
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
