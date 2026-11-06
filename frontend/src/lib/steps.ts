// How an agent turn folds: each run of thinking and tool calls the agent has moved on from (a
// remark of its follows) becomes one line, "Used 3 tools, edited 2 files". What it says between
// them stays in the flow, and so do cards (requests, files, errors). The run it is still in
// stays open, so its tools show as they run.

import type { TranscriptPart } from "./transcript";

const STEPS = new Set(["reasoning", "dynamic-tool"]);
const EDITS = /^(write|edit|write_file|edit_file|patch|apply_patch)$/;

export interface Fold {
  /** The run's parts: `start` up to, not including, `end`. */
  start: number;
  end: number;
  summary: string;
}

const plural = (n: number, one: string, many = `${one}s`) =>
  `${n} ${n === 1 ? one : many}`;

function summary(steps: TranscriptPart[]): string {
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
  const s = said.length ? said.join(", ") : "thought it over";
  return s[0]!.toUpperCase() + s.slice(1);
}

/** The folds of a turn's parts: its runs of tool calls (with their thinking) that text follows.
 * A run of thinking alone is not folded: it is one line already. */
export function foldsOf(parts: TranscriptPart[]): Fold[] {
  const folds: Fold[] = [];
  let start = -1;
  parts.forEach((p, i) => {
    if (STEPS.has(p.type)) {
      if (start < 0) start = i;
      return;
    }
    const run = start < 0 ? [] : parts.slice(start, i);
    if (p.type === "text" && run.some((r) => r.type === "dynamic-tool")) {
      folds.push({ start, end: i, summary: summary(run) });
    }
    // Only text ends a run: a card between two tools does not split it.
    if (p.type === "text") start = -1;
  });
  return folds;
}

/** The fold part `i` is in (cards inside a run are not). */
export function foldAt(
  parts: TranscriptPart[],
  folds: Fold[],
  i: number,
): Fold | undefined {
  if (!STEPS.has(parts[i]!.type)) return undefined;
  return folds.find((f) => f.start <= i && i < f.end);
}
