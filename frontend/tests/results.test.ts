// @vitest-environment happy-dom
// Tool results shown as links and tables, rendered with the real kit.

import { afterEach, describe, expect, it } from "vitest";
import ResultLinks from "../src/components/chat/ResultLinks.vue";
import ResultTable from "../src/components/chat/ResultTable.vue";
import { mountInApp } from "./mount";

afterEach(() => {
  document.body.innerHTML = "";
});

describe("ResultLinks", () => {
  it("links each title out in a new tab without a referrer, with host, snippet and badges", async () => {
    const w = await mountInApp(ResultLinks, {
      items: [
        { title: "Rust <b>Book</b>", url: "https://doc.rust-lang.org/book/", host: "doc.rust-lang.org", snippet: "Learn Rust" },
        { title: "Example", url: "https://example.com/", host: "example.com", snippet: "" },
      ],
      meta: [["backend", "openrouter"]],
    });
    const links = w.findAll("a");
    expect(links.map((a) => a.attributes("href"))).toEqual(["https://doc.rust-lang.org/book/", "https://example.com/"]);
    for (const a of links) {
      expect(a.attributes("target")).toBe("_blank");
      expect(a.attributes("rel")).toBe("noopener noreferrer");
    }
    // Titles are text, never HTML.
    expect(links[0]!.text()).toBe("Rust <b>Book</b>");
    expect(w.findAll("[data-link]")[0]!.text()).toContain("doc.rust-lang.org");
    expect(w.text()).toContain("Learn Rust");
    expect(w.text()).toContain("backend: openrouter");
  });
});

describe("ResultTable", () => {
  const rows = Array.from({ length: 12 }, (_, i) => ({ "file.name": `f${i}`, size: String(i * 10) }));

  it("shows the first rows and all of them on request", async () => {
    const w = await mountInApp(ResultTable, { columns: ["file.name", "size"], rows, meta: [] });
    expect(w.findAll("th").map((th) => th.text())).toEqual(["file.name", "size"]);
    expect(w.findAll("tbody tr")).toHaveLength(8);
    expect(w.find("tbody tr td").text()).toBe("f0");
    const more = w.findAll("button").find((b) => b.text() === "Show all 12 rows");
    expect(more).toBeDefined();
    await more!.trigger("click");
    expect(w.findAll("tbody tr")).toHaveLength(12);
  });
});
