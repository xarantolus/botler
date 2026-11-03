// End-to-end smoke test of the web app against a throwaway hub with real agents (Podman) and a
// scripted model. Run locally, not in CI: it builds the agent image (minutes the first time).
//
//   cargo build -p botler && cd frontend && npm run e2e
//
// It creates a scratch data directory, points the models at a stub OpenAI-compatible server
// (this file), starts `botler serve` on 127.0.0.1:BOTLER_E2E_PORT (default 7437, never 7420; its
// agents reach it over its Unix socket), and drives the
// app in headless Chromium: sign in, send a message, attach a file, see the files the agent shows (a
// picture it read, a video and a PDF it attached), answer the agent's question inline, ask for a
// change and reject the proposal, and run a job from the System view when there is one. At the
// end it stops the hub, which removes its agent containers, then removes the images labelled with
// the scratch data directory and the directory itself. Set BOTLER_E2E_KEEP=1 to keep them.
//
// Env: BOTLER_E2E_CHROMIUM (a Chromium binary; default: Playwright's), BOTLER_E2E_HEADED=1,
// BOTLER_E2E_SHOTS (directory for screenshots), and those of harness.mjs.

import { mkdirSync } from "node:fs";
import { crc32, deflateSync } from "node:zlib";
import { join } from "node:path";
import { chromium } from "playwright-core";
import { startHub, step, textOf } from "./harness.mjs";

const shots = process.env.BOTLER_E2E_SHOTS;
if (shots) mkdirSync(shots, { recursive: true });

// ---- the scripted model ----------------------------------------------------------------------
// An OpenAI-compatible chat completions endpoint that answers from the last message: a request to
// "ask" calls ask_user, an answer to it is acknowledged, "propose" commits and proposes a change,
// anything else gets a short reply.

/** A real PNG (a 64x40 gradient), base64: pi's `read` decodes it. */
function png() {
  const chunk = (type, data) => {
    const out = Buffer.alloc(12 + data.length);
    out.writeUInt32BE(data.length, 0);
    out.write(type, 4, "latin1");
    data.copy(out, 8);
    out.writeUInt32BE(crc32(Buffer.concat([Buffer.from(type, "latin1"), data])), 8 + data.length);
    return out;
  };
  const [w, h] = [64, 40];
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  const rows = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) rows.set([x * 4, y * 6, 160], y * (w * 3 + 1) + 1 + x * 3);
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  return Buffer.concat([sig, chunk("IHDR", ihdr), chunk("IDAT", deflateSync(rows)), chunk("IEND", Buffer.alloc(0))]).toString("base64");
}
const SHOT = png();
const PDF = Buffer.from("%PDF-1.1\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n").toString("base64");

function decide(messages) {
  // The image `read` returned follows its tool message as a user message.
  const lastCall = messages.findLast((m) => m.role === "assistant")?.tool_calls?.[0]?.function?.name;
  const afterRead = lastCall === "read" && messages.at(-1)?.role === "user" && messages.at(-2)?.role === "tool";
  const last = afterRead ? messages.at(-2) : messages.at(-1);
  if (last?.role === "tool") {
    const call = messages.findLast((m) => m.role === "assistant")?.tool_calls?.[0];
    if (call?.function?.name === "bash" && textOf(last).includes("e2e-commit")) {
      return { tool: "propose_change", args: { branch: "agents/botler/work", title: "Add an e2e note" } };
    }
    if (call?.function?.name === "bash" && textOf(last).includes("e2e-files")) return { tool: "read", args: { path: "/home/agent/out/shot.png" } };
    if (call?.function?.name === "read") return { tool: "attach_files", args: { files: ["out/demo.mp4", "~/out/report.pdf"] } };
    if (call?.function?.name.endsWith("attach_files")) return { text: "Here are the files." };
    return { text: "Done." };
  }
  const said = textOf(last).toLowerCase();
  if (/\bblue\b/.test(said)) return { text: "You chose blue." };
  if (said.includes("please show me files")) {
    const command = `mkdir -p ~/out && cd ~/out && echo ${SHOT} | base64 -d > shot.png && echo ${PDF} | base64 -d > report.pdf && head -c 4096 /dev/urandom > demo.mp4 && echo e2e-files`;
    return { tool: "bash", args: { command } };
  }
  if (said.includes("please ask me something")) return { tool: "ask_user", args: { question: "Which colour?", options: ["red", "blue"] } };
  if (said.includes("please propose a change")) {
    const command =
      "cd ~/monorepo && echo e2e >> README.md && git add README.md && git -c user.name=e2e -c user.email=e2e@localhost commit -qm 'Add an e2e note' && git push -q origin HEAD:agents/botler/work && echo e2e-commit";
    return { tool: "bash", args: { command } };
  }
  return { text: "Hello from the stub model." };
}

const { url, token, botler, log, stop } = await startHub(decide);

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
  step("sign in with a one-time code");
  await page.goto(url);
  await page.getByLabel("Sign-in code or token").fill(botler("login-code").trim());
  await page.getByRole("button", { name: "Sign in" }).click();
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

  step("see what the agent shows: a picture it read, a video and a PDF it attached");
  await composer.fill("Please show me files");
  await composer.press("Enter");
  await page.getByText("Here are the files.").waitFor({ timeout: 120_000 });
  // Shown next to the folded tool calls, without opening them.
  const shown = page.locator("[data-role=assistant]").last();
  await shown.locator("[data-file=image] img").waitFor({ timeout: 30_000 });
  await shown.locator("[data-file=video] video[controls]").waitFor({ timeout: 30_000 });
  const pdf = shown.locator("[data-file=open]").getByRole("button", { name: /report\.pdf/ });
  await pdf.waitFor();
  const size = await shown.locator("[data-file=image] img").evaluate((img) => [img.naturalWidth, img.naturalHeight]);
  if (size[0] !== 64 || size[1] !== 40) throw new Error(`the picture did not load: ${size}`);
  const [tab] = await Promise.all([page.waitForEvent("popup"), pdf.click()]);
  // Headless Chromium has no PDF viewer: it navigates to the blob and aborts that as a download,
  // or downloads it at once.
  await Promise.race([tab.waitForURL(/^blob:/, { timeout: 10_000 }), tab.waitForEvent("download", { timeout: 10_000 })]).catch(
    (e) => {
      if (!String(e).includes("ERR_ABORTED")) throw e;
    },
  );
  await tab.close();
  // The session view references the picture; its bytes stay in the session file.
  const view = await (await fetch(`${url}/api/v1/sessions/botler`, { headers: { authorization: `Bearer ${token}` } })).json();
  const read = view.items.findLast((it) => it.kind === "tool_result" && it.tool === "read");
  if (JSON.stringify(read?.images) !== JSON.stringify([{ index: 1, mime: "image/png" }]) || read.text.length > 1000) {
    throw new Error(`the picture is not a reference: ${JSON.stringify(read).slice(0, 300)}`);
  }
  await snap("files-shown");

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
  // The list has a table and stacked rows for phones; only one of them shows.
  await page.getByText("Add an e2e note").filter({ visible: true }).first().click({ timeout: 180_000 });
  await page.getByRole("button", { name: "Reject" }).click();
  await page.getByLabel("Reason").fill("Only a smoke test.");
  await page.getByRole("dialog").getByRole("button", { name: "Reject" }).click();
  await page.getByText("Only a smoke test.").filter({ visible: true }).first().waitFor({ timeout: 30_000 });
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
  console.error(`\nsmoke test FAILED: ${e instanceof Error ? e.message : e}\n--- hub log (tail) ---\n${log().slice(-4000)}`);
} finally {
  await browser?.close();
  await stop();
}
process.exit(failed ? 1 : 0);
