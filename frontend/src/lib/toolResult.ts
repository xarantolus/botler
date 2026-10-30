// How a tool result is shown: a best-effort reading of its text (often JSON) as link cards, a
// table, a key/value list, Markdown, an image or, failing all of those, code. A render hint the
// tool declared (`@tool(render=...)`, `ToolRender`) is tried first; when the result does not fit
// it, the heuristics decide as if there were none.

import type { ToolRender } from "../generated";
import { detectLanguage } from "./highlight";

export interface LinkItem {
  title: string;
  url: string;
  host: string;
  snippet: string;
}

/** Top-level scalar fields shown next to a list (e.g. `backend`, `cost_usd`), as label/value. */
export type Meta = [string, string][];

export type ResultView =
  | { kind: "error"; text: string }
  | { kind: "links"; items: LinkItem[]; meta: Meta }
  | { kind: "table"; columns: string[]; rows: Record<string, string>[]; meta: Meta }
  | { kind: "keyvalue"; entries: [string, string][] }
  | { kind: "markdown"; text: string }
  | { kind: "image"; src: string }
  | { kind: "code"; text: string };

type Obj = Record<string, unknown>;

const LIST_KEYS = ["results", "items", "data", "hits", "entries", "links", "rows", "records"];
const URL_KEYS = ["url", "link", "href", "uri"];
const TITLE_KEYS = ["title", "name", "label", "headline"];
const SNIPPET_KEYS = ["snippet", "description", "summary", "excerpt", "content", "text"];
const TABLE_MAX_COLUMNS = 8;
/** Strings longer than this read better as Markdown than as code. */
const LONG_TEXT = 400;

const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const isScalar = (v: unknown) => v === null || ["string", "number", "boolean"].includes(typeof v);
const show = (v: unknown): string => (typeof v === "string" ? v : v === null ? "null" : JSON.stringify(v));

function parseJson(text: string): { value: unknown } | null {
  const t = text.trim();
  if (!/^[[{"]/.test(t)) return null;
  try {
    return { value: JSON.parse(t) };
  } catch {
    // Not (complete) JSON, e.g. truncated output: shown as text.
    return null;
  }
}

function httpUrl(v: unknown): URL | null {
  if (typeof v !== "string") return null;
  try {
    const u = new URL(v);
    return u.protocol === "http:" || u.protocol === "https:" ? u : null;
  } catch {
    // Not a URL.
    return null;
  }
}

/** The list a result holds: itself, the `key` named, or a well-known (or its only) list key. */
function listOf(value: unknown, key?: string): { list: unknown[]; meta: Meta } | null {
  if (Array.isArray(value)) return key ? null : { list: value, meta: [] };
  if (!isObj(value)) return null;
  const arrays = Object.keys(value).filter((k) => Array.isArray(value[k]));
  const name = key ?? LIST_KEYS.find((k) => arrays.includes(k)) ?? (arrays.length === 1 ? arrays[0] : undefined);
  if (name === undefined || !Array.isArray(value[name])) return null;
  const meta: Meta = Object.entries(value)
    .filter(([k, v]) => k !== name && v !== null && isScalar(v))
    .map(([k, v]) => [k, show(v)]);
  return { list: value[name] as unknown[], meta };
}

function pick(o: Obj, key: string | undefined, guesses: string[]): unknown {
  return key !== undefined ? o[key] : o[guesses.find((k) => typeof o[k] === "string") ?? ""];
}

function links(value: unknown, h: Extract<ToolRender, { kind: "links" }> | null): ResultView | null {
  const found = listOf(value, h?.items);
  if (!found || found.list.length === 0) return null;
  const items: LinkItem[] = [];
  for (const it of found.list) {
    if (!isObj(it)) return null;
    const url = httpUrl(pick(it, h?.url, URL_KEYS));
    const title = pick(it, h?.title, TITLE_KEYS);
    const snippet = pick(it, h?.snippet, SNIPPET_KEYS);
    if (!url) return null;
    items.push({
      url: url.href,
      host: url.hostname.replace(/^www\./, ""),
      title: typeof title === "string" && title.trim() ? title : url.href,
      snippet: typeof snippet === "string" ? snippet : "",
    });
  }
  // Without a hint, cards need titles too (a list of bare URLs reads fine as a table).
  if (!h && !found.list.every((it) => TITLE_KEYS.some((k) => typeof (it as Obj)[k] === "string"))) return null;
  return { kind: "links", items, meta: found.meta };
}

function table(value: unknown, h: Extract<ToolRender, { kind: "table" }> | null): ResultView | null {
  const found = listOf(value, h?.items);
  if (!found || found.list.length === 0 || !found.list.every(isObj)) return null;
  const rows = found.list as Obj[];
  let columns = h?.columns;
  if (!columns) {
    // Consistent keys: every row has the first row's keys and no others, mostly scalar values.
    const keys = Object.keys(rows[0]!);
    const same = rows.every((r) => Object.keys(r).length === keys.length && keys.every((k) => k in r));
    const flat = rows.every((r) => Object.values(r).every(isScalar));
    if (!same || !flat || keys.length === 0 || keys.length > TABLE_MAX_COLUMNS) return null;
    columns = keys;
  }
  const cols = columns;
  return {
    kind: "table",
    columns: cols,
    rows: rows.map((r) => Object.fromEntries(cols.map((c) => [c, r[c] === undefined ? "" : show(r[c])]))),
    meta: found.meta,
  };
}

function keyvalue(value: unknown, hinted: boolean): ResultView | null {
  if (!isObj(value) || Object.keys(value).length === 0) return null;
  if (!hinted && !Object.values(value).every(isScalar)) return null;
  return { kind: "keyvalue", entries: Object.entries(value).map(([k, v]) => [k, show(v)]) };
}

function image(v: unknown): ResultView | null {
  if (typeof v !== "string") return null;
  const s = v.trim();
  if (/^data:image\/(png|jpeg|gif|webp|svg\+xml);base64,[A-Za-z0-9+/=]+$/.test(s)) return { kind: "image", src: s };
  const url = httpUrl(s);
  return url && /\.(png|jpe?g|gif|webp|svg|avif)$/i.test(url.pathname) ? { kind: "image", src: url.href } : null;
}

function looksLikeMarkdown(s: string): boolean {
  return /^(#{1,6} |\s*[-*] |\s*\d+\. |```|> )|\*\*[^*]+\*\*|\[[^\]]+\]\([^)]+\)/m.test(s);
}

const field = (value: unknown, name: string | undefined) => (name === undefined ? value : isObj(value) ? value[name] : undefined);

function hinted(h: ToolRender, value: unknown, text: string): ResultView | null {
  switch (h.kind) {
    case "links":
      return links(value, h);
    case "table":
      return table(value, h);
    case "keyvalue":
      return keyvalue(value, true);
    case "markdown": {
      const s = field(value, h.field);
      return typeof s === "string" ? { kind: "markdown", text: s } : null;
    }
    case "image":
      return image(field(value, h.field));
    case "json":
      return { kind: "code", text: value === text ? text : JSON.stringify(value, null, 2) };
  }
}

/** How to show a tool result: its text, whether the call failed, and the tool's hint if any. */
export function resultView(text: string, failed: boolean, hint?: ToolRender | null): ResultView {
  if (failed) return { kind: "error", text };
  const json = parseJson(text);
  const value = json ? json.value : text;
  const fromHint = hint ? hinted(hint, value, text) : null;
  if (fromHint) return fromHint;
  if (json) {
    const v = json.value;
    if (typeof v === "string") return resultView(v, false);
    return links(v, null) ?? table(v, null) ?? keyvalue(v, false) ?? { kind: "code", text: JSON.stringify(v, null, 2) };
  }
  const img = image(text);
  if (img) return img;
  // Long output that is code (or a diff, a shell listing) stays code.
  const lang = detectLanguage(text);
  if ((looksLikeMarkdown(text) && lang !== "diff") || (text.length > LONG_TEXT && !lang)) return { kind: "markdown", text };
  return { kind: "code", text };
}
