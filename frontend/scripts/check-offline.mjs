// The hub may run without internet, so the built app must never load anything from elsewhere:
// no web fonts, no icon API, no CDN. This fails on any URL in dist/ that is not one of the
// known inert strings (XML namespaces, error-message links, license comments).
//
//   node scripts/check-offline.mjs [dir]   (part of `npm run build` and `npm run check-dist`)

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const dist = process.argv[2] ?? fileURLToPath(new URL("../dist", import.meta.url));

/** URLs that appear in the bundle as plain strings and are never requested. */
const INERT = [
  /^http:\/\/www\.w3\.org\/(2000\/svg|1999\/xlink|1999\/xhtml|1998\/Math\/MathML|XML\/1998\/namespace|2000\/xmlns\/?)$/,
  /^https:\/\/vuejs\.org\/error-reference\/?(#[\w${}-]*)?$/, // Vue's error messages
  /^https:\/\/tailwindcss\.com\/?$/, // license comment
  /^https:\/\/github\.com\/markedjs\/marked\.?$/, // marked's error message
  /^https:\/\/github\.com\/highlightjs\/highlight\.js\/(issues\/2277|wiki\/security)$/, // highlight.js console warnings
];

function files(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) files(p, out);
    else if (/\.(js|css|html|svg|json|webmanifest)$/.test(name)) out.push(p);
  }
  return out;
}

const problems = [];
for (const f of files(dist)) {
  const text = readFileSync(f, "utf8");
  for (const m of text.matchAll(/(?:https?:)?\/\/[a-z0-9-]+(?:\.[a-z0-9-]+)+[^\s"'`)<>\\]*/gi)) {
    const url = m[0];
    // `//` inside code (comments, regexes) without a scheme only counts in CSS url() / @import.
    if (url.startsWith("//") && !/\.css$/.test(f)) continue;
    if (INERT.some((re) => re.test(url))) continue;
    problems.push(`${relative(dist, f)}: ${url}`);
  }
  if (/\.css$/.test(f) && /@import\s+(url\()?["']?(https?:)?\/\//.test(text)) problems.push(`${relative(dist, f)}: remote @import`);
}

if (problems.length) {
  console.error("frontend/dist references external URLs (the app must work offline):");
  for (const p of [...new Set(problems)]) console.error(`  ${p}`);
  console.error("Bundle the resource, or add the URL to INERT in scripts/check-offline.mjs if it is never requested.");
  process.exit(1);
}
console.log("frontend/dist references no external resources");
