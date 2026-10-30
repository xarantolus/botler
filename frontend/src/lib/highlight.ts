// Syntax highlighting for code anywhere in the app (Markdown fences, tool arguments and results,
// JSON views, diffs): highlight.js with only the common languages registered. A given language is
// used as is; otherwise it is detected (diffs and JSON by shape, the rest by highlight.js).
// The output is sanitized HTML (highlight.js escapes the code; DOMPurify on top, to be safe).

import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import diff from "highlight.js/lib/languages/diff";
import ini from "highlight.js/lib/languages/ini";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

for (const [name, lang] of Object.entries({ bash, diff, ini, javascript, json, python, rust, typescript, xml, yaml })) {
  hljs.registerLanguage(name, lang);
}

/** Languages auto-detection chooses from (diff and JSON are recognised by shape first). */
const AUTO = ["bash", "python", "ini", "yaml", "rust", "typescript", "xml"];
/** Below this highlight.js relevance a guess is prose, left plain. */
const MIN_RELEVANCE = 4;
/** Longer text (big logs, results) stays plain: highlighting it would stall the page. */
const MAX_CHARS = 100_000;
/** Detection looks at the start only. */
const SAMPLE_CHARS = 4_000;

function looksLikeDiff(code: string): boolean {
  if (/^diff --git /m.test(code)) return true;
  if (!/^@@ -\d+(,\d+)? \+\d+(,\d+)? @@/m.test(code)) return false;
  return code.split("\n").every((l) => l === "" || /^([-+ @\\]|diff |index )/.test(l));
}

function looksLikeJson(code: string): boolean {
  const t = code.trim();
  if (!/^[[{]/.test(t)) return false;
  try {
    JSON.parse(t);
    return true;
  } catch {
    // Not JSON: left to the detector.
    return false;
  }
}

/** Shell commands highlight.js scores too low on their own (`ls -la`, `$ cargo test`). */
const SHELL = /^(\$ |#!\/.*\b(ba|z)?sh\b|(sudo|cd|ls|cat|echo|grep|rg|git|npm|npx|cargo|uv|pip|python3?|curl|podman|docker|make|export|mkdir|rm|cp|mv) )/;

/** The language of `code`, or null for plain text. */
export function detectLanguage(code: string): string | null {
  if (looksLikeDiff(code)) return "diff";
  if (looksLikeJson(code)) return "json";
  if (SHELL.test(code.trimStart())) return "bash";
  const guess = hljs.highlightAuto(code.slice(0, SAMPLE_CHARS), AUTO);
  return guess.language && guess.relevance >= MIN_RELEVANCE ? guess.language : null;
}

function escape(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** `code` as HTML with highlight.js classes; `lang` (a fence's info) when known and registered. */
export function highlight(code: string, lang?: string | null): { html: string; language: string | null } {
  if (code.length > MAX_CHARS) return { html: escape(code), language: null };
  const given = lang?.trim().split(/\s/)[0]?.toLowerCase();
  const language = given && hljs.getLanguage(given) ? given : detectLanguage(code);
  const html = language ? hljs.highlight(code, { language, ignoreIllegals: true }).value : escape(code);
  return { html: DOMPurify.sanitize(html), language };
}
