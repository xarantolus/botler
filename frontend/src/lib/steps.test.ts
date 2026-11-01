import { describe, expect, it } from "vitest";
import { folded, foldOf } from "./steps";
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

describe("foldOf", () => {
  it("folds the steps before the last text into one line", () => {
    const parts = [
      think,
      tool("bash"),
      text("checking"),
      tool("edit", { path: "a" }),
      tool("edit", { path: "a" }),
      tool("write", { path: "b" }),
      card,
      text("Done."),
    ];
    const f = foldOf(parts)!;
    expect(f.summary).toBe("Used 1 tool, edited 2 files");
    expect(parts.map((_, i) => folded(parts, f, i))).toEqual([
      true,
      true,
      true,
      true,
      true,
      true,
      false,
      false,
    ]);
  });

  it("leaves a turn alone without steps, or still without text", () => {
    expect(foldOf([text("hi")])).toBeNull();
    expect(foldOf([text("a"), text("b")])).toBeNull();
    expect(foldOf([think, tool("bash")])).toBeNull();
    expect(foldOf([think, text("hi")])!.summary).toBe("Thought it over");
  });

  it("keeps what runs after the latest remark visible", () => {
    const parts = [tool("read"), text("now testing"), tool("bash")];
    const f = foldOf(parts)!;
    expect(parts.map((_, i) => folded(parts, f, i))).toEqual([
      true,
      false,
      false,
    ]);
  });
});
