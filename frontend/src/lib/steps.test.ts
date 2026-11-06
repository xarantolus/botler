import { describe, expect, it } from "vitest";
import { foldAt, foldsOf } from "./steps";
import type { TranscriptPart } from "./transcript";

const text = (t: string): TranscriptPart =>
  ({ type: "text", text: t }) as TranscriptPart;
const think: TranscriptPart = {
  type: "reasoning",
  text: "hm",
} as TranscriptPart;
const tool = (toolName: string, input: unknown = {}): TranscriptPart =>
  ({
    type: "dynamic-tool",
    toolName,
    toolCallId: toolName + JSON.stringify(input),
    input,
    state: "output-available",
    output: "ok",
  }) as TranscriptPart;
const card: TranscriptPart = {
  type: "data-error",
  data: { text: "x" },
} as TranscriptPart;

const hidden = (parts: TranscriptPart[]) => {
  const folds = foldsOf(parts);
  return parts.map((_, i) => !!foldAt(parts, folds, i));
};

describe("foldsOf", () => {
  it("folds each run of tool calls, keeping the remarks between them and cards", () => {
    const parts = [
      think,
      tool("bash"),
      text("checking"),
      tool("edit", { path: "a" }),
      tool("edit", { path: "a" }),
      card,
      tool("write", { path: "b" }),
      text("Done."),
    ];
    expect(foldsOf(parts).map((f) => f.summary)).toEqual(["Used 1 tool", "Edited 2 files"]);
    expect(hidden(parts)).toEqual([true, true, false, true, true, false, true, false]);
  });

  it("leaves text, thinking alone, and the run still going alone", () => {
    expect(foldsOf([text("hi")])).toEqual([]);
    expect(foldsOf([think, text("hi")])).toEqual([]);
    expect(foldsOf([think, tool("bash")])).toEqual([]);
    const parts = [tool("read"), text("now testing"), tool("bash")];
    expect(hidden(parts)).toEqual([true, false, false]);
  });

  it("folds all but the latest tool calls of the run still going", () => {
    const parts = [text("on it"), think, tool("a"), tool("b"), tool("c"), tool("d"), tool("e")];
    expect(foldsOf(parts).map((f) => f.summary)).toEqual(["Used 2 tools so far"]);
    expect(hidden(parts)).toEqual([false, true, true, true, false, false, false]);
    expect(foldsOf(parts.slice(0, 6))).toEqual([]);
  });
});
