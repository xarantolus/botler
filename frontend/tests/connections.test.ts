// @vitest-environment happy-dom
// The chat's Connections slideover: grants with Revoke, subscriptions with remove, related chats.

import { flushPromises } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import ConnectionsSlideover from "../src/components/chat/ConnectionsSlideover.vue";
import type { Connections } from "../src/generated";
import { mountInApp } from "./mount";

const calls = vi.hoisted(() => [] as { name: string; args: unknown }[]);
const data: Connections = {
  chat: "dev-1",
  archived: false,
  parent: "botler",
  children: ["dev-2"],
  definition_secrets: ["openrouter"],
  grants: [
    {
      id: "g1",
      holder: "dev-1",
      kind: "secret",
      resource: "github-ssh",
      mode: null,
      granted_by: "user",
      created_at: "2026-10-30T10:00:00Z",
      revoked_at: null,
      suspended: false,
    },
  ],
  services: ["notes"],
  subscriptions: [
    {
      id: "s1",
      instance: "dev-1",
      pattern: "mail.received",
      filter: { folder: "INBOX" },
      wake: false,
      note: "triage",
      created_at: "2026-10-30T10:00:00Z",
    },
  ],
  timers: [],
};
vi.mock("../src/api/client", async (orig) => ({
  ...(await orig<typeof import("../src/api/client")>()),
  call: vi.fn(async (name: string, args: unknown) => {
    calls.push({ name, args });
    return name === "connections" ? data : {};
  }),
}));

afterEach(() => {
  document.body.innerHTML = "";
});

describe("ConnectionsSlideover", () => {
  it("shows the chat's edges and removes a subscription", async () => {
    await mountInApp(ConnectionsSlideover, { name: "dev-1", open: true });
    await flushPromises();
    expect(calls[0]).toEqual({ name: "connections", args: { body: { chat: "dev-1" } } });
    const text = document.body.textContent ?? "";
    for (const s of ["secret:github-ssh", "openrouter", "mail.received", 'folder = "INBOX"', "quiet", "botler", "dev-2", "notes"]) {
      expect(text).toContain(s);
    }
    expect([...document.querySelectorAll("button")].some((b) => b.textContent?.includes("Revoke"))).toBe(true);
    (document.querySelector('button[aria-label="Remove subscription"]') as HTMLButtonElement).click();
    await flushPromises();
    expect(calls).toContainEqual({ name: "unsubscribe", args: { body: { id: "s1" } } });
  });
});
