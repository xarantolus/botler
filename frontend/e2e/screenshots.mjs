// The README's screenshots (docs/screenshots/), taken from a throwaway hub with real agents and a
// scripted model (harness.mjs): a reminder, a change Botler proposes and the reviewer approves, a
// task handed to a `dev` chat that asks the user a question, and the same chat on a phone.
//
//   cargo build -p botler && cd frontend && node e2e/screenshots.mjs
//
// Env: BOTLER_E2E_CHROMIUM (a Chromium binary; default: Playwright's), BOTLER_SHOTS (output
// directory; default ../docs/screenshots), and those of harness.mjs.

import { mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { startHub, step, textOf } from "./harness.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(process.env.BOTLER_SHOTS ?? join(here, "../../docs/screenshots"));
mkdirSync(out, { recursive: true });

const REMIND = "Remind me tomorrow at 8 to call the landlord about the heating.";
const BRIEFING = "Could you send me a short briefing every weekday morning? Weather, my calendar, and anything that needs me.";
const FLAT = "I'm looking for a flat: under €1500, two rooms, near the river. Can you keep an eye on the listing sites?";

/** Tomorrow 08:00 on this machine (the hub's time zone), as RFC 3339 with its offset. */
function tomorrowAt8() {
  const d = new Date();
  d.setDate(d.getDate() + 1);
  d.setHours(8, 0, 0, 0);
  const off = -d.getTimezoneOffset();
  const pad = (n) => String(Math.abs(n)).padStart(2, "0");
  const day = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  return `${day}T08:00:00${off >= 0 ? "+" : "-"}${pad(Math.trunc(off / 60))}:${pad(off % 60)}`;
}

const TRIGGER = `version = 1

[[trigger]]
name = "morning-briefing"
cron = "30 7 * * 1-5"
target = { resume = "botler" }
prompt = "Write the user's morning briefing: today's weather, their calendar, and anything waiting for them."
`;

/** A hub op as the model calls it: from a codemode script (only a few hub tools are direct). */
const hub = (op, args) => ({ tool: "codemode", args: { code: `return await tools.mcp__botler__${op}(${JSON.stringify(args)});` } });

function decide(messages) {
  const system = textOf(messages[0]);
  const last = messages.at(-1);
  const call = messages.findLast((m) => m.role === "assistant")?.tool_calls?.[0]?.function;
  // A codemode call by the op its script calls.
  const lastCall = call?.name === "codemode" ? (/mcp__botler__(\w+)/.exec(call.arguments)?.[1] ?? "codemode") : (call?.name ?? "");
  const user = textOf(messages.findLast((m) => m.role === "user"));

  if (system.includes("You are the reviewer")) {
    const id = /proposal_id="([^"]+)"/.exec(messages.map(textOf).join("\n"))?.[1];
    return {
      tool: "submit_review",
      args: { proposal_id: id, verdict: "approve", notes: "Adds one weekday trigger that wakes botler. No new access, no new code." },
    };
  }
  if (system.includes("You are a `dev` agent")) {
    if (last?.role === "tool") return { text: "Noted. I'll start with those two and show you the first results before anything runs on a schedule." };
    return {
      tool: "ask_user",
      args: { question: "Which listing sites should I watch?", options: ["The two big portals", "Portals and letting agents", "Only the ones you use now"] },
    };
  }
  if (last?.role === "tool") {
    if (lastCall.endsWith("set_timer")) return { text: "Done. I'll remind you **tomorrow at 08:00** to call the landlord about the heating." };
    if (lastCall === "bash" && textOf(last).includes("pushed")) {
      return {
        text:
          "I've proposed **Morning briefing on weekdays**: a trigger that wakes me at 07:30, Monday to Friday, " +
          "to write you a briefing with the weather, your calendar and anything waiting for you.\n\n" +
          "It's being checked and reviewed now; I'll tell you once it's live.",
        tool: "propose_change",
        args: { branch: "agents/botler/morning-briefing", title: "Morning briefing on weekdays" },
      };
    }
    if (lastCall.endsWith("spawn_agent")) {
      return {
        text:
          "I've started **Flat scout** to build this. The plan:\n\n" +
          "1. A job that checks the listing sites every morning and keeps what matches.\n" +
          "2. A short list as a document you can open any time.\n" +
          "3. A message to you only for the really good ones.\n\n" +
          "It will ask you which sites to watch; you can open it in the sidebar.",
      };
    }
    return { text: "Done." };
  }
  if (user.includes(REMIND)) return hub("set_timer", { at: tomorrowAt8(), prompt: "Remind the user to call the landlord about the heating." });
  if (user.includes(BRIEFING)) {
    const command =
      `cd ~/monorepo && git checkout -qb morning-briefing && cat > triggers/morning-briefing.toml <<'EOF'\n${TRIGGER}EOF\n` +
      "git add triggers && git -c user.name=botler -c user.email=botler@localhost commit -qm 'Morning briefing on weekdays' " +
      "&& git push -q origin HEAD:agents/botler/morning-briefing && echo pushed";
    return { tool: "bash", args: { command } };
  }
  if (user.includes(FLAT)) {
    return hub("spawn_agent", { definition: "dev", slug: "flat-scout", title: "Flat scout", task: `The user asked: "${FLAT}" Build a daily job for it.` });
  }
  return { text: "Hi! What can I do for you?" };
}

const { url, botler, log, stop } = await startHub(decide);
let browser;
let failed = false;
try {
  browser = await chromium.launch({ executablePath: process.env.BOTLER_E2E_CHROMIUM });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  const page = await context.newPage();
  const shot = (name) => page.screenshot({ path: join(out, `${name}.png`) });

  step("sign in");
  await page.goto(`${url}/#code=${botler("login-code").trim()}`);
  const composer = page.locator("#chat-composer textarea");
  await composer.waitFor({ timeout: 60_000 });
  const say = async (text, until) => {
    await composer.fill(text);
    await composer.press("Enter");
    await page.getByText(until).first().waitFor({ timeout: 8 * 60_000 }); // the first wake builds the image
  };

  step("a reminder and a task for a dev chat");
  await say(REMIND, "tomorrow at 08:00");
  await say(FLAT, "you can open it in the sidebar");
  await page.getByText("Flat scout").first().waitFor({ timeout: 120_000 });
  // Before the dev chat's first turn and any proposal notes arrive.
  await page.waitForTimeout(1500); // the list settles at its end
  await page.mouse.click(10, 890); // nothing focused or hovered
  await shot("chat");

  step("the chat on a phone, dark");
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, colorScheme: "dark", isMobile: true, hasTouch: true });
  const p = await phone.newPage();
  await p.goto(`${url}/#code=${botler("login-code").trim()}`);
  await p.getByText("you can open it in the sidebar").waitFor({ timeout: 60_000 });
  await p.waitForTimeout(1500);
  await p.screenshot({ path: join(out, "phone.png") });
  await phone.close();

  step("a change Botler proposes");
  await say(BRIEFING, "It's being checked and reviewed now");

  step("the dev chat's question");
  await page.getByRole("link", { name: "Requests" }).click();
  // Its first wake builds the dev image.
  await page.getByText("Which listing sites should I watch?").first().waitFor({ timeout: 15 * 60_000 });
  // Its top: the card and the menu, not the empty page under them.
  await page.screenshot({ path: join(out, "requests.png"), clip: { x: 0, y: 0, width: 1440, height: 480 } });

  step("the proposal, once merged (the gate builds its image first)");
  await page.getByRole("link", { name: "Proposals" }).click();
  await page.getByText("Morning briefing on weekdays").filter({ visible: true }).first().click({ timeout: 60_000 });
  const merged = page.getByText("merged", { exact: true }).first();
  const approve = page.getByRole("button", { name: "Approve", exact: true });
  await merged.or(approve).first().waitFor({ timeout: 20 * 60_000 });
  if (await approve.isVisible()) await approve.click(); // when the summary asks for the user
  await merged.waitFor({ timeout: 5 * 60_000 });
  await shot("proposal");

  console.log(`\nscreenshots in ${out}`);
} catch (e) {
  failed = true;
  console.error(`\nscreenshots FAILED: ${e instanceof Error ? e.message : e}\n--- hub log (tail) ---\n${log().slice(-4000)}`);
} finally {
  await browser?.close();
  await stop();
}
process.exit(failed ? 1 : 0);
