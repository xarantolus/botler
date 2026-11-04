// Delivery envelopes: the botler pi extension turns every delivered event into a user turn whose
// first line is a header; the hub parses it into the session item's fields (sessions.rs).

export interface Envelope {
  from: string;
  provenance: string;
  kind: string | null;
  delivery: string | null;
  event: string | null;
  /** When it was delivered, in the instance's local time. */
  localTime: string | null;
  /** After a long silence, e.g. `first message in 3 days`. */
  gap: string | null;
  body: string;
}

/** Whether a delivery is the user talking (rendered as a chat bubble, not an event card). */
export function isUserMessage(env: Pick<Envelope, "from" | "kind">): boolean {
  return env.from === "user" && (env.kind === null || env.kind === "message");
}

/** The envelope of a session user item, or null for one without a delivery header. */
export function envelopeOf(item: {
  text: string;
  from: string | null;
  provenance: string | null;
  event_kind: string | null;
  delivery: string | null;
  event: string | null;
  local_time?: string | null;
  gap?: string | null;
}): Envelope | null {
  if (item.from === null || item.provenance === null) return null;
  return {
    from: item.from,
    provenance: item.provenance,
    kind: item.event_kind,
    delivery: item.delivery,
    event: item.event,
    localTime: item.local_time ?? null,
    gap: item.gap ?? null,
    body: item.text,
  };
}

/** If the body is a JSON document (typical for structured events), pretty-print it. */
export function prettyJsonBody(body: string): string | null {
  const t = body.trim();
  if (!(t.startsWith("{") || t.startsWith("["))) return null;
  try {
    return JSON.stringify(JSON.parse(t), null, 2);
  } catch {
    // Not JSON after all: render as markdown.
    return null;
  }
}

/** The JSON payload a hub event's body carries (its first ```json block), if any. */
export function payloadOf(body: string): Record<string, unknown> | null {
  const m = /```json\n([\s\S]*?)\n```/.exec(body);
  if (!m) return null;
  try {
    const v: unknown = JSON.parse(m[1]!);
    return v && typeof v === "object" && !Array.isArray(v)
      ? (v as Record<string, unknown>)
      : null;
  } catch {
    // Not JSON: the body renders as Markdown.
    return null;
  }
}

/** The payload fields that say what a delivered event is about, in order of preference. */
const PREVIEW_FIELDS = [
  "text",
  "last_reply",
  "question",
  "detail",
  "summary",
  "reason",
  "error",
  "status",
  "changes",
];

/** One line saying what a delivery is about, shown while its card is folded. */
export function previewOf(body: string): string {
  const p = payloadOf(body) ?? payloadOf("```json\n" + body.trim() + "\n```");
  const field =
    p &&
    PREVIEW_FIELDS.map((f) => p[f]).find(
      (v) => (typeof v === "string" && v.trim()) || Array.isArray(v),
    );
  const text = Array.isArray(field)
    ? field.join("; ")
    : typeof field === "string"
      ? field
      : body.replace(/```json[\s\S]*?```/g, "");
  const line = text
    .split("\n")
    .find((l) => l.trim() && !/^[{}[\]]+,?$/.test(l.trim()));
  return (line ?? "").trim().replace(/^#+\s*/, "");
}
