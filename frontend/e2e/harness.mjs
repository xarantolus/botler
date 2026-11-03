// A throwaway hub for end-to-end runs: a scratch data directory, the models pointed at a scripted
// OpenAI-compatible stub (`decide(messages, tools)` → `{ text }` or `{ tool, args }`), `botler
// serve` on 127.0.0.1:BOTLER_E2E_PORT (default 7437, never 7420; agents reach it over its Unix
// socket), and cleanup: stopping the hub removes its agent containers; then its services, the
// images labelled with the data directory and the directory itself go (BOTLER_E2E_KEEP=1 keeps
// them).
//
// Env: BOTLER_BIN (default ../target/debug/botler), BOTLER_E2E_DIR (where the data directory goes;
// default: the system temp directory).

import { execFileSync, spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

export const step = (what) => console.log(`\n== ${what}`);

/** The text of a chat message's content (a string or parts). */
export const textOf = (m) => (typeof m?.content === "string" ? m.content : (m?.content ?? []).map((p) => p.text ?? "").join(""));

function chunks(reply, model) {
  const base = { id: "stub", object: "chat.completion.chunk", created: Math.floor(Date.now() / 1000), model };
  // Text, a tool call, or both (words before a call that ends the turn).
  const delta = { role: "assistant", ...(reply.text ? { content: reply.text } : {}) };
  if (reply.tool) delta.tool_calls = [{ index: 0, id: `call_${Date.now()}`, type: "function", function: { name: reply.tool, arguments: JSON.stringify(reply.args) } }];
  return [
    { ...base, choices: [{ index: 0, delta, finish_reason: null }] },
    { ...base, choices: [{ index: 0, delta: {}, finish_reason: reply.tool ? "tool_calls" : "stop" }], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } },
  ];
}

function stubServer(decide) {
  return createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      if (!req.url?.endsWith("/chat/completions")) {
        res.writeHead(404).end();
        return;
      }
      const r = JSON.parse(body);
      const tools = new Set((r.tools ?? []).map((t) => t.function?.name));
      let reply = decide(r.messages ?? [], tools);
      // Tool names as pi registered them: a direct MCP tool is `mcp__botler__<op>` (the reviewer, for
      // one, has no ask_user).
      if (reply.tool && !tools.has(reply.tool)) {
        const direct = [...tools].find((t) => t?.endsWith(`__${reply.tool}`));
        reply = direct ? { ...reply, tool: direct } : { text: "Done." };
      }
      console.log(`   model: ${reply.tool ? `${reply.tool}(${JSON.stringify(reply.args).slice(0, 60)})` : JSON.stringify(reply.text?.slice(0, 60))}`);
      if (!r.stream) {
        const c = chunks(reply, r.model)[0].choices[0].delta;
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ id: "stub", object: "chat.completion", model: r.model, choices: [{ index: 0, message: c, finish_reason: reply.tool ? "tool_calls" : "stop" }], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } }));
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      for (const c of chunks(reply, r.model)) res.write(`data: ${JSON.stringify(c)}\n\n`);
      res.end("data: [DONE]\n\n");
    });
  });
}

/** Start the stub and a hub on a fresh data directory. */
export async function startHub(decide) {
  const bin = resolve(process.env.BOTLER_BIN ?? join(here, "../../target/debug/botler"));
  const port = Number(process.env.BOTLER_E2E_PORT ?? 7437);
  if (port === 7420) throw new Error("7420 is the real hub's port; pick another BOTLER_E2E_PORT");
  const data = mkdtempSync(join(process.env.BOTLER_E2E_DIR ?? tmpdir(), "botler-e2e-"));
  const botler = (...args) => execFileSync(bin, ["--data", data, ...args], { encoding: "utf8", stdio: ["pipe", "pipe", "inherit"] });

  const stub = stubServer(decide);
  // Agents reach the stub through host.containers.internal (the host's address, not loopback).
  await new Promise((ok) => stub.listen(0, "0.0.0.0", ok));
  const stubPort = stub.address().port;

  step(`hub in ${data} on :${port}, model stub on :${stubPort}`);
  botler("init");
  const work = join(data, "e2e-clone");
  const git = (...args) => execFileSync("git", ["-C", work, ...args], { stdio: "pipe" });
  execFileSync("git", ["clone", "-q", join(data, "repo.git"), work]);
  const toml = readFileSync(join(work, "botler.toml"), "utf8").replace(
    /^\[models\]\n/m,
    `[models]\nbase_url = "http://host.containers.internal:${stubPort}/v1"\n`,
  );
  writeFileSync(join(work, "botler.toml"), toml);
  git("-c", "user.name=e2e", "-c", "user.email=e2e@localhost", "commit", "-qam", "e2e: models from the stub");
  git("push", "-q", "origin", "HEAD:main");
  execFileSync(bin, ["--data", data, "secret", "set", "OPENROUTER_API_KEY"], { input: "stub-key" });
  const token = botler("token").trim();
  const hub = spawn(bin, ["--data", data, "serve", "--listen", `127.0.0.1:${port}`], {
    env: { ...process.env, RUST_LOG: "info,rmcp=warn" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let log = "";
  hub.stderr.on("data", (d) => (log = (log + d).slice(-20_000)));
  hub.stdout.on("data", (d) => (log = (log + d).slice(-20_000)));
  const url = `http://127.0.0.1:${port}`;
  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`${url}/healthz`)).ok) break;
    } catch {
      // not up yet
    }
    if (i > 240) throw new Error(`the hub did not start:\n${log}`);
    await new Promise((r) => setTimeout(r, 500));
  }

  async function stop() {
    stub.close();
    // SIGINT: the hub stops its agents (removing their containers) before exiting.
    hub.kill("SIGINT");
    await new Promise((r) => hub.once("exit", r));
    if (process.env.BOTLER_E2E_KEEP) {
      console.log(`kept ${data}`);
      return;
    }
    // Services outlive the hub on purpose (they survive its restarts): remove this hub's.
    const containers = execFileSync("podman", ["ps", "-aq", "--filter", `label=botler.data=${data}`], { encoding: "utf8" })
      .split("\n")
      .filter(Boolean);
    if (containers.length) execFileSync("podman", ["rm", "-f", ...containers], { stdio: ["ignore", "ignore", "inherit"] });
    const images = () =>
      execFileSync("podman", ["images", "--filter", `label=botler.data=${data}`, "--format", '{{if eq .Repository "<none>"}}{{.ID}}{{else}}{{.Repository}}:{{.Tag}}{{end}}'], { encoding: "utf8" })
        .split("\n")
        .filter(Boolean);
    // An image build the hub started may still be finishing (its build container holds the base), so
    // what is left is listed again and removed until nothing is.
    for (let attempt = 1, left = images(); left.length; attempt++, left = images()) {
      try {
        execFileSync("podman", ["rmi", ...left], { stdio: "inherit" });
      } catch (e) {
        if (attempt === 5) throw e;
        await new Promise((r) => setTimeout(r, 15_000));
      }
    }
    // The hub's network, `botler-<hub id>` (`Paths::hub_id`: the data directory's hash).
    const network = `botler-${createHash("sha256").update(data).digest("hex").slice(0, 8)}`;
    const exists = spawnSync("podman", ["network", "exists", network]).status;
    if (exists === 0) execFileSync("podman", ["network", "rm", network], { stdio: ["ignore", "ignore", "inherit"] });
    else if (exists !== 1) throw new Error(`podman network exists ${network} failed (${exists})`);
    rmSync(data, { recursive: true, force: true });
  }

  return { url, token, botler, log: () => log, stop };
}
