// What a chat is doing, as one badge: from its stored state, the hub's wake phase, the latest
// live status, whether pi is busy, and whether it waits on the user.

import type { AgentStatus, InstanceState, WakePhase, WakeStep } from "../generated";

export type BadgeType = "default" | "info" | "success" | "warning" | "error";

export interface Badge {
  label: string;
  type: BadgeType;
  /** Something is in progress (show a spinner). */
  busy: boolean;
  /** Longer explanation (tooltip / header). */
  detail?: string;
}

export const WAKE_STEPS: { step: WakeStep; label: string }[] = [
  { step: "building_image", label: "Building image" },
  { step: "preparing_workspace", label: "Preparing workspace" },
  { step: "starting_container", label: "Starting container" },
];

/** The kit's semantic color for a badge type. */
export function badgeColor(t: BadgeType): "neutral" | "info" | "success" | "warning" | "error" {
  return t === "default" ? "neutral" : t;
}

export function stepLabel(step: WakeStep): string {
  return WAKE_STEPS.find((s) => s.step === step)?.label ?? step;
}

/** The newest view of a wake: a live status overrides the phase from the last fetch. */
export function currentStatus(phase: WakePhase | null, live: AgentStatus | null): AgentStatus | null {
  if (live) return live;
  if (!phase) return null;
  if (phase.phase === "waking") return { status: "waking", step: phase.step };
  if (phase.phase === "retrying") return { status: "retrying", at: phase.at, error: phase.error };
  return { status: "failed", error: phase.error };
}

export interface ChatSituation {
  state: InstanceState;
  status: AgentStatus | null;
  busy?: boolean;
  pending?: number;
  /** Open dialogs this chat asked. */
  waitingOnUser?: number;
  archiveRequested?: boolean;
}

export function phaseBadge(s: ChatSituation): Badge {
  if (s.state === "archived") return { label: "archived", type: "default", busy: false };
  const st = s.status;
  if (st?.status === "failed") return { label: "crashed", type: "error", busy: false, detail: st.error };
  if (st?.status === "paused") return { label: "paused", type: "error", busy: false, detail: st.reason };
  if (st?.status === "retrying") {
    return { label: "retrying", type: "warning", busy: true, detail: `retrying ${retryIn(st.at)}: ${st.error}` };
  }
  if (st?.status === "waking") return { label: "starting", type: "info", busy: true, detail: stepLabel(st.step) };
  if (s.state === "starting") return { label: "starting", type: "info", busy: true };
  if (s.archiveRequested) return { label: "handing over", type: "warning", busy: true };
  const running = st ? st.status === "running" : s.state === "running";
  if (running && s.busy) return { label: "working", type: "success", busy: true };
  if (s.waitingOnUser) {
    const n = s.waitingOnUser;
    const waiting = `${n} ${n === 1 ? "request" : "requests"} waiting for your answer`;
    return {
      label: "waiting for you",
      type: "warning",
      busy: false,
      detail: running ? waiting : `Sleeping, ${waiting}`,
    };
  }
  if (s.pending) return { label: "queued", type: "info", busy: true, detail: `${s.pending} message(s) queued` };
  if (running) return { label: "running", type: "success", busy: false };
  return { label: "sleeping", type: "default", busy: false };
}

/** "in 2m" (or "now") until an RFC 3339 time. */
export function retryIn(at: string, now: number = Date.now()): string {
  const secs = Math.round((Date.parse(at) - now) / 1000);
  if (!(secs > 0)) return "now";
  if (secs < 60) return `in ${secs}s`;
  if (secs < 3600) return `in ${Math.round(secs / 60)}m`;
  return `in ${Math.round(secs / 3600)}h`;
}
