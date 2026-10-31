// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { renderInline, renderMarkdown } from "./markdown";
import { references, refTarget } from "./references";

const proposal = "01a191d2-394a-7400-80ab-ef4257cde8ce";
const refs = references({
  instances: [{ name: "botler" }, { name: "dev-3f2a" }, { name: "chat-177b10e0960f" }],
  proposals: [{ id: proposal }],
  dialogs: [{ id: "01a191d3-0000-7000-8000-000000000001" }],
  jobs: ["web-health", "backup"],
  agents: ["chat", "dev"],
  secretGroups: ["claude-code"],
});
const md = (src: string) => renderMarkdown(src, refs);
/** The links in `html`: [text, href]. */
const links = (html: string) => {
  const d = document.createElement("div");
  d.innerHTML = html;
  return [...d.querySelectorAll("a")].map((a) => [a.textContent, a.getAttribute("href"), a.hasAttribute("data-ref")]);
};

describe("references in Markdown", () => {
  it("links code spans that are exactly a known name or id, keeping the code", () => {
    const html = md("Ask `dev-3f2a`, run `web-health`, see `botler`, group `claude-code`, agent `dev`.");
    expect(links(html)).toEqual([
      ["dev-3f2a", "/chat/dev-3f2a", true],
      ["web-health", "/system?job=web-health", true],
      ["botler", "/chat/botler", true],
      ["claude-code", "/secrets", true],
      ["dev", "/settings", true],
    ]);
    expect(html).toContain('<a data-ref="" href="/chat/dev-3f2a"><code>dev-3f2a</code></a>');
  });

  it("links distinctive chat names and full ids in prose", () => {
    expect(links(md(`I asked dev-3f2a and chat-177b10e0960f. Proposal ${proposal} is up.`))).toEqual([
      ["dev-3f2a", "/chat/dev-3f2a", true],
      ["chat-177b10e0960f", "/chat/chat-177b10e0960f", true],
      [proposal, `/proposals/${proposal}`, true],
    ]);
    expect(links(md("A request: 01a191d3-0000-7000-8000-000000000001."))).toEqual([
      ["01a191d3-0000-7000-8000-000000000001", "/requests", true],
    ]);
  });

  it("leaves ordinary words, unknown names and look-alikes plain", () => {
    for (const src of [
      "botler runs the backup for dev and chat", // prose words that are also names
      "`backups`, `dev-3f2a2`, `01a191d2-394a-7400-80ab-000000000000`", // unknown
      "on agents/dev-3f2a/work and in dev-3f2a.log", // part of a path or file name
      "dev-3f2ab and xdev-3f2a", // longer words
      "Tom &amp; dev-3f2a-old", // entities, longer names
    ]) {
      expect(links(md(src)), src).toEqual([]);
    }
  });

  it("never links inside code blocks or existing links", () => {
    expect(links(md("```\nchat dev-3f2a, `dev-3f2a`\n```\n\n    indented dev-3f2a"))).toEqual([]);
    expect(links(md("[talk to dev-3f2a](https://example.com) and [`dev-3f2a`](https://example.com)"))).toEqual([
      ["talk to dev-3f2a", "https://example.com", false],
      ["dev-3f2a", "https://example.com", false],
    ]);
  });

  it("links in inline Markdown and headings too, and only with references given", () => {
    expect(links(renderInline("for `backup`", refs))).toEqual([["backup", "/system?job=backup", true]]);
    expect(links(md("## dev-3f2a"))).toEqual([["dev-3f2a", "/chat/dev-3f2a", true]]);
    expect(links(renderMarkdown("`dev-3f2a` and dev-3f2a"))).toEqual([]);
  });

  it("routes plain left clicks on reference links only", () => {
    const d = document.createElement("div");
    d.innerHTML = '<a data-ref href="/chat/dev-3f2a"><code>dev-3f2a</code></a><a href="https://example.com">x</a><a data-ref href="//evil.example">y</a>';
    const [ref, ext, evil] = [...d.querySelectorAll("a")];
    const click = (el: Element, init: MouseEventInit = {}) => {
      const e = new MouseEvent("click", { button: 0, ...init });
      Object.defineProperty(e, "target", { value: el });
      return refTarget(e);
    };
    expect(click(ref!.firstElementChild!)).toBe("/chat/dev-3f2a");
    expect(click(ref!, { ctrlKey: true }), "a new tab is the browser's").toBeNull();
    expect(click(ext!)).toBeNull();
    expect(click(evil!)).toBeNull();
  });

  it("changes its key only when the names change", () => {
    const again = references({
      instances: [{ name: "botler" }, { name: "dev-3f2a" }, { name: "chat-177b10e0960f" }],
      proposals: [{ id: proposal }],
      dialogs: [{ id: "01a191d3-0000-7000-8000-000000000001" }],
      jobs: ["web-health", "backup"],
      agents: ["chat", "dev"],
      secretGroups: ["claude-code"],
    });
    expect(again.key).toBe(refs.key);
    expect(references({ instances: [], proposals: [], dialogs: [], jobs: ["backup"], agents: [], secretGroups: [] }).key).not.toBe(refs.key);
  });
});
