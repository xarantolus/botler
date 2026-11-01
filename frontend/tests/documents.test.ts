// @vitest-environment happy-dom
// The Documents view: the list, and one document rendered as Markdown with its author.

import { flushPromises } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";

vi.mock("../src/api/client", async (orig) => ({
  ...(await orig<typeof import("../src/api/client")>()),
  call: vi.fn(async (name: string) => {
    const at = "2026-11-01T12:00:00Z";
    if (name === "list_documents") return [{ name: "flat-shortlist", title: "Flat shortlist", author: "agent:flat-curator", bytes: 20, updated_at: at }];
    if (name === "read_document")
      return { name: "flat-shortlist", title: "Flat shortlist", body: "## Riverside\n\n€1400, bills included", previous_body: null, author: "agent:flat-curator", created_at: at, updated_at: at };
    return {};
  }),
}));

import DocumentsView from "../src/views/DocumentsView.vue";
import { mountInApp } from "./mount";

describe("DocumentsView", () => {
  it("lists documents and shows one with who wrote it", async () => {
    const w = await mountInApp(DocumentsView, { name: "flat-shortlist" });
    await flushPromises();
    expect(w.find("aside").text()).toContain("Flat shortlist");
    expect(w.text()).toContain("€1400, bills included");
    expect(w.text()).toContain("flat-curator");
    expect(w.text()).not.toContain("Previous version");
  });
});
