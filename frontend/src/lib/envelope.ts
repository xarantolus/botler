// Delivery envelopes: the botler pi extension turns every delivered event into a user turn whose
// first line is a header like
//   [from agent:main | provenance: trusted | message | delivery D | event E]
// The hub parses the strict form already (sessions.rs); this parser is tolerant of older and
// partial headers too (e.g. without kind or event), and of the malformed-delivery marker.

export interface Envelope {
  from: string;
  provenance: string;
  kind: string | null;
  delivery: string | null;
  event: string | null;
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
    return { from: "?", provenance: "untrusted", kind: null, delivery: null, event: null, body, malformed: true };
  }
  if (!first.startsWith("[from ") || !first.endsWith("]")) return null;
  const parts = first.slice(1, -1).split(" | ").map((p) => p.trim());
  const env: Envelope = { from: "", provenance: "", kind: null, delivery: null, event: null, body, malformed: false };
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
}): Envelope | null {
  if (item.from !== null && item.provenance !== null) {
    return {
      from: item.from,
      provenance: item.provenance,
      kind: item.event_kind,
      delivery: item.delivery,
      event: item.event,
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
