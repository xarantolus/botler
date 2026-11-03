// Records which sources `dist` was built from, so a stale `dist` is rebuilt (the hub's build.rs
// runs `check` and builds on failure).
//
//   node scripts/source-hash.mjs write   after `vite build` (part of `npm run build`)
//   node scripts/source-hash.mjs check   exits 1 if dist/.source-hash does not match
//
// The hub checks the same hash in `cargo test` (crates/botler-hub/src/web.rs); keep the file
// selection and the hashing identical in both places.

import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const TOP = ["index.html", "package.json", "package-lock.json", "vite.config.ts", "tsconfig.json"];

function walk(dir, out) {
  if (!existsSync(dir)) return;
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (!p.endsWith(".test.ts")) out.push(p);
  }
}

export function sourceHash() {
  const files = TOP.map((f) => join(root, f));
  walk(join(root, "src"), files);
  walk(join(root, "public"), files);
  const rel = files.map((p) => [relative(root, p).split(sep).join("/"), p]);
  // Byte-wise order, as Rust sorts strings.
  rel.sort(([a], [b]) => (Buffer.compare(Buffer.from(a), Buffer.from(b))));
  const h = createHash("sha256");
  for (const [r, p] of rel) {
    h.update(r);
    h.update(Buffer.from([0]));
    h.update(readFileSync(p));
    h.update(Buffer.from([0]));
  }
  return h.digest("hex");
}

const target = join(root, "dist", ".source-hash");
const mode = process.argv[2];
if (mode === "write") {
  writeFileSync(target, `${sourceHash()}\n`);
} else if (mode === "check") {
  const want = sourceHash();
  const have = existsSync(target) ? readFileSync(target, "utf8").trim() : "(none)";
  if (want !== have) {
    console.error(`frontend/dist is stale (built from ${have}, sources are ${want}). Run \`npm run build\`.`);
    process.exit(1);
  }
  console.log("frontend/dist is current");
} else {
  console.error("usage: source-hash.mjs write|check");
  process.exit(2);
}
