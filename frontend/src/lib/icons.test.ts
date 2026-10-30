// Every icon the sources name must exist in the bundled Lucide set: the app never fetches icons
// (see iconify-offline.ts), so a misspelt one would silently render as nothing.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const src = fileURLToPath(new URL("..", import.meta.url));
const lucide = JSON.parse(
  readFileSync(fileURLToPath(new URL("../../node_modules/@iconify-json/lucide/icons.json", import.meta.url)), "utf8"),
) as { icons: Record<string, unknown>; aliases?: Record<string, unknown> };

function sources(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) sources(p, out);
    else if (/\.(vue|ts)$/.test(name) && !name.endsWith(".test.ts")) out.push(p);
  }
  return out;
}

describe("icons", () => {
  it("are all in the bundled Lucide collection", () => {
    const used = new Set<string>();
    for (const f of sources(src)) for (const m of readFileSync(f, "utf8").matchAll(/i-lucide-([a-z0-9-]+)/g)) used.add(m[1]!);
    expect(used.size).toBeGreaterThan(10);
    const missing = [...used].filter((n) => !(n in lucide.icons) && !(n in (lucide.aliases ?? {})));
    expect(missing).toEqual([]);
  });
});
