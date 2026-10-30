// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { resultView } from "./toolResult";

// web.search's output, as the pi extension passes it on (indented JSON).
const search = JSON.stringify(
  {
    backend: "openrouter",
    cost_usd: 0.0012,
    note: null,
    results: [
      { title: "Rust Book", url: "https://doc.rust-lang.org/book/", snippet: "The Rust Programming Language" },
      { title: "", url: "https://www.example.com/x?y=1", snippet: "no title" },
    ],
  },
  null,
  2,
);

describe("resultView", () => {
  it("shows web.search results as link cards with scalar fields as badges", () => {
    expect(resultView(search, false)).toEqual({
      kind: "links",
      items: [
        { title: "Rust Book", url: "https://doc.rust-lang.org/book/", host: "doc.rust-lang.org", snippet: "The Rust Programming Language" },
        { title: "https://www.example.com/x?y=1", url: "https://www.example.com/x?y=1", host: "example.com", snippet: "no title" },
      ],
      meta: [
        ["backend", "openrouter"],
        ["cost_usd", "0.0012"],
      ],
    });
  });

  it("never links anything but http(s)", () => {
    const bad = JSON.stringify([{ title: "x", url: "javascript:alert(1)" }]);
    expect(resultView(bad, false).kind).not.toBe("links");
  });

  it("shows a list of flat objects with consistent keys as a table", () => {
    const rows = JSON.stringify({ data: [{ name: "a", n: 1 }, { name: "b", n: null }] });
    expect(resultView(rows, false)).toEqual({
      kind: "table",
      columns: ["name", "n"],
      rows: [
        { name: "a", n: "1" },
        { name: "b", n: "null" },
      ],
      meta: [],
    });
    // Inconsistent keys or nested values: JSON.
    expect(resultView(JSON.stringify([{ a: 1 }, { b: 2 }]), false).kind).toBe("code");
    expect(resultView(JSON.stringify([{ a: { b: 1 } }]), false).kind).toBe("code");
  });

  it("shows a flat object as key/value pairs", () => {
    expect(resultView('{"words": 3, "ok": true}', false)).toEqual({
      kind: "keyvalue",
      entries: [
        ["words", "3"],
        ["ok", "true"],
      ],
    });
  });

  it("shows an error as an error", () => {
    expect(resultView("ValueError: nope", true)).toEqual({ kind: "error", text: "ValueError: nope" });
  });

  it("renders Markdown and long prose as Markdown, code and short text as code", () => {
    expect(resultView("# Title\n\nSome **bold** text.", false).kind).toBe("markdown");
    expect(resultView("word ".repeat(120), false).kind).toBe("markdown");
    expect(resultView("3", false)).toEqual({ kind: "code", text: "3" });
    expect(resultView("@@ -1 +1 @@\n-a\n+b\n", false).kind).toBe("code");
    expect(resultView('"# Title\\n\\ntext"', false).kind).toBe("markdown");
  });

  it("shows image URLs and data URIs as images", () => {
    expect(resultView("https://example.com/cat.png", false)).toEqual({ kind: "image", src: "https://example.com/cat.png" });
    expect(resultView("data:image/png;base64,iVBORw0KGgo=", false).kind).toBe("image");
    expect(resultView("data:text/html;base64,PHNjcmlwdD4=", false).kind).toBe("code");
  });

  it("falls back to text for malformed JSON", () => {
    const cut = `${search.slice(0, 80)}\n[truncated 400 characters]`;
    expect(resultView(cut, false)).toEqual({ kind: "code", text: cut });
  });

  it("follows a declared hint over the heuristics, and ignores one that does not fit", () => {
    expect(resultView(search, false, { kind: "json" }).kind).toBe("code");
    expect(resultView(search, false, { kind: "table", items: "results", columns: ["url"] })).toMatchObject({
      kind: "table",
      columns: ["url"],
      rows: [{ url: "https://doc.rust-lang.org/book/" }, { url: "https://www.example.com/x?y=1" }],
    });
    const mapped = JSON.stringify({ hits: [{ heading: "H", target: "https://a.example/", body: "b" }] });
    expect(resultView(mapped, false, { kind: "links", items: "hits", title: "heading", url: "target", snippet: "body" })).toMatchObject({
      kind: "links",
      items: [{ title: "H", url: "https://a.example/", snippet: "b" }],
    });
    expect(resultView('{"md": "# hi"}', false, { kind: "markdown", field: "md" })).toEqual({ kind: "markdown", text: "# hi" });
    // A links hint on a result without links: the heuristics decide.
    expect(resultView('{"a": 1}', false, { kind: "links" }).kind).toBe("keyvalue");
  });
});
