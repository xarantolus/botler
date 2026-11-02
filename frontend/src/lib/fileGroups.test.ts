import { describe, expect, it } from "vitest";
import type { FileRef } from "./attachments";
import { fileGroups } from "./fileGroups";
import type { TranscriptPart } from "./transcript";

const file = (path: string): FileRef => ({ path, name: path, mime: "image/png", size: 1 }) as FileRef;
const files = (...paths: string[]) => ({ type: "data-files", data: { files: paths.map(file) } }) as TranscriptPart;
const text = { type: "text", text: "so" } as TranscriptPart;
const tool = { type: "dynamic-tool", toolName: "shot", toolCallId: "t", state: "output-available", input: {}, output: "" } as TranscriptPart;

describe("fileGroups", () => {
  it("shows a run's files together at the first, until a text", () => {
    const g = fileGroups([tool, files("a"), tool, files("b", "c"), text, tool, files("d")]);
    expect(g.get(1)?.map((f) => f.path)).toEqual(["a", "b", "c"]);
    expect(g.get(3)).toBeNull();
    expect(g.get(6)?.map((f) => f.path)).toEqual(["d"]);
  });
});
