import { describe, expect, it } from "vitest";
import type { CatalogModel, Settings } from "../generated";
import { changes, contextLabel, defaults, draftOf, errorsAt, fieldKind, generalErrors, missing, priceLabel, resolve, searchModels, usedBy, type Schema } from "./settings";

function cat(id: string, extra: Partial<CatalogModel> = {}): CatalogModel {
  return {
    id,
    name: id,
    context_length: null,
    max_output: null,
    image: false,
    price_in: null,
    price_out: null,
    price_cache_read: null,
    price_cache_write: null,
    ...extra,
  };
}

const settings: Settings = {
  commit: "c1",
  models: { default: "a/x", models: [{ id: "a/x", note: "cheap" }] },
  routes: [],
  agents: [
    { name: "chat", description: "", edit: { model: null, tools: ["web.*"], mcp: [], idle_timeout: null, compact_at: null }, effective_model: "a/x", mcp_stdio: [] },
    { name: "dev", description: "", edit: { model: "b/y", tools: [], mcp: [], idle_timeout: "30m", compact_at: null }, effective_model: "b/y", mcp_stdio: [] },
  ],
  defaults: { idle_timeout: "15m" },
  defaults_schema: {},
  mirror: null,
  tools: [],
  services: [],
  warnings: [],
  runnables: [
    {
      job: "watch",
      description: "",
      lifetime: "service",
      enabled: true,
      schema: { properties: { every: { type: "integer", default: 5 }, folder: { type: "string" } } },
      values: { folder: "INBOX" },
    },
  ],
};

describe("changes", () => {
  it("sends only what differs", () => {
    const d = draftOf(settings);
    expect(changes(settings, d)).toBeNull();
    d.agents.dev!.idle_timeout = "1h";
    expect(changes(settings, d)).toEqual({ commit: "c1", agents: { dev: { model: "b/y", tools: [], mcp: [], idle_timeout: "1h", compact_at: null } } });
    d.mirror = " ssh://git@host:222/me/repo.git ";
    expect(changes(settings, d)?.deployment?.mirror).toBe("ssh://git@host:222/me/repo.git");
    expect(settings.agents[1]!.edit.idle_timeout).toBe("30m"); // the loaded copy is untouched
  });

  it("fills a job's settings from the defaults and sends all of them when one changes", () => {
    const d = draftOf(settings);
    expect(d.runnables.watch).toEqual({ every: 5, folder: "INBOX" });
    d.runnables.watch!.every = 10;
    expect(changes(settings, d)?.runnables).toEqual({ watch: { every: 10, folder: "INBOX" } });
  });
});

describe("errors", () => {
  const errors = [
    { field: "agents.chat.model", message: "unknown" },
    { field: "agents.chat", message: "file" },
    { field: "botler.toml", message: "whole" },
  ];
  it("are found by field prefix, and the rest is general", () => {
    expect(errorsAt(errors, "agents.chat")).toEqual(["unknown", "file"]);
    expect(errorsAt(errors, "agents.chat.model")).toEqual(["unknown"]);
    expect(generalErrors(errors, ["models", "agents"])).toEqual([errors[2]]);
  });
});

describe("models", () => {
  const catalog = [cat("a/x"), cat("b/y", { name: "Bee Why" }), cat("c/z", { name: "Sea" })];
  it("lists the typical ones first, then catalog matches", () => {
    expect(searchModels(catalog, settings.models.models, "").map((o) => o.id)).toEqual(["a/x", "b/y", "c/z"]);
    expect(searchModels(catalog, settings.models.models, "bee").map((o) => o.id)).toEqual(["b/y"]);
    expect(searchModels(catalog, settings.models.models, "cheap")[0]).toMatchObject({ id: "a/x", note: "cheap" });
    expect(searchModels(catalog, [], "", 2)).toHaveLength(2);
  });
  it("labels price and context", () => {
    expect(priceLabel(cat("p", { price_in: 0.05, price_out: 0.5 }))).toBe("$0.050 in · $0.50 out /M");
    expect(priceLabel(cat("p", { price_in: 3, price_out: 15 }))).toBe("$3.00 in · $15 out /M");
    expect(priceLabel(cat("p"))).toBeNull();
    expect(contextLabel(1_048_576)).toBe("1M ctx");
    expect(contextLabel(131_072)).toBe("131k ctx");
    expect(contextLabel(null)).toBeNull();
  });
  it("knows who uses which", () => {
    expect(usedBy(settings.agents).get("a/x")).toEqual(["chat"]);
  });
});

describe("schema", () => {
  const root = { $defs: { D: { type: "string", description: "a duration" } } };
  it("resolves refs and nullable alternatives", () => {
    expect(resolve(root, { $ref: "#/$defs/D" })).toEqual({ node: { type: "string", description: "a duration" }, nullable: false });
    expect(resolve(root, { anyOf: [{ $ref: "#/$defs/D" }, { type: "null" }] }).nullable).toBe(true);
    expect(resolve(root, { type: ["number", "null"] })).toEqual({ node: { type: "number" }, nullable: true });
  });
});

describe("forms", () => {
  const form: Schema = {
    required: ["folder", "days"],
    properties: {
      folder: { type: "string", enum: ["INBOX", "Archive"], default: "INBOX" },
      days: { type: "array", items: { type: "string", enum: ["mon", "tue"] } },
      scans: { type: "array", items: { type: "string", format: "file" } },
      when: { type: "string", format: "date" },
      notes: { type: "string", "x-multiline": true },
      every: { type: "integer" },
    },
  };

  it("picks an editor per field", () => {
    const kinds = Object.values(form.properties!).map(fieldKind);
    expect(kinds).toEqual(["choice", "multi", "files", "date", "multiline", "integer"]);
  });

  it("starts from the defaults and lists what is still required", () => {
    const v = defaults(form);
    expect(v).toEqual({ folder: "INBOX" });
    expect(missing(form, v)).toEqual(["days"]);
    expect(missing(form, { ...v, days: ["mon"] })).toEqual([]);
  });
});
