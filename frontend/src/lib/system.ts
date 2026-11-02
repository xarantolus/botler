// The System view's logic: when things fire next, and colors for states.

import type { JobView, RunStatus, ServiceState, SystemView, TriggerPreview } from "../generated";
import type { Color } from "./runs";

/** "in 40 s", "in 12 min", "in 9 h", "in 3 d" (or "now"). */
export function untilLabel(at: string, now = Date.now()): string {
  const s = Math.round((new Date(at).getTime() - now) / 1000);
  if (s < 1) return "now";
  if (s < 60) return `in ${s} s`;
  if (s < 3600) return `in ${Math.round(s / 60)} min`;
  if (s < 2 * 86400) return `in ${Math.round(s / 3600)} h`;
  return `in ${Math.round(s / 86400)} d`;
}

/** A fire time in the instance's time zone: "Sat 06:23". */
export function localLabel(at: string, timeZone: string): string {
  return new Intl.DateTimeFormat("en-GB", { weekday: "short", hour: "2-digit", minute: "2-digit", hour12: false, timeZone })
    .format(new Date(at))
    .replace(",", "");
}

/** "in 9 h · Sat 06:23". */
export function fireLabel(at: string, timeZone: string, now = Date.now()): string {
  return `${untilLabel(at, now)} · ${localLabel(at, timeZone)}`;
}

/** What a schedule runs: the job or agent it belongs to, or the trigger's own file. */
export interface Schedule {
  trigger: TriggerPreview;
  /** The job (for the slideover), if it runs one. */
  job: string | null;
  next: string;
}

/** Every trigger with an upcoming fire, once, soonest first. */
export function schedules(v: SystemView): Schedule[] {
  const seen = new Map<string, Schedule>();
  const add = (trigger: TriggerPreview, job: string | null) => {
    const next = trigger.upcoming[0];
    if (next && !seen.has(trigger.id)) seen.set(trigger.id, { trigger, job, next });
  };
  for (const j of v.jobs) for (const t of j.triggers) add(t, j.name);
  for (const a of v.agents) for (const t of a.triggers) add(t, null);
  for (const t of v.triggers) add(t, null);
  return [...seen.values()].sort((a, b) => a.next.localeCompare(b.next));
}

export function serviceStateColor(s: ServiceState): Color {
  return s === "running" ? "success" : s === "failed" ? "error" : s === "backoff" ? "warning" : s === "starting" || s === "updating" ? "info" : "neutral";
}

/** A trigger line without its Markdown code quotes ("run job `x`" → "run job x"). */
export function plain(text: string): string {
  return text.replace(/`/g, "");
}

/** The soonest upcoming fire of any of `triggers`. */
export function nextFire(triggers: Pick<TriggerPreview, "upcoming">[]): string | null {
  return triggers.map((t) => t.upcoming[0]).filter((t): t is string => !!t).sort()[0] ?? null;
}

/** Whether the last run of a job failed. */
export function lastFailed(j: Pick<JobView, "recent_runs">): boolean {
  const s: RunStatus | undefined = j.recent_runs[0]?.status;
  return s === "failed" || s === "timeout";
}
