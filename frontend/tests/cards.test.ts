// @vitest-environment happy-dom
// The cards of the chat and the inbox, rendered with the real kit.

import { flushPromises } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ChangeSummary from "../src/components/common/ChangeSummary.vue";
import DialogCard from "../src/components/common/DialogCard.vue";
import EnvelopeCard from "../src/components/common/EnvelopeCard.vue";
import MessageParts from "../src/components/chat/MessageParts.vue";
import type { Dialog, DialogView } from "../src/generated";
import { buildTranscript, noLive } from "../src/lib/transcript";
import { mountInApp } from "./mount";

const calls = vi.hoisted(() => [] as { name: string; args: unknown }[]);
vi.mock("../src/api/client", async (orig) => ({
  ...(await orig<typeof import("../src/api/client")>()),
  call: vi.fn(async (name: string, args: unknown) => {
    calls.push({ name, args });
    if (name === "dialogs") return { open: [], recent: [] };
    return {};
  }),
}));

const dialog = (extra: Partial<DialogView> = {}): DialogView => ({
  id: "d1",
  instance: "botler",
  kind: "question",
  question: "Which calendar?",
  detail: "I found **two**.",
  ctas: ["work", "personal"],
  about: [],
  priority: "normal",
  proposal_id: null,
  spec: null,
  status: "open",
  answer: null,
  abort_reason: null,
  expires_at: null,
  created_at: "2026-10-30T10:00:00Z",
  resolved_at: null,
  ...extra,
});

beforeEach(() => {
  calls.length = 0;
});
afterEach(() => {
  document.body.innerHTML = "";
});

describe("DialogCard", () => {
  it("answers a question with one of its choices", async () => {
    const w = await mountInApp(DialogCard, { dialog: dialog() });
    expect(w.text()).toContain("Which calendar?");
    expect(w.find(".md strong").text()).toBe("two");
    const buttons = w.findAll("button").filter((b) => ["work", "personal"].includes(b.text()));
    expect(buttons.map((b) => b.text())).toEqual(["work", "personal"]);
    await buttons[0]!.trigger("click");
    await flushPromises();
    expect(calls[0]).toEqual({ name: "answer_dialog", args: { params: { id: "d1" }, body: { answer: "work" } } });
  });

  it("shows the outcome of a closed dialog without actions", async () => {
    const w = await mountInApp(DialogCard, { dialog: dialog({ status: "answered", answer: "personal" }) });
    expect(w.text()).toContain("answered");
    expect(w.text()).toContain("Answer: personal");
    expect(w.findAll("button").filter((b) => b.text() === "work")).toHaveLength(0);
  });

  it("asks for each secret and saves only when all are filled", async () => {
    const w = await mountInApp(DialogCard, {
      dialog: dialog({
        kind: "credentials",
        question: "Deploy access",
        detail: null,
        ctas: [],
        group: "deploy",
        new_group: true,
        form: [{ name: "DEPLOY_TOKEN", description: "Project token", file: null }],
      }),
    });
    expect(w.text()).toContain("DEPLOY_TOKEN");
    const save = w.findAll("button").find((b) => b.text() === "Save secrets")!;
    expect(save.attributes("disabled")).toBeDefined();
    await w.find("input[type=password]").setValue("s3cret");
    expect(save.attributes("disabled")).toBeUndefined();
    await save.trigger("click");
    await flushPromises();
    expect(calls[0]).toEqual({
      name: "submit_secrets",
      args: { params: { id: "d1" }, body: { values: { DEPLOY_TOKEN: "s3cret" }, decline: false } },
    });
  });

  it("renders an approval's change summary", async () => {
    const w = await mountInApp(DialogCard, {
      dialog: dialog({
        kind: "approval",
        ctas: ["approve", "reject"],
        detail: "**Will start happening:**\n- at 07:00: message `botler`\n\n**Gate:** passed",
      }),
    });
    expect(w.text()).toContain("Approval");
    expect(w.text()).toContain("Will start happening");
    expect(w.text()).toContain("passed");
    expect(w.findAll("button").map((b) => b.text())).toEqual(expect.arrayContaining(["approve", "reject"]));
  });

  it("rejects an approval only with a reason, which reaches the hub", async () => {
    const w = await mountInApp(DialogCard, {
      dialog: dialog({ kind: "approval", ctas: ["approve", "reject"], proposal_id: "p1", detail: null }),
    });
    expect(w.find('a[href="/proposals/p1"]').text()).toContain("Review changes");
    await w.findAll("button").find((b) => b.text() === "reject")!.trigger("click");
    await flushPromises();
    const modal = () => document.querySelector('[role="dialog"]')!;
    const confirmButton = () => [...modal().querySelectorAll("button")].find((b) => b.textContent?.trim() === "Reject")!;
    expect(modal().textContent).toContain("Reject this change?");
    expect(confirmButton().disabled).toBe(true);
    const box = modal().querySelector("textarea")!;
    box.value = "   ";
    box.dispatchEvent(new Event("input"));
    await flushPromises();
    expect(confirmButton().disabled, "blank is no reason").toBe(true);
    box.value = "Not on weekdays";
    box.dispatchEvent(new Event("input"));
    await flushPromises();
    expect(confirmButton().disabled).toBe(false);
    confirmButton().click();
    await flushPromises();
    expect(calls.find((c) => c.name === "answer_dialog")).toEqual({
      name: "answer_dialog",
      args: { params: { id: "d1" }, body: { answer: "reject", reason: "Not on weekdays" } },
    });
  });
});

describe("EnvelopeCard", () => {
  it("shows who sent what, how far it is trusted, and JSON bodies indented", async () => {
    const w = await mountInApp(EnvelopeCard, {
      env: {
        from: "runnable:poller",
        provenance: "external",
        kind: "mail.received",
        delivery: "dl",
        event: "ev1",
        localTime: "Fri 17:05",
        gap: "first message in 3 days",
        body: '{"subject":"hi"}',
        malformed: false,
      },
      at: "2026-10-30T10:00:00Z",
    });
    const text = w.text();
    expect(text).toContain("mail.received");
    expect(text).toContain("runnable:poller");
    expect(text).toContain("external");
    expect(text).toContain("first message in 3 days");
    expect(w.find("pre").text()).toBe('{\n  "subject": "hi"\n}');
    expect(w.find("a[href*='event=ev1']").exists()).toBe(true);
  });
});

describe("ChangeSummary", () => {
  it("renders plain markdown when it is not a summary", async () => {
    const w = await mountInApp(ChangeSummary, { markdown: "just *text*" });
    expect(w.find(".md em").text()).toBe("text");
  });

  it("shows changes as before and after", async () => {
    const w = await mountInApp(ChangeSummary, {
      markdown: '**Will change:**\n- `nightly`: was "at 02:00", becomes "at 03:00"\n\n**Check this (likely mistakes):**\n- fires at 03:00',
    });
    expect(w.text()).toContain("Will change");
    expect(w.text()).toContain("nightly");
    expect(w.text()).toMatch(/was\s*at 02:00/);
    expect(w.text()).toMatch(/now\s*at 03:00/);
    expect(w.text()).toContain("likely mistakes");
  });
});

describe("MessageParts", () => {
  it("renders reasoning, text, tool calls and dialogs of a turn", async () => {
    const d: Dialog = dialog({ id: "q1" });
    const t = buildTranscript({
      items: [
        {
          kind: "assistant",
          id: "a1",
          at: null,
          text: "Let me **check**.",
          thinking: "hmm",
          tool_calls: [
            { id: "c1", name: "bash", arguments: { command: "ls -la" } },
            { id: "c2", name: "ask_user", arguments: {} },
          ],
          model: null,
          cost_usd: null,
          error: null,
          message_ts: null,
        },
        {
          kind: "tool_result",
          id: "r2",
          at: null,
          tool: "ask_user",
          tool_call_id: "c2",
          text: "{}",
          is_error: false,
          dialog_id: "q1",
          proposal_id: null,
        },
      ],
      dialogs: [d],
      queued: [],
      streaming: noLive(),
      busy: true,
    });
    const w = await mountInApp(MessageParts, { message: t.messages[0], busy: true });
    const text = w.text();
    expect(text).toMatch(/Thinking|Thought/);
    expect(w.find(".md strong").text()).toBe("check");
    expect(text).toContain("bash");
    expect(text).toContain("ls -la");
    expect(text).toContain("Which calendar?");
  });
});
