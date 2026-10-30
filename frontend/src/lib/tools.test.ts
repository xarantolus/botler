import { describe, expect, it } from "vitest";
import { argsSummary, argsText, resultText, toolIcon } from "./tools";

describe("toolIcon", () => {
  it("knows the common tools and falls back to a wrench", () => {
    expect(toolIcon("bash")).toBe("i-lucide-square-terminal");
    expect(toolIcon("ask_user")).toBe("i-lucide-message-circle-question");
    expect(toolIcon("memory_write")).toBe("i-lucide-brain");
    expect(toolIcon("frobnicate")).toBe("i-lucide-wrench");
  });
});

describe("argsSummary", () => {
  it("prefers the telling argument", () => {
    expect(argsSummary({ timeout: 5, command: "ls   -la\n/tmp" })).toBe("ls -la /tmp");
    expect(argsSummary({ path: "a.txt", content: "..." })).toBe("a.txt");
  });
  it("falls back to any string, then the argument names", () => {
    expect(argsSummary({ foo: 1, bar: "baz" })).toBe("baz");
    expect(argsSummary({ foo: 1, bar: 2 })).toBe("foo, bar");
    expect(argsSummary({})).toBe("");
    expect(argsSummary(null)).toBe("");
  });
  it("shortens long values", () => {
    expect(argsSummary({ command: "x".repeat(200) }, 10)).toBe(`${"x".repeat(9)}…`);
  });
});

describe("argsText and resultText", () => {
  it("shows code as itself and anything else as JSON", () => {
    expect(argsText({ code: "print(1)" })).toBe("print(1)");
    expect(argsText({ path: "a" })).toBe('{\n  "path": "a"\n}');
    expect(argsText({})).toBe("");
  });
  it("indents JSON results and keeps text", () => {
    expect(resultText('{"a":1}')).toBe('{\n  "a": 1\n}');
    expect(resultText("{not json")).toBe("{not json");
    expect(resultText("plain")).toBe("plain");
  });
});
