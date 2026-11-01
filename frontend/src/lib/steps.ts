// How an agent turn folds once it answers: its thinking, tool calls and the remarks between them
// (everything before its last text) become one line, "Used 3 tools, edited 2 files". Cards
// (requests, files, errors) are never folded away.

import type { TranscriptPart } from "./transcript";

const FOLDS = new Set(["reasoning", "dynamic-tool", "text"]);
const EDITS = /^(write|edit|write_file|edit_file|patch|apply_patch)$/;

export interface Fold {
  /** Foldable parts before this index are folded. */
  until: number;
  summary: string;
}

const plural = (n: number, one: string, many = `${one}s`) =>
  `${n} ${n === 1 ? one : many}`;

/** The fold of a turn's parts, if it has steps before its last text. */
export function foldOf(parts: TranscriptPart[]): Fold | null {
  const until = parts.findLastIndex((p) => p.type === "text");
  const steps = parts
    .slice(0, Math.max(0, until))
    .filter((p) => FOLDS.has(p.type));
  if (!steps.some((p) => p.type !== "text")) return null;
  const files = new Set<string>();
  let tools = 0;
  for (const p of steps) {
    if (p.type !== "dynamic-tool") continue;
    const input = (p.input ?? {}) as Record<string, unknown>;
    const path = input.path ?? input.file_path ?? input.file;
    if (EDITS.test(p.toolName) && typeof path === "string") files.add(path);
    else tools += 1;
  }
  const said = [
    tools && `used ${plural(tools, "tool")}`,
    files.size && `edited ${plural(files.size, "file")}`,
  ].filter(Boolean);
  const summary = said.length ? said.join(", ") : "thought it over";
  return { until, summary: summary[0]!.toUpperCase() + summary.slice(1) };
}

/** Whether part `i` is hidden while `fold` is closed. */
export function folded(
  parts: TranscriptPart[],
  fold: Fold | null,
  i: number,
): boolean {
  return !!fold && i < fold.until && FOLDS.has(parts[i]!.type);
}
