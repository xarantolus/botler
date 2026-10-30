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

export function stepLabel(step: WakeStep): string {
  return WAKE_STEPS.find((s) => s.step === step)?.label ?? step;
}

/** The newest view of a wake: a live status overrides the phase from the last fetch. */
export function currentStatus(phase: WakePhase | null, live: AgentStatus | null): AgentStatus | null {
  if (live) return live;
  if (!phase) return null;
  return phase.phase === "waking" ? { status: "waking", step: phase.step } : { status: "failed", error: phase.error };
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
  if (st?.status === "waking") return { label: "starting", type: "info", busy: true, detail: stepLabel(st.step) };
  if (s.state === "starting") return { label: "starting", type: "info", busy: true };
  if (s.archiveRequested) return { label: "handing over", type: "warning", busy: true };
  const running = st ? st.status === "running" : s.state === "running";
  if (running && s.busy) return { label: "working", type: "success", busy: true };
  if (s.waitingOnUser) {
    return {
      label: "waiting for you",
      type: "warning",
      busy: false,
      detail: running ? "Waiting for your answer" : "Sleeping, waiting for your answer",
    };
  }
  if (s.pending) return { label: "queued", type: "info", busy: true, detail: `${s.pending} message(s) queued` };
  if (running) return { label: "running", type: "success", busy: false };
  return { label: "sleeping", type: "default", busy: false };
}
