// The Settings page's logic: what changed (only that is sent), model search and labels, and
// field errors by path.

import type { AgentEdit, CatalogModel, FieldError, JsonValue, ModelEntry, Settings, SettingsEdit } from "../generated";

/** The editable copy of what the hub sent. */
export interface Draft {
  models: Settings["models"];
  agents: Record<string, AgentEdit>;
  defaults: Record<string, unknown>;
  /** null: push is off. */
  push: Record<string, unknown> | null;
  /** The mirror's URL; empty: off. */
  mirror: string;
  /** Per job: its settings with the schema's defaults filled in. */
  runnables: Record<string, Record<string, unknown>>;
}

export function draftOf(s: Settings): Draft {
  const copy = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
  return {
    models: copy(s.models),
    agents: Object.fromEntries(s.agents.map((a) => [a.name, copy(a.edit)])),
    defaults: copy(s.defaults) as Record<string, unknown>,
    push: copy(s.push) as Record<string, unknown> | null,
    mirror: s.mirror ?? "",
    runnables: Object.fromEntries(
      s.runnables.map((r) => [r.job, { ...defaults(r.schema as Schema), ...copy(r.values as Record<string, unknown>) }]),
    ),
  };
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** The edit to send: only the sections and agents that differ from what was loaded. */
export function changes(loaded: Settings, draft: Draft): SettingsEdit | null {
  const before = draftOf(loaded);
  const edit: SettingsEdit = { commit: loaded.commit };
  if (!same(before.models, draft.models)) edit.models = draft.models;
  const agents = Object.fromEntries(Object.entries(draft.agents).filter(([n, a]) => !same(before.agents[n], a)));
  if (Object.keys(agents).length) edit.agents = agents;
  if (!same(before.defaults, draft.defaults) || !same(before.push, draft.push) || before.mirror !== draft.mirror) {
    edit.deployment = { defaults: draft.defaults as JsonValue, push: draft.push as JsonValue, mirror: draft.mirror.trim() || null };
  }
  const runnables = Object.fromEntries(
    Object.entries(draft.runnables).filter(([n, v]) => !same(before.runnables[n], v)),
  ) as Record<string, JsonValue>;
  if (Object.keys(runnables).length) edit.runnables = runnables;
  return edit.models || edit.agents || edit.deployment || edit.runnables ? edit : null;
}

/** The errors at `field` or below it (`agents.chat` gets `agents.chat.model` too). */
export function errorsAt(errors: FieldError[], field: string): string[] {
  return errors.filter((e) => e.field === field || e.field.startsWith(`${field}.`)).map((e) => e.message);
}

/** Errors no field of the page shows (a whole file's). */
export function generalErrors(errors: FieldError[], fields: string[]): FieldError[] {
  return errors.filter((e) => !fields.some((f) => e.field === f || e.field.startsWith(`${f}.`)));
}

/** Which agents run on each model id. */
export function usedBy(agents: Settings["agents"]): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const a of agents) {
    if (!a.effective_model) continue;
    out.set(a.effective_model, [...(out.get(a.effective_model) ?? []), a.name]);
  }
  return out;
}

const usd = (v: number) => (v >= 10 ? v.toFixed(0) : v >= 1 ? v.toFixed(2) : v.toPrecision(2));

/** "$0.05 in · $0.50 out /M", or null without prices. */
export function priceLabel(m: CatalogModel): string | null {
  if (m.price_in === null && m.price_out === null) return null;
  const part = (v: number | null, what: string) => (v === null ? `? ${what}` : `$${usd(v)} ${what}`);
  return `${part(m.price_in, "in")} · ${part(m.price_out, "out")} /M`;
}

/** "1M ctx", "128k ctx". */
export function contextLabel(tokens: number | null): string | null {
  if (!tokens) return null;
  return tokens >= 1_000_000 ? `${+(tokens / 1_048_576).toFixed(1) || 1}M ctx` : `${Math.round(tokens / 1000)}k ctx`;
}

export interface ModelOption {
  id: string;
  /** The catalog's entry, when OpenRouter lists it. */
  model?: CatalogModel;
  /** The typically used list's note. */
  note?: string;
}

/** Options for a search: the typical models first, then catalog matches, at most `limit`. */
export function searchModels(catalog: CatalogModel[], typical: ModelEntry[], term: string, limit = 40): ModelOption[] {
  const byId = new Map(catalog.map((m) => [m.id, m]));
  const q = term.trim().toLowerCase();
  const hit = (s: string) => q.split(/\s+/).every((w) => s.toLowerCase().includes(w));
  const out: ModelOption[] = typical
    .filter((t) => !q || hit(`${t.id} ${t.note}`))
    .map((t) => ({ id: t.id, model: byId.get(t.id), note: t.note }));
  const seen = new Set(out.map((o) => o.id));
  for (const m of catalog) {
    if (out.length >= limit) break;
    if (!seen.has(m.id) && (!q || hit(`${m.id} ${m.name}`))) out.push({ id: m.id, model: m });
  }
  return out;
}

/** A JSON Schema node, as schemars writes them. */
export interface Schema {
  type?: string | string[];
  title?: string;
  description?: string;
  enum?: unknown[];
  format?: string;
  default?: unknown;
  items?: Schema;
  required?: string[];
  "x-multiline"?: boolean;
  properties?: Record<string, Schema>;
  $ref?: string;
  anyOf?: Schema[];
  $defs?: Record<string, Schema>;
}

/** The node itself: `$ref`s resolved, `null` alternatives dropped; whether null is allowed. */
export function resolve(root: Schema, node: Schema): { node: Schema; nullable: boolean } {
  let nullable = false;
  let n = node;
  for (let i = 0; i < 8; i++) {
    if (n.$ref) {
      const def = root.$defs?.[n.$ref.replace("#/$defs/", "")] ?? {};
      n = { ...def, ...n, $ref: undefined, description: n.description ?? def.description };
    } else if (n.anyOf) {
      nullable ||= n.anyOf.some((a) => a.type === "null");
      const real = n.anyOf.find((a) => a.type !== "null") ?? {};
      n = { ...real, description: n.description ?? real.description };
    } else break;
  }
  if (Array.isArray(n.type)) {
    nullable ||= n.type.includes("null");
    n = { ...n, type: n.type.find((t) => t !== "null") };
  }
  return { node: n, nullable };
}

/** `rollover_tokens` → "Rollover tokens". */
export function humanize(key: string): string {
  const s = key.replace(/_/g, " ");
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** How a field is edited. */
export type FieldKind = "object" | "boolean" | "number" | "integer" | "choice" | "multi" | "file" | "files" | "date" | "date-time" | "time" | "multiline" | "string";

export function fieldKind(n: Schema): FieldKind {
  if (n.properties) return "object";
  const t = typeof n.type === "string" ? n.type : "string";
  if (t === "array") return n.items?.format === "file" ? "files" : "multi";
  if (t === "boolean" || t === "number" || t === "integer") return t;
  if (n.enum) return "choice";
  if (n.format === "file" || n.format === "date" || n.format === "date-time" || n.format === "time") return n.format;
  return n["x-multiline"] ? "multiline" : "string";
}

/** A form's starting values: each field's `default`. */
export function defaults(schema: Schema): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(schema.properties ?? {})
      .filter(([, f]) => f.default !== undefined)
      .map(([k, f]) => [k, JSON.parse(JSON.stringify(f.default))]),
  );
}

/** Required fields of a form that are still empty. */
export function missing(schema: Schema, values: Record<string, unknown>): string[] {
  const empty = (v: unknown) => v === undefined || v === null || v === "" || (Array.isArray(v) && v.length === 0);
  return (schema.required ?? []).filter((k) => empty(values[k]));
}
