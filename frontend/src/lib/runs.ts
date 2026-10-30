// Colors and labels for statuses of runs, proposals, incidents, dialogs and event provenance.

import type { DialogStatus, IncidentStatus, ProposalStatus, PublishedPort, Run, RunStatus, ServiceStatus } from "../generated";

export function serviceColor(s: Pick<ServiceStatus, "running" | "failed">): Color {
  return s.failed ? "error" : s.running ? "success" : "neutral";
}

/** A published service port as a link: through the name this page was reached by (`hostname`),
 * when the service listens on every interface. */
export function portUrl(p: PublishedPort, hostname: string): string {
  return p.bind === "0.0.0.0" || p.bind === "::" ? `http://${hostname}:${p.host_port}` : p.url;
}

export type Color = "neutral" | "primary" | "info" | "success" | "warning" | "error";

export function runStatusColor(s: RunStatus): Color {
  return s === "succeeded" ? "success" : s === "running" || s === "queued" ? "info" : s === "cancelled" ? "neutral" : "error";
}

export function proposalStatusColor(s: ProposalStatus): Color {
  if (s === "merged" || s === "approved") return "success";
  if (s === "rejected" || s === "failed") return "error";
  if (s === "awaiting_user") return "warning";
  return "info";
}

export function proposalStatusLabel(s: ProposalStatus): string {
  return s.replace(/_/g, " ");
}

export function incidentStatusColor(s: IncidentStatus): Color {
  return s === "open" ? "warning" : "success";
}

export function dialogStatusColor(s: DialogStatus): Color {
  return s === "answered" ? "success" : s === "open" ? "warning" : "neutral";
}

/** How far an event's source is trusted. */
export function provenanceColor(p: string): Color {
  return p === "trusted" ? "success" : p === "external" ? "warning" : "error";
}

/** How long a run took (or has been running), e.g. `42s` or `3 min`. */
export function runDuration(r: Pick<Run, "started_at" | "finished_at">, now = Date.now()): string {
  if (!r.started_at) return "";
  const end = r.finished_at ? new Date(r.finished_at).getTime() : now;
  const s = Math.max(0, Math.round((end - new Date(r.started_at).getTime()) / 1000));
  return s < 120 ? `${s}s` : `${Math.round(s / 60)} min`;
}
