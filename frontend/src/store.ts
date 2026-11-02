// App state: one reactive object, refreshed from the API and patched by the live stream.

import { computed, reactive, shallowRef, watch } from "vue";
import { call, getToken, onUnauthorized, setToken, Unauthorized } from "./api/client";
import type {
  AgentStatus,
  AppView,
  BotlerEvent,
  Dialog,
  Dialogs,
  HubStatus,
  Instance,
  Proposal,
  SessionItem,
  SessionView,
  StatePatch,
  Usage,
} from "./generated";
import { currentStatus } from "./lib/phase";
import { noReferences, references, type References } from "./lib/references";
import { noLive, persisted, type LiveMessage } from "./lib/transcript";
import { notifyError } from "./notify";

export type Connection = "connecting" | "live" | "offline";

export interface AppState {
  authed: boolean;
  /** Whether the start-up check for a session has finished (until then, show nothing). */
  authChecked: boolean;
  /** Why the hub did not answer the sign-in check (it is retried), if it did not. */
  unreachable: string | null;
  connection: Connection;
  status: HubStatus | null;
  usage: Usage | null;
  instances: Instance[];
  dialogs: Dialogs | null;
  /** The latest proposals, newest first; patched live as they move. */
  proposals: Proposal[] | null;
  /** Services' web UIs (`[app]`). */
  apps: AppView[] | null;
  events: BotlerEvent[];
  /** Latest known status per chat: from the live stream, or the wake phase of a fetch. */
  liveStatus: Record<string, AgentStatus>;
  /** When we first saw a chat waking (for elapsed time). */
  wakeStarted: Record<string, number>;
  session: SessionView | null;
  sessionError: string | null;
  /** The assistant message streaming in for the viewed chat. */
  streaming: LiveMessage;
  /** The viewed chat's conversation is being compacted (pi's compaction_start): since when, why. */
  compacting: { since: number; reason: string } | null;
  showArchived: boolean;
}

// Typed as the plain interface: the data holds no refs, and unwrapping the recursive JSON types
// through reactive() is too deep for the type checker.
const plainReactive = reactive as <T extends object>(target: T) => T;
export const state = plainReactive<AppState>({
  authed: false,
  authChecked: false,
  unreachable: null,
  connection: "connecting",
  status: null,
  usage: null,
  instances: [],
  dialogs: null,
  proposals: null,
  apps: null,
  events: [],
  liveStatus: {},
  wakeStarted: {},
  session: null,
  sessionError: null,
  streaming: noLive(),
  compacting: null,
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

/** While the live stream is up, the shared state (status, spend, chats, requests, apps,
 * proposals) changes only through it: a refresh asks it for what changed (live.ts), so a slow
 * answer can never overwrite a newer patch. Null while it is down: refreshes fetch. */
let viaLive: (() => Promise<void>) | null = null;
export function refreshViaLive(f: typeof viaLive) {
  viaLive = f;
}

/** Apply the live stream's state: the whole of it (`ready`) or what changed. */
export function applyState(p: StatePatch) {
  if (p.status) state.status = p.status;
  if (p.usage) state.usage = p.usage;
  if (p.dialogs) state.dialogs = p.dialogs;
  if (p.apps) state.apps = p.apps;
  if (p.proposals) state.proposals = p.proposals;
  if (p.all_instances) state.instances = p.instances;
  else if (p.instances.length || p.removed_instances.length) {
    const replaced = new Set([...p.removed_instances, ...p.instances.map((i) => i.name)]);
    state.instances = [...state.instances.filter((i) => !replaced.has(i.name)), ...p.instances];
  }
}

export function refreshStatus() {
  if (viaLive) return viaLive();
  return guarded("Loading hub status", async () => {
    state.status = await call("status", {});
  });
}

export function refreshUsage() {
  if (viaLive) return viaLive();
  return guarded("Loading spend", async () => {
    state.usage = await call("usage", {});
  });
}

export function refreshInstances() {
  if (viaLive) return viaLive();
  return guarded("Loading chats", async () => {
    state.instances = await call("instances", {});
  });
}

// Refreshes overlap (the live stream and an answer both ask): only the newest response is
// applied, so a slow older one cannot bring back a dialog that was just answered.
let dialogsSeq = 0;
let proposalsSeq = 0;

export function refreshDialogs() {
  if (viaLive) return viaLive();
  const seq = ++dialogsSeq;
  return guarded("Loading requests", async () => {
    const dialogs = await call("dialogs", {});
    if (seq === dialogsSeq) state.dialogs = dialogs;
  });
}

export function refreshApps() {
  if (viaLive) return viaLive();
  return guarded("Loading apps", async () => {
    state.apps = await call("apps", {});
  });
}

export function refreshProposals() {
  if (viaLive) return viaLive();
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
    const fresh = await call("events", { query: { limit: 100 } });
    // Events pushed meanwhile may be newer: merged, newest first (ids are time-ordered).
    const known = new Set(fresh.map((e) => e.id));
    const all = [...fresh, ...state.events.filter((e) => !known.has(e.id))];
    state.events = all.sort((a, b) => (a.id < b.id ? 1 : a.id > b.id ? -1 : 0)).slice(0, MAX_EVENTS);
  });
}

let sessionFor: string | null = null;
/** An earlier session file of that chat being viewed (from a handover), or null for the current. */
let sessionFile: string | null = null;
/** Session items a chat loads at first, and how many more each step back adds. */
export const SESSION_PAGE = 50;

const lastId = (items: SessionItem[]) => items[items.length - 1]?.id;

let earlierSeq = 0;
/** Load the chat on screen further back: the page before its first item (all of it with `all`,
 * e.g. to reach a link). A page that no longer adjoins what is shown (the chat or its branch
 * changed meanwhile) is dropped. */
export function showEarlier(name: string, all = false) {
  const s = state.session;
  if (all || s?.instance.name !== name || !s.items.length) return refreshSession(name, all ? "all" : "latest");
  const first = s.items[0]!.id;
  const file = sessionFile;
  const seq = ++earlierSeq;
  return guarded(`Loading chat ${name}`, async () => {
    const page = await call("session", { params: { instance: name }, query: { file: file ?? undefined, last: SESSION_PAGE, before: first } });
    const now = state.session;
    if (seq !== earlierSeq || sessionFor !== name || sessionFile !== file || now?.items[0]?.id !== first || !page.continues) return;
    now.items = [...page.items, ...now.items];
    now.earlier = page.earlier;
  });
}

/** View one of the chat's session files (null: the current one). */
export function showSessionFile(name: string, file: string | null) {
  sessionFor = name; // so refreshSession keeps the file
  sessionFile = file;
  return refreshSession(name, "latest");
}

let sessionSeq = 0;
/** Bring the chat on screen up to date. `new`: only the items after the last one shown (the
 * latest page when none is shown, or its branch moved); `latest`: the latest page afresh; `all`:
 * the whole chat. */
export function refreshSession(name: string, mode: "new" | "latest" | "all" = "new") {
  if (sessionFor !== name) {
    sessionFile = null;
    if (mode === "new") mode = "latest";
  }
  sessionFor = name;
  const shown = state.session?.instance.name === name ? state.session : null;
  const after = mode === "new" && shown ? lastId(shown.items) : undefined;
  const seq = ++sessionSeq;
  return guarded(`Loading chat ${name}`, async () => {
    try {
      const s = await call("session", {
        params: { instance: name },
        query: { file: sessionFile ?? undefined, last: mode === "all" ? undefined : SESSION_PAGE, after },
      });
      // The user moved on, or a later refresh or push came (an answer may overtake an older one).
      if (sessionFor !== name || seq !== sessionSeq) return;
      if (!applySession(after ?? null, s)) return;
      sessionLoaded?.(name, shownLast(name));
    } catch (e) {
      if (sessionFor === name) state.sessionError = e instanceof Error ? e.message : String(e);
      throw e;
    }
  });
}

/** Show `s`: the items after item `after` (null: a page to show instead). False when they do
 * not continue what is shown (it moved on meanwhile): nothing changes. */
function applySession(after: string | null, s: SessionView): boolean {
  const prev = state.session;
  if (after !== null && s.continues) {
    if (!prev || prev.instance.name !== s.instance.name || lastId(prev.items) !== after) return false;
    state.session = { ...s, items: [...prev.items, ...s.items], earlier: prev.earlier };
  } else state.session = s;
  // The session has the streamed message now: show it from there.
  if (persisted(state.session.items, state.streaming)) state.streaming = noLive();
  state.sessionError = null;
  // The session is newer than any earlier live status.
  const now = currentStatus(s.phase, null);
  if (now) state.liveStatus[s.instance.name] = now;
  else delete state.liveStatus[s.instance.name];
  trackWake(s.instance.name, s.phase?.phase === "waking" || s.instance.state === "starting");
  return true;
}

/** Told when a chat's items were loaded over HTTP, with its last item (the live stream then
 * follows from there). */
let sessionLoaded: ((name: string, last: string | null) => void) | null = null;
export function onSessionLoaded(f: typeof sessionLoaded) {
  sessionLoaded = f;
}

/** The live stream's update of the chat on screen (`session`): applied when it continues what is
 * shown, and newer than any answer still on its way. False when it does not continue it (the
 * stream should follow from what is shown instead). */
export function pushedSession(after: string | null, s: SessionView): boolean {
  // Another chat, or an older session file of it on screen: not for what is shown.
  if (sessionFor !== s.instance.name || sessionFile !== null) return true;
  if (!applySession(after, s)) return false;
  sessionSeq++;
  return true;
}

/** The last item of the chat on screen, if any. */
export function shownLast(name: string): string | null {
  return state.session?.instance.name === name ? (lastId(state.session.items) ?? null) : null;
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

/** Fetch everything (no live stream to bring it). */
export function refreshAll() {
  return Promise.all([
    refreshStatus(),
    refreshUsage(),
    refreshInstances(),
    refreshDialogs(),
    refreshProposals(),
    refreshApps(),
    refreshEvents(),
    sessionFor ? refreshSession(sessionFor) : Promise.resolve(),
  ]);
}

/** Whether this browser is signed in (a session cookie or a stored token): checked at start. A
 * hub that does not answer (restarting, or the network) is retried, not taken for signed out. */
export async function checkSignedIn(): Promise<void> {
  try {
    state.status = await call("status", {});
    state.authed = true;
    state.unreachable = null;
    state.authChecked = true;
  } catch (e) {
    if (e instanceof Unauthorized) {
      state.authed = false;
      state.unreachable = null;
      state.authChecked = true;
      return;
    }
    state.unreachable = e instanceof Error ? e.message : String(e);
    setTimeout(() => void checkSignedIn(), 3000);
  }
}

/** Sign this browser in with a one-time code or a user token: the hub sets a session cookie. */
export async function login(secret: string, name: string): Promise<void> {
  setToken(null);
  await call("login", { body: { secret, name } });
  state.status = await call("status", {});
  state.authed = true;
}

export async function logout(): Promise<void> {
  try {
    if (!getToken()) await call("logout", {});
  } finally {
    setToken(null);
    state.authed = false;
  }
}
