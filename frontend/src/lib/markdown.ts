import DOMPurify from "dompurify";
import { Marked } from "marked";
import { highlight } from "./highlight";

const marked = new Marked({
  gfm: true,
  breaks: true,
  async: false,
  // Fenced and indented code, highlighted (lib/highlight.ts escapes it).
  renderer: {
    code({ text, lang }) {
      const { html, language } = highlight(text, lang);
      return `<pre><code class="hljs"${language ? ` data-language="${language}"` : ""}>${html}</code></pre>\n`;
    },
  },
});

// One configuration for every sanitize (highlight.ts too): HTML only. Set once, as passing it to
// each call parses it again every time, a large part of rendering a long chat.
DOMPurify.setConfig({ USE_PROFILES: { html: true } });

// Links open outside the app and never leak the referrer.
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

/** Agent text is untrusted: render markdown, then sanitize. */
export function renderMarkdown(src: string): string {
  const html = marked.parse(src) as string;
  return DOMPurify.sanitize(html);
}

/** Inline markdown (no paragraphs), e.g. for summary lines with `code`. */
export function renderInline(src: string): string {
  const html = marked.parseInline(src) as string;
  return DOMPurify.sanitize(html);
}
