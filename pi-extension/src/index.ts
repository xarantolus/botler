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
// /botler/agent-dir/tools.json (entries of tools.manifest.json). The agent image has them, with
// their dependencies, installed in /opt/botler/tools-venv; each call runs
// `python -m botler.tools call <name>` with that venv's Python and installs nothing.
//
// MCP servers of granted services (/botler/agent-dir/mcp-services.json) are registered here, not
// in pi's mcp.json: pi gives up on a configured server it cannot reach as the session starts (the
// service restarting just then) and never tries it again. A registered server that offers no tools
// is registered again at the next turn (at most every RETRY_MCP_MS), which makes pi connect anew.
//
// Hub ops after which the turn is over (ask_user, propose_change, ...) are registered here too,
// from /botler/agent-dir/turn-tools.json (rendered by the hub from its op specs): MCP results
// cannot end a turn, these return `terminate: true` so no model call follows. The hub hides
// their MCP duplicates.

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";

import { compact, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

import type { DeliveryEnvelope as Envelope, McpService, ToolDecl, TurnTool } from "./generated/index.js";

// pi's update prompt keeps everything the previous summary held, so a long-running chat's summary
// only grows (the apartment curator's reached 100k characters, about as large as the threshold
// itself, and it compacted every few calls). Compactions here run pi's own summarizer with a
// budget that makes it drop what is done and stale.
const SUMMARY_BUDGET =
  "Keep the whole summary under 1,500 words. Drop finished work that no longer matters, superseded " +
  "decisions and details the agent can look up again (files, memory, tools); keep open tasks, the " +
  "user's preferences and anything needed to continue.";

const TOOLS_FILE = "/botler/agent-dir/tools.json";
const TURN_TOOLS_FILE = "/botler/agent-dir/turn-tools.json";
const MCP_SERVICES_FILE = "/botler/agent-dir/mcp-services.json";
const RETRY_MCP_MS = 15_000;
const TOOLS_PYTHON = "/opt/botler/tools-venv/bin/python";
const TOOL_TIMEOUT_MS = 300_000;
const MAX_OUTPUT = 50_000;

/** Model APIs accept `[A-Za-z0-9_-]{1,64}`; manifest names are dotted (`weather.forecast`). */
export function piToolName(name: string): string {
  return name.replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 64);
}

function loadJson<T>(path: string, empty: T): T {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return empty; // nothing granted
    throw e;
  }
  return JSON.parse(text) as T;
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
  // -P: the working directory is not on sys.path, so nothing there shadows the tools' modules.
  const argv = ["-P", "-m", "botler.tools", "call", name, "--manifest", TOOLS_FILE];
  return new Promise((resolve, reject) => {
    const child = spawn(TOOLS_PYTHON, argv, {
      cwd: "/",
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" },
      timeout: TOOL_TIMEOUT_MS,
      signal,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d: Buffer) => (stdout += d.toString()));
    child.stderr.on("data", (d: Buffer) => (stderr += d.toString()));
    child.on("error", (e: NodeJS.ErrnoException) =>
      reject(
        e.code === "ENOENT"
          ? new Error(`tool ${name}: this agent image has no ${TOOLS_PYTHON}; its Dockerfile must install the tools (see the template's images/agent/Dockerfile)`)
          : e,
      ),
    );
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
/** Written after navigating the session tree: the leaf is the last entry, so this pins it. */
const NAV = "botler.nav";

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
  /** Deliveries passed to pi whose message is not in the session yet. */
  const unsaved = new Map<string, Envelope>();
  const registered = new Set<string>();

  const services = Object.entries(loadJson<Record<string, McpService>>(MCP_SERVICES_FILE, {}));
  const attempts = new Map<string, { at: number; n: number }>();
  const connect = (name: string, url: string) => {
    const n = (attempts.get(name)?.n ?? 0) + 1;
    attempts.set(name, { at: Date.now(), n });
    // pi connects a server again only when its config changed, so each attempt is numbered.
    pi.registerMcpServer(name, n === 1 ? { url } : { url, headers: { "X-Botler-Attempt": String(n) } });
  };
  for (const [name, { url }] of services) connect(name, url);
  pi.on("turn_start", async () => {
    const tools = pi.getAllTools();
    for (const [name, { url }] of services) {
      const namespace = `mcp__${name.replace(/-/g, "_")}`;
      const offered = tools.some((t) => t.namespace?.name === namespace && t.exposure !== "hidden");
      if (!offered && Date.now() - (attempts.get(name)?.at ?? 0) >= RETRY_MCP_MS) connect(name, url);
    }
  });

  // Session tree commands, sent by the hub only while pi is idle (navigating throws mid-turn).
  pi.registerCommand("botler-goto", {
    description: "botler: continue this conversation from an earlier entry",
    handler: async (args, ctx) => {
      const target = args.trim();
      const { cancelled } = await ctx.navigateTree(target);
      if (!cancelled) pi.appendEntry(NAV, { target });
    },
  });
  pi.registerCommand("botler-label", {
    description: "botler: label an entry of this conversation (no label: remove it)",
    handler: async (args) => {
      const [target = "", ...words] = args.trim().split(/\s+/);
      pi.setLabel(target, words.join(" ") || undefined);
    },
  });

  pi.on("session_start", async (_event, ctx) => {
    for (const op of loadJson<TurnTool[]>(TURN_TOOLS_FILE, [])) {
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
    for (const t of loadJson<ToolDecl[]>(TOOLS_FILE, [])) {
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
    if (env.quiet) {
      pi.appendEntry(SEEN, { id: env.id, event: env.event, from: env.from });
      pi.sendMessage(
        { customType: NOTE, content: `${header(env)}\n(note: delivered without waking you; no reply needed)\n${env.body}`, display: true },
        { triggerTurn: false },
      );
      return { action: "handled" };
    }
    // Recorded once the message is in the session: pi may compact first, and a container stopped
    // meanwhile must get the delivery again instead of dropping it as one it has seen.
    unsaved.set(env.id, env);
    return { action: "transform", text: `${header(env)}\n${env.body}`, images: event.images };
  });

  pi.on("message_end", async (event) => {
    if (event.message.role !== "user" || unsaved.size === 0) return;
    const content = event.message.content;
    const text = typeof content === "string" ? content : content.map((c) => (c.type === "text" ? c.text : "")).join("");
    for (const [id, env] of unsaved) {
      if (text.includes(`| delivery ${id} |`)) {
        unsaved.delete(id);
        pi.appendEntry(SEEN, { id, event: env.event, from: env.from });
      }
    }
  });

  pi.on("session_before_compact", async (event, ctx) => {
    if (!ctx.model) return;
    const auth = await ctx.modelRegistry.getApiKeyAndHeaders(ctx.model);
    if (!auth.ok) throw new Error(`no credentials for ${ctx.model.id}`);
    const focus = event.customInstructions ? `${event.customInstructions}\n\n${SUMMARY_BUDGET}` : SUMMARY_BUDGET;
    const compaction = await compact(
      event.preparation,
      ctx.model,
      auth.apiKey,
      auth.headers && Object.fromEntries(Object.entries(auth.headers).filter((h): h is [string, string] => h[1] != null)),
      focus,
      event.signal,
      pi.getThinkingLevel(),
      undefined,
      auth.env,
    );
    return { compaction };
  });
}
