// End-to-end smoke test of the web app against a throwaway hub with real agents (Podman) and a
// scripted model. Run locally, not in CI: it builds the agent image (minutes the first time).
//
//   cargo build -p botler && cd frontend && npm run e2e
//
// It creates a scratch data directory, points the models at a stub OpenAI-compatible server
// (this file), starts `botler serve` on 127.0.0.1:BOTLER_E2E_PORT (default 7437, never 7420; its
// agents reach it over its Unix socket), and drives the
// app in headless Chromium: sign in, send a message, attach a file, answer the agent's question inline, ask for a
// change and reject the proposal, and run a job from the System view when there is one. At the
// end it stops the hub, which removes its agent containers, then removes the images labelled with
// the scratch data directory and the directory itself. Set BOTLER_E2E_KEEP=1 to keep them.
//
// Env: BOTLER_BIN (default ../target/debug/botler), BOTLER_E2E_CHROMIUM (a Chromium binary;
// default: Playwright's), BOTLER_E2E_HEADED=1, BOTLER_E2E_SHOTS (directory for screenshots),
// BOTLER_E2E_DIR (where the scratch data directory goes; default: the system temp directory).

import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const here = dirname(fileURLToPath(import.meta.url));
const bin = resolve(process.env.BOTLER_BIN ?? join(here, "../../target/debug/botler"));
const port = Number(process.env.BOTLER_E2E_PORT ?? 7437);
if (port === 7420) throw new Error("7420 is the real hub's port; pick another BOTLER_E2E_PORT");
const shots = process.env.BOTLER_E2E_SHOTS;
if (shots) mkdirSync(shots, { recursive: true });
const data = mkdtempSync(join(process.env.BOTLER_E2E_DIR ?? tmpdir(), "botler-e2e-"));
const botler = (...args) => execFileSync(bin, ["--data", data, ...args], { encoding: "utf8", stdio: ["pipe", "pipe", "inherit"] });
const step = (what) => console.log(`\n== ${what}`);

// ---- the scripted model ----------------------------------------------------------------------
// An OpenAI-compatible chat completions endpoint that answers from the last message: a request to
// "ask" calls ask_user, an answer to it is acknowledged, "propose" commits and proposes a change,
// anything else gets a short reply.

/** The text of a chat message's content (a string or parts). */
const textOf = (m) => (typeof m?.content === "string" ? m.content : (m?.content ?? []).map((p) => p.text ?? "").join(""));

function decide(messages) {
  const last = messages.at(-1);
  if (last?.role === "tool") {
    const call = messages.findLast((m) => m.role === "assistant")?.tool_calls?.[0];
    if (call?.function?.name === "bash" && textOf(last).includes("e2e-commit")) {
      return { tool: "propose_change", args: { branch: "agents/botler/work", title: "Add an e2e note" } };
    }
    return { text: "Done." };
  }
  const said = textOf(last).toLowerCase();
  if (/\bblue\b/.test(said)) return { text: "You chose blue." };
  if (said.includes("please ask me something")) return { tool: "ask_user", args: { question: "Which colour?", options: ["red", "blue"] } };
  if (said.includes("please propose a change")) {
    const command =
      "cd ~/monorepo && echo e2e >> README.md && git add README.md && git -c user.name=e2e -c user.email=e2e@localhost commit -qm 'Add an e2e note' && git push -q origin HEAD:agents/botler/work && echo e2e-commit";
    return { tool: "bash", args: { command } };
  }
  return { text: "Hello from the stub model." };
}

function chunks(reply, model) {
  const base = { id: "stub", object: "chat.completion.chunk", created: Math.floor(Date.now() / 1000), model };
  const delta = reply.tool
    ? { role: "assistant", tool_calls: [{ index: 0, id: `call_${Date.now()}`, type: "function", function: { name: reply.tool, arguments: JSON.stringify(reply.args) } }] }
    : { role: "assistant", content: reply.text };
  return [
    { ...base, choices: [{ index: 0, delta, finish_reason: null }] },
    { ...base, choices: [{ index: 0, delta: {}, finish_reason: reply.tool ? "tool_calls" : "stop" }], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } },
  ];
}

const stub = createServer((req, res) => {
  let body = "";
  req.on("data", (d) => (body += d));
  req.on("end", () => {
    if (!req.url?.endsWith("/chat/completions")) {
      res.writeHead(404).end();
      return;
    }
    const r = JSON.parse(body);
    const tools = new Set((r.tools ?? []).map((t) => t.function?.name));
    let reply = decide(r.messages ?? []);
    // Tool names as pi registered them (the reviewer, for one, has no ask_user).
    if (reply.tool && !tools.has(reply.tool)) reply = { text: "Done." };
    console.log(`   model: ${reply.tool ? `${reply.tool}(${JSON.stringify(reply.args).slice(0, 60)})` : JSON.stringify(reply.text)}`);
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
// Agents reach the stub through host.containers.internal (the host's address, not loopback).
await new Promise((ok) => stub.listen(0, "0.0.0.0", ok));
const stubPort = stub.address().port;

// ---- the hub ---------------------------------------------------------------------------------
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
// The hub listens on localhost; its agents reach it over its Unix socket.
const hub = spawn(bin, ["--data", data, "serve", "--listen", `127.0.0.1:${port}`], {
  env: { ...process.env, RUST_LOG: "info,rmcp=warn" },
  stdio: ["ignore", "ignore", "pipe"],
});
let hubLog = "";
hub.stderr.on("data", (d) => (hubLog = (hubLog + d).slice(-20_000)));
const url = `http://127.0.0.1:${port}`;
for (let i = 0; ; i++) {
  try {
    if ((await fetch(`${url}/healthz`)).ok) break;
  } catch {
    // not up yet
  }
  if (i > 60) throw new Error(`the hub did not start:\n${hubLog}`);
  await new Promise((r) => setTimeout(r, 500));
}

// ---- the app ---------------------------------------------------------------------------------
let browser;
let page;
let n = 0;
const snap = async (name) => shots && (await page.screenshot({ path: join(shots, `${String(++n).padStart(2, "0")}-${name}.png`) }));
const AGENT = 6 * 60_000; // the first wake builds the agent image
let failed = false;

try {
  browser = await chromium.launch({ executablePath: process.env.BOTLER_E2E_CHROMIUM, headless: !process.env.BOTLER_E2E_HEADED });
  page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  page.on("pageerror", (e) => console.log(`   page error: ${e.message}`));
  step("sign in");
  await page.goto(url);
  await page.getByLabel("User token").fill(token);
  await page.getByRole("button", { name: "Open" }).click();
  const composer = page.locator("#chat-composer textarea");
  await composer.waitFor({ timeout: 60_000 });
  await snap("signed-in");

  step("send a message (the front door wakes; the first time builds its image)");
  await composer.fill("Hi there");
  await composer.press("Enter");
  await page.getByText("Hello from the stub model.").waitFor({ timeout: AGENT });
  await snap("replied");

  step("attach a file to a message");
  await page.locator("#chat-composer input[type=file]").setInputFiles({ name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") });
  await page.locator("#chat-composer").getByText("notes.txt").waitFor();
  await composer.fill("Here is a file");
  await composer.press("Enter");
  await page.locator("[data-role=user]").getByRole("button", { name: "notes.txt" }).waitFor({ timeout: 120_000 });
  await snap("attached");

  step("answer the agent's question inline");
  await composer.fill("Please ask me something");
  await composer.press("Enter");
  await page.getByRole("button", { name: "blue", exact: true }).click({ timeout: 120_000 });
  await page.getByText("You chose blue.").waitFor({ timeout: 120_000 });
  await snap("answered");

  step("ask for a change and reject the proposal");
  await composer.fill("Please propose a change");
  await composer.press("Enter");
  await page.getByRole("link", { name: "Proposals" }).click();
  await page.getByText("Add an e2e note").first().click({ timeout: 180_000 });
  await page.getByRole("button", { name: "Reject" }).click();
  await page.getByLabel("Reason").fill("Only a smoke test.");
  await page.getByRole("dialog").getByRole("button", { name: "Reject" }).click();
  await page.getByText("Only a smoke test.").waitFor({ timeout: 30_000 });
  await snap("rejected");

  step("System: run a job now");
  const system = page.getByRole("link", { name: "System" });
  if (await system.count()) {
    await system.first().click();
    await page.getByRole("button", { name: /run now/i }).first().click({ timeout: 30_000 });
    await snap("system-run");
  } else {
    console.log("   skipped: this build has no System view");
  }
  console.log("\nsmoke test passed");
} catch (e) {
  failed = true;
  if (page) await snap("failure").catch((err) => console.error(`   no failure screenshot: ${err}`));
  console.error(`\nsmoke test FAILED: ${e instanceof Error ? e.message : e}\n--- hub log (tail) ---\n${hubLog.slice(-4000)}`);
} finally {
  await browser?.close();
  stub.close();
  // SIGINT: the hub stops its agents (removing their containers) before exiting.
  hub.kill("SIGINT");
  await new Promise((r) => hub.once("exit", r));
  if (process.env.BOTLER_E2E_KEEP) {
    console.log(`kept ${data}`);
  } else {
    const images = execFileSync("podman", ["images", "--filter", `label=botler.data=${data}`, "--format", "{{.Repository}}:{{.Tag}}"], { encoding: "utf8" })
      .split("\n")
      .filter(Boolean);
    if (images.length) execFileSync("podman", ["rmi", ...images], { stdio: "inherit" });
    rmSync(data, { recursive: true, force: true });
  }
}
process.exit(failed ? 1 : 0);
