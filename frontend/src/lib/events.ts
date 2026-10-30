import type { BotlerEvent, BuiltinPayload, Dialog, Instance, Source } from "../generated";

// Every built-in type; `satisfies` makes the compiler check this against the generated union.
const BUILTIN = {
  message: true,
  "runnable.failed": true,
  "runnable.finished": true,
  "dialog.answered": true,
  "dialog.aborted": true,
  "proposal.updated": true,
  "action.result": true,
  "breaker.tripped": true,
  "archive.requested": true,
  "agent.idle": true,
  timer: true,
} as const satisfies Record<BuiltinPayload["type"], true>;

/** The typed payload of a built-in event, or null for agent-defined event types. */
export function asBuiltin(ev: Pick<BotlerEvent, "type" | "payload">): BuiltinPayload | null {
  return ev.type in BUILTIN ? ({ type: ev.type, payload: ev.payload } as BuiltinPayload) : null;
}

export function sourceLabel(s: Source): string {
  return "name" in s ? `${s.kind}:${s.name}` : s.kind;
}

/** One line about what an event says. */
export function eventSummary(ev: BotlerEvent): string {
  const b = asBuiltin(ev);
  if (!b) {
    const p = ev.payload;
    if (p && typeof p === "object" && !Array.isArray(p)) {
      const text = (p as Record<string, unknown>).text;
      if (typeof text === "string") return text;
    }
    return JSON.stringify(p ?? null).slice(0, 160);
  }
  switch (b.type) {
    case "message":
      return b.payload.text;
    case "runnable.failed":
      return `${b.payload.runnable} failed (${b.payload.kind}${b.payload.exit_code !== null ? `, exit ${b.payload.exit_code}` : ""}) ×${b.payload.count}`;
    case "runnable.finished":
      return `${b.payload.runnable} finished${b.payload.output ? `: ${b.payload.output}` : ""}`;
    case "dialog.answered":
      return `"${b.payload.question}" → ${b.payload.answer}`;
    case "dialog.aborted":
      return `"${b.payload.question}" withdrawn: ${b.payload.reason}`;
    case "proposal.updated":
      return `${b.payload.title}: ${b.payload.status}`;
    case "action.result":
      return `${b.payload.action}: ${b.payload.ok ? "ok" : "failed"}`;
    case "breaker.tripped":
      return `${b.payload.breaker} breaker on ${b.payload.subject}: ${b.payload.reason}`;
    case "archive.requested":
      return `archive requested${b.payload.reason ? `: ${b.payload.reason}` : ""}`;
    case "agent.idle":
      return `${b.payload.title ?? b.payload.instance} is idle`;
    case "timer":
      return `timer ${b.payload.timer_id}${b.payload.note ? `: ${b.payload.note}` : ""}`;
  }
}

/** The pinned front door chat: `botler` once renamed, `main` before. */
export function frontDoor(instances: Pick<Instance, "name">[]): string | null {
  for (const n of ["botler", "main"]) if (instances.some((i) => i.name === n)) return n;
  return null;
}

export function chatTitle(i: Pick<Instance, "name" | "title" | "slug">): string {
  return i.title || i.slug || i.name;
}

/** The dialog a tool result opened, directly or through its proposal's approval. */
export function linkedDialog(
  link: { dialog_id: string | null; proposal_id: string | null },
  dialogs: Dialog[],
): Dialog | null {
  if (link.dialog_id) return dialogs.find((d) => d.id === link.dialog_id) ?? null;
  if (link.proposal_id) return dialogs.find((d) => d.proposal_id === link.proposal_id) ?? null;
  return null;
}
