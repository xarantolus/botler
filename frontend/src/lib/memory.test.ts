import { describe, expect, it } from "vitest";
import { byNamespace, namespaceOf } from "./memory";

describe("memory", () => {
  it("finds namespaces", () => {
    expect(namespaceOf("user/a/b.md")).toBe("user");
    expect(namespaceOf("agents/dev/x/y.md")).toBe("agents/dev");
    expect(namespaceOf("projects/gs/INDEX.md")).toBe("projects/gs");
  });

  it("groups files with user first", () => {
    const f = (path: string) => ({ path, bytes: 1 });
    const groups = byNamespace([f("agents/dev/a.md"), f("user/p.md"), f("agents/dev/b.md"), f("agents/botler/c.md")]);
    expect(groups.map((g) => g.namespace)).toEqual(["user", "agents/botler", "agents/dev"]);
    expect(groups[2]!.files.map((x) => x.path)).toEqual(["agents/dev/a.md", "agents/dev/b.md"]);
  });
});
