// The botler pi extension. Loaded into every agent session by the hub (read-only mount).
//
// The hub delivers events as prompts of the form `botler:delivery {json}`. This extension turns
// them into readable user turns with a provenance header, and drops deliveries it has already
// seen: the hub delivers at least once, so a crash between "pi accepted it" and "the hub recorded
// it" re-sends the same delivery id. Seen ids are stored in the session itself (custom entries),
// so dedup survives restarts and follows the session's branch.
//
// A `quiet` delivery (e.g. "thanks, no reply needed" from another agent) is a note: it is added
// to the session as a custom message without starting a turn (appended after the current turn
// if one is running), so the agent reads it with whatever wakes it next.
//
// Hub tools come from the hub's MCP server (see mcp.json). Python tools from the monorepo's
// `tools/` are registered here: the hub renders the ones this agent is granted into
// /botler/agent-dir/tools.json (entries of tools.manifest.json) and mounts the applied main commit
// at /botler/applied; each call runs `python -m botler.tools call <name>` there.
//
// Hub ops after which the turn is over (ask_user, propose_change, ...) are registered here too,
// from /botler/agent-dir/turn-tools.json (rendered by the hub from its op specs): MCP results
// cannot end a turn, these return `terminate: true` so no model call follows. The hub hides
// their MCP duplicates.

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const TOOLS_FILE = "/botler/agent-dir/tools.json";
const TURN_TOOLS_FILE = "/botler/agent-dir/turn-tools.json";
const APPLIED = "/botler/applied";
const TOOL_TIMEOUT_MS = 300_000; // the first call may install the tools' dependencies
const MAX_OUTPUT = 50_000;

interface ToolDecl {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  effects: string[];
}

/** Model APIs accept `[A-Za-z0-9_-]{1,64}`; manifest names are dotted (`weather.forecast`). */
export function piToolName(name: string): string {
  return name.replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 64);
}

/** A hub op the extension exposes as a turn-ending tool. */
interface OpDecl {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
}

function loadJson<T>(path: string): T[] {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return []; // nothing granted
    throw e;
  }
  return JSON.parse(text) as T[];
}

/** Call hub op `name` (`POST /api/v1/ops/{name}`, as the Python SDK does); throws on failure. */
export async function callOp(name: string, args: unknown, signal?: AbortSignal): Promise<unknown> {
  const url = process.env.BOTLER_URL;
  const token = process.env.BOTLER_TOKEN;
  if (!url || !token) throw new Error(`cannot call ${name}: BOTLER_URL or BOTLER_TOKEN is not set`);
  const resp = await fetch(`${url.replace(/\/$/, "")}/api/v1/ops/${name}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify(args ?? {}),
    signal,
  });
  const raw = await resp.text();
  if (!resp.ok) {
    let message = raw || resp.statusText;
    try {
      message = String((JSON.parse(raw) as { error?: unknown }).error ?? message);
    } catch {
      // not JSON: keep the raw body
    }
    throw new Error(`${name} failed (${resp.status}): ${message}`);
  }
  return raw ? JSON.parse(raw) : null;
}

/** Run one tool call in a subprocess: JSON args on stdin, `{ok, result | error}` on stdout. */
export function runTool(name: string, args: unknown, signal?: AbortSignal): Promise<unknown> {
  const argv = ["run", "--frozen", "--no-dev", "--all-packages", "--project", APPLIED, "python", "-m", "botler.tools", "call", name];
  return new Promise((resolve, reject) => {
    const child = spawn("uv", argv, {
      cwd: APPLIED,
      env: { ...process.env, UV_PROJECT_ENVIRONMENT: "/tmp/botler-tools-venv", PYTHONDONTWRITEBYTECODE: "1" },
      timeout: TOOL_TIMEOUT_MS,
      signal,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d: Buffer) => (stdout += d.toString()));
    child.stderr.on("data", (d: Buffer) => (stderr += d.toString()));
    child.on("error", reject);
    child.on("close", (code, sig) => {
      const tail = stderr.slice(-2000);
      const last = stdout.trim().split("\n").pop() ?? "";
      let reply: { ok?: boolean; result?: unknown; error?: { type?: string; message?: string } };
      try {
        reply = JSON.parse(last);
      } catch {
        const why = sig ? `killed by ${sig} (timeout ${TOOL_TIMEOUT_MS / 1000}s?)` : `exit code ${code}`;
        reject(new Error(`tool ${name} failed (${why}) without a result.\n${tail}`));
        return;
      }
      if (reply.ok) resolve(reply.result);
      else reject(new Error(`${reply.error?.type ?? "Error"}: ${reply.error?.message ?? "unknown"}\n${tail}`));
    });
    child.stdin.end(JSON.stringify(args ?? {}));
  });
}

function toText(result: unknown): string {
  const text = typeof result === "string" ? result : JSON.stringify(result, null, 2);
  return text.length > MAX_OUTPUT ? `${text.slice(0, MAX_OUTPUT)}\n[truncated ${text.length - MAX_OUTPUT} characters]` : text;
}

const PREFIX = "botler:delivery ";
const SEEN = "botler.delivered";
const NOTE = "botler.note";

// Mirrors botler_core::event::payload::DeliveryEnvelope.
interface Envelope {
  id: string;
  event: string;
  kind: string;
  from: string;
  provenance: "trusted" | "external" | "untrusted";
  correlation: string;
  body: string;
  quiet?: boolean;
  at?: string;
  gap?: string;
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
  const when = env.at ? `${env.at}${env.gap ? ` (${env.gap})` : ""} | ` : "";
  return `[${when}from ${env.from} | provenance: ${env.provenance} | ${env.kind} | delivery ${env.id} | event ${env.event}]`;
}

export default function botler(pi: ExtensionAPI) {
  let seen = new Set<string>();
  const registered = new Set<string>();

  pi.on("session_start", async (_event, ctx) => {
    for (const op of loadJson<OpDecl>(TURN_TOOLS_FILE)) {
      if (registered.has(op.name)) continue;
      registered.add(op.name);
      pi.registerTool({
        name: op.name,
        label: op.name,
        description: `${op.description}\n\nOn success this ends your turn: say anything else in the same reply, before the call.`,
        parameters: op.parameters as never,
        // Ending the turn only works when the model calls it, not from a codemode script.
        exposure: "model-only",
        async execute(_id, params, signal) {
          const result = await callOp(op.name, params, signal);
          return { content: [{ type: "text", text: toText(result) }], details: undefined, terminate: true };
        },
      });
    }
    for (const t of loadJson<ToolDecl>(TOOLS_FILE)) {
      const name = piToolName(t.name);
      if (registered.has(name)) continue;
      registered.add(name);
      const effects = t.effects.length ? `\n\nEffects: ${t.effects.join(", ")}.` : "";
      pi.registerTool({
        name,
        label: t.name,
        description: `${t.description}${effects}`,
        // Plain JSON Schema, as pi's own MCP tools pass it.
        parameters: t.parameters as never,
        async execute(_id, params, signal) {
          const result = await runTool(t.name, params, signal);
          return { content: [{ type: "text", text: toText(result) }], details: undefined };
        },
      });
    }
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
    if (env.quiet) {
      pi.sendMessage(
        { customType: NOTE, content: `${header(env)}\n(note: delivered without waking you; no reply needed)\n${env.body}`, display: true },
        { triggerTurn: false },
      );
      return { action: "handled" };
    }
    return { action: "transform", text: `${header(env)}\n${env.body}`, images: event.images };
  });
}
