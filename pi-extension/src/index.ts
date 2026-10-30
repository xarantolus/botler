// The botler pi extension. Loaded into every agent session by the hub (read-only mount).
//
// The hub delivers events as prompts of the form `botler:delivery {json}`. This extension turns
// them into readable user turns with a provenance header, and drops deliveries it has already
// seen: the hub delivers at least once, so a crash between "pi accepted it" and "the hub recorded
// it" re-sends the same delivery id. Seen ids are stored in the session itself (custom entries),
// so dedup survives restarts and follows the session's branch.
//
// Tools are not defined here: they come from the hub's MCP server (see mcp.json).

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const PREFIX = "botler:delivery ";
const SEEN = "botler.delivered";

interface Envelope {
  id: string;
  event: string;
  kind: string;
  from: string;
  provenance: "trusted" | "external" | "untrusted";
  correlation: string;
  body: string;
}

function parse(text: string): Envelope | undefined {
  if (!text.startsWith(PREFIX)) return undefined;
  try {
    const env = JSON.parse(text.slice(PREFIX.length)) as Envelope;
    return typeof env.id === "string" && typeof env.body === "string" ? env : undefined;
  } catch {
    return undefined;
  }
}

export function header(env: Envelope): string {
  return `[from ${env.from} | provenance: ${env.provenance} | ${env.kind} | delivery ${env.id} | event ${env.event}]`;
}

export default function botler(pi: ExtensionAPI) {
  let seen = new Set<string>();

  pi.on("session_start", async (_event, ctx) => {
    seen = new Set();
    for (const entry of ctx.sessionManager.getBranch()) {
      if (entry.type === "custom" && entry.customType === SEEN) {
        const id = (entry.data as { id?: unknown } | undefined)?.id;
        if (typeof id === "string") seen.add(id);
      }
    }
  });

  pi.on("input", async (event) => {
    const env = parse(event.text);
    if (!env) {
      if (event.text.startsWith(PREFIX)) {
        // Malformed envelope: show it rather than lose it.
        return { action: "transform", text: `[botler: malformed delivery]\n${event.text}` };
      }
      return { action: "continue" };
    }
    if (seen.has(env.id)) return { action: "handled" };
    seen.add(env.id);
    pi.appendEntry(SEEN, { id: env.id, event: env.event, from: env.from });
    return { action: "transform", text: `${header(env)}\n${env.body}`, images: event.images };
  });
}
