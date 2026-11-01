// Mentions of botler's own things in rendered Markdown become links to their page in the app.
// Only what the hub knows resolves (chats, proposals, requests, jobs and services, agent
// definitions, secret groups), so ordinary words and unknown ids stay plain:
//
// - an inline code span that is exactly a known name or id (`dev-3f2a`, `web-health`);
// - in prose, only what cannot be an ordinary word: a chat name with a digit or a hyphen
//   (`chat-177b10e0960f`, `dev-3f2a`) and full ids (proposals, requests).
//
// Code blocks and existing links are never touched (lib/markdown.ts). The links are `<a
// data-ref href="/…">`; MarkdownText routes clicks on them inside the app.

/** Where each name or id leads (an app path). */
export interface References {
  /** Linked as an inline code span. */
  exact: Map<string, string>;
  /** Linked in prose too. */
  prose: Map<string, string>;
  /** Changes with the set of names and ids (not with their state). */
  key: string;
}

export const noReferences: References = { exact: new Map(), prose: new Map(), key: "" };

export interface Known {
  instances: { name: string }[];
  proposals: { id: string }[];
  dialogs: { id: string }[];
  jobs: string[];
  agents: string[];
  secretGroups: string[];
}

/** A word that is never prose: it has a digit or a hyphen. */
const distinctive = /[\d-]/;

export function references(k: Known): References {
  const exact = new Map<string, string>();
  const prose = new Map<string, string>();
  // Later kinds win a name they share with an earlier one (a chat over its definition).
  for (const g of k.secretGroups) exact.set(g, "/secrets");
  for (const a of k.agents) exact.set(a, `/settings?tab=agents&agent=${encodeURIComponent(a)}`);
  for (const j of k.jobs) exact.set(j, `/system?job=${encodeURIComponent(j)}`);
  for (const d of k.dialogs) prose.set(d.id, "/requests");
  for (const p of k.proposals) prose.set(p.id, `/proposals/${encodeURIComponent(p.id)}`);
  for (const i of k.instances) {
    const path = `/chat/${encodeURIComponent(i.name)}`;
    exact.set(i.name, path);
    if (distinctive.test(i.name)) prose.set(i.name, path);
  }
  for (const [name, path] of prose) exact.set(name, path);
  return { exact, prose, key: [...exact].map(([n, p]) => `${n} ${p}`).join("\n") + `\n${prose.size}` };
}

const escapeAttr = (s: string) => s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");

/** `html` (already rendered) as a link to `path`. */
export const refLink = (path: string, html: string) => `<a data-ref href="${escapeAttr(path)}">${html}</a>`;

// A name or id standing alone: not part of a path, file name, URL or HTML entity.
const word = /(?<![\w&#/.:@-])[A-Za-z0-9][\w-]*[A-Za-z0-9](?![\w-]|\.\w)/g;

/** Escaped prose text (no tags) with the names and ids in it linked. */
export function linkProse(html: string, refs: References): string {
  if (!refs.prose.size) return html;
  return html.replace(word, (w) => {
    const path = refs.prose.get(w);
    return path ? refLink(path, w) : w;
  });
}

/** The app path of a link a click landed on, if it is one of these (else the browser handles it). */
export function refTarget(e: MouseEvent): string | null {
  if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || e.defaultPrevented) return null;
  const a = (e.target as Element | null)?.closest?.("a[data-ref]");
  const href = a?.getAttribute("href");
  return href && href.startsWith("/") && !href.startsWith("//") ? href : null;
}
