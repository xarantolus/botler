import DOMPurify from "dompurify";
import { Marked, Renderer, type Tokens } from "marked";
import { highlight } from "./highlight";
import { linkProse, noReferences, refLink, type References } from "./references";

/** What the render in progress links (lib/references.ts), and whether it is inside a link. */
let refs: References = noReferences;
let inLink = 0;
const base = Renderer.prototype;

const marked = new Marked({
  gfm: true,
  breaks: true,
  async: false,
  renderer: {
    // Fenced and indented code, highlighted (lib/highlight.ts escapes it). Never linked.
    code({ text, lang }) {
      const { html, language } = highlight(text, lang);
      return `<pre><code class="hljs"${language ? ` data-language="${language}"` : ""}>${html}</code></pre>\n`;
    },
    // Mentions of chats, proposals, jobs, … link to them, except inside a link.
    codespan(token: Tokens.Codespan) {
      const html = base.codespan.call(this, token);
      const path = inLink ? undefined : refs.exact.get(token.text);
      return path ? refLink(path, html) : html;
    },
    text(token: Tokens.Text | Tokens.Escape) {
      const html = base.text.call(this, token);
      return inLink || ("tokens" in token && token.tokens) ? html : linkProse(html, refs);
    },
    link(token: Tokens.Link) {
      inLink++;
      try {
        return base.link.call(this, token);
      } finally {
        inLink--;
      }
    },
  },
});

// One configuration for every sanitize (highlight.ts too): HTML only. Set once, as passing it to
// each call parses it again every time, a large part of rendering a long chat.
DOMPurify.setConfig({ USE_PROFILES: { html: true } });

// Links open outside the app and never leak the referrer; links to the app's own pages (from
// lib/references.ts) stay in it.
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName !== "A") return;
  const href = node.getAttribute("href") ?? "";
  if (node.hasAttribute("data-ref") && href.startsWith("/") && !href.startsWith("//")) return;
  node.removeAttribute("data-ref");
  node.setAttribute("target", "_blank");
  node.setAttribute("rel", "noopener noreferrer");
});

function render(parse: (src: string) => string, src: string, known: References): string {
  refs = known;
  try {
    return DOMPurify.sanitize(parse(src));
  } finally {
    refs = noReferences;
  }
}

/** Agent text is untrusted: render markdown, then sanitize. The names and ids in `known` link to their pages. */
export function renderMarkdown(src: string, known: References = noReferences): string {
  return render((s) => marked.parse(s) as string, src, known);
}

/** Inline markdown (no paragraphs), e.g. for summary lines with `code`. */
export function renderInline(src: string, known: References = noReferences): string {
  return render((s) => marked.parseInline(s) as string, src, known);
}
