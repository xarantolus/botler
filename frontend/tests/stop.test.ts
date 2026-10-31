// @vitest-environment happy-dom
// Stopping a chat: the confirmation's cascade checkbox and reason reach `stop_chat`.

import { flushPromises } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => [] as { name: string; args: unknown }[]);
const answer = vi.hoisted(() => ({ value: null as { reason: string; checked: boolean } | null }));
vi.mock("../src/api/client", async (orig) => ({
  ...(await orig<typeof import("../src/api/client")>()),
  call: vi.fn(async (name: string, args: unknown) => {
    calls.push({ name, args });
    if (name === "stop_chat") return { stopped: [{ chat: "c1", aborted: true, cancelled: 2 }, { chat: "c2", aborted: false, cancelled: 0, with: "c1" }] };
    return {};
  }),
}));
vi.mock("../src/confirm", () => ({ confirm: vi.fn(async () => answer.value) }));

import { stopChat } from "../src/chatActions";
import ConfirmModal from "../src/components/common/ConfirmModal.vue";
import { mountInApp } from "./mount";

afterEach(() => {
  calls.length = 0;
  document.body.innerHTML = "";
});

describe("stopChat", () => {
  it("sends the cascade choice and the reason, and does nothing when cancelled", async () => {
    answer.value = null;
    await stopChat("c1");
    expect(calls.filter((c) => c.name === "stop_chat")).toEqual([]);

    answer.value = { reason: "wrong approach", checked: true };
    await stopChat("c1");
    await flushPromises();
    expect(calls.find((c) => c.name === "stop_chat")?.args).toEqual({ body: { chat: "c1", cascade: true, reason: "wrong approach" } });
  });
});

describe("ConfirmModal", () => {
  it("returns the checkbox with the reason", async () => {
    const w = await mountInApp(ConfirmModal, { open: true, title: "Stop?", confirmLabel: "Stop", check: { label: "Also stop the chats it spawned" }, reason: { label: "Reason" } });
    const modal = w.findComponent(ConfirmModal);
    const box = document.querySelector<HTMLButtonElement>("button[role=checkbox]")!;
    box.click();
    await flushPromises();
    [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Stop")!.click();
    await flushPromises();
    expect(modal.emitted("close")?.[0]).toEqual([{ reason: "", checked: true }]);
  });
});
