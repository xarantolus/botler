// Delivery envelopes: the botler pi extension turns every delivered event into a user turn whose
// first line is a header like
//   [Fri 2026-10-30 17:05 Europe/Berlin (first message in 3 days) | from agent:x | provenance: trusted | message | delivery D | event E]
// (the local time and gap note are absent in older headers).
// The hub parses the strict form already (sessions.rs); this parser is tolerant of older and
// partial headers too (e.g. without kind or event), and of the malformed-delivery marker.

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
  malformed: boolean;
}

const MALFORMED = "[botler: malformed delivery]";

/** Parse an envelope header off `text`, or return null for plain text. */
export function parseEnvelope(text: string): Envelope | null {
  const nl = text.indexOf("\n");
  const first = (nl < 0 ? text : text.slice(0, nl)).trim();
  const body = nl < 0 ? "" : text.slice(nl + 1);
  if (first === MALFORMED) {
    return {
      from: "?",
      provenance: "untrusted",
      kind: null,
      delivery: null,
      event: null,
      localTime: null,
      gap: null,
      body,
      malformed: true,
    };
  }
  if (!first.startsWith("[") || !first.endsWith("]")) return null;
  const parts = first
    .slice(1, -1)
    .split(" | ")
    .map((p) => p.trim());
  const env: Envelope = {
    from: "",
    provenance: "",
    kind: null,
    delivery: null,
    event: null,
    localTime: null,
    gap: null,
    body,
    malformed: false,
  };
  // An optional leading local time, with an optional gap note in parentheses.
  if (parts[0] && !parts[0].startsWith("from ")) {
    const when = parts.shift()!;
    const m = /^(.*) \((.*)\)$/.exec(when);
    env.localTime = m ? m[1]! : when;
    env.gap = m ? m[2]! : null;
  }
  if (!parts[0]?.startsWith("from ")) return null;
  for (const p of parts) {
    if (p.startsWith("from ")) env.from = p.slice(5);
    else if (p.startsWith("provenance: ")) env.provenance = p.slice(12);
    else if (p.startsWith("delivery ")) env.delivery = p.slice(9);
    else if (p.startsWith("event ")) env.event = p.slice(6);
    else if (env.kind === null && /^[\w.-]+$/.test(p)) env.kind = p;
    else return null;
  }
  if (!env.from || !env.provenance) return null;
  return env;
}

/** Whether a delivery is the user talking (rendered as a chat bubble, not an event card). */
export function isUserMessage(env: Pick<Envelope, "from" | "kind">): boolean {
  return env.from === "user" && (env.kind === null || env.kind === "message");
}

/** The envelope of a session user item: the hub's parse when present, else ours. */
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
  if (item.from !== null && item.provenance !== null) {
    return {
      from: item.from,
      provenance: item.provenance,
      kind: item.event_kind,
      delivery: item.delivery,
      event: item.event,
      localTime: item.local_time ?? null,
      gap: item.gap ?? null,
      body: item.text,
      malformed: false,
    };
  }
  return parseEnvelope(item.text);
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
