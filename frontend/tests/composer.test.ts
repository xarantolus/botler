// @vitest-environment happy-dom
// Attaching files in the composer: each uploads right away; the message names them; a failed
// upload blocks sending until it is removed.

import { flushPromises } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => [] as { name: string; args: unknown }[]);
vi.mock("../src/api/client", async (orig) => ({
  ...(await orig<typeof import("../src/api/client")>()),
  call: vi.fn(async (name: string, args: unknown) => {
    calls.push({ name, args });
    return {};
  }),
  upload: vi.fn(async (f: File, progress: (p: number) => void) => {
    progress(1);
    if (f.name === "bad.bin") throw new Error("too large");
    return { file: `abc123/${f.name}`, size: f.size };
  }),
}));

import ChatComposer from "../src/components/chat/ChatComposer.vue";
import { mountInApp } from "./mount";

afterEach(() => {
  calls.length = 0;
  document.body.innerHTML = "";
});

async function pick(w: Awaited<ReturnType<typeof mountInApp>>, files: File[]) {
  const input = w.find("input[type=file]");
  Object.defineProperty(input.element, "files", { value: files, configurable: true });
  await input.trigger("change");
  await flushPromises();
}

describe("ChatComposer attachments", () => {
  it("sends the uploaded files with the message", async () => {
    const w = await mountInApp(ChatComposer, { instance: "botler", archived: false, busy: false });
    await pick(w, [new File(["x"], "notes.txt")]);
    expect(w.text()).toContain("notes.txt");
    await w.find("textarea").setValue("see attached");
    await w.find("form").trigger("submit");
    await flushPromises();
    expect(calls.find((c) => c.name === "send_message")?.args).toEqual({
      body: { to: "botler", text: "see attached", mode: "steer", files: ["abc123/notes.txt"] },
    });
    expect(w.text()).not.toContain("notes.txt");
  });

  it("blocks sending while an upload failed, until it is removed", async () => {
    const w = await mountInApp(ChatComposer, { instance: "botler", archived: false, busy: false });
    await pick(w, [new File(["x"], "bad.bin")]);
    await w.find("textarea").setValue("hi");
    expect(w.text()).toContain("Remove the files that failed");
    await w.find("form").trigger("submit");
    await flushPromises();
    expect(calls.some((c) => c.name === "send_message")).toBe(false);
    await w.find("button[aria-label='Remove bad.bin']").trigger("click");
    await w.find("form").trigger("submit");
    await flushPromises();
    expect(calls.find((c) => c.name === "send_message")?.args).toEqual({ body: { to: "botler", text: "hi", mode: "steer", files: undefined } });
  });
});

describe("ChatComposer while the agent works", () => {
  const modes = () => calls.filter((c) => c.name === "send_message").map((c) => (c.args as { body: { mode: string } }).body.mode);

  it("sends with Enter (read after the running step) and queues until done with Ctrl+Enter", async () => {
    const w = await mountInApp(ChatComposer, { instance: "botler", archived: false, busy: true });
    const box = w.find("textarea");
    await box.setValue("look at this too");
    await box.trigger("keydown", { key: "Enter" });
    await flushPromises();
    await box.setValue("and then this");
    await box.trigger("keydown", { key: "Enter", ctrlKey: true });
    await flushPromises();
    await box.setValue("and this");
    await w.findAll("button").find((b) => b.text() === "When done")!.trigger("click");
    await flushPromises();
    expect(modes()).toEqual(["steer", "queue", "queue"]);
  });
});
