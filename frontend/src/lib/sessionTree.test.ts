import { describe, expect, it } from "vitest";
import type { TreeNode } from "../generated";
import { forkable, shapeTree, type TreeRow } from "./sessionTree";

const n = (id: string, parent: string | null, kind: string, text = id, extra: Partial<TreeNode> = {}): TreeNode => ({
  id,
  parent,
  kind,
  text,
  label: null,
  at: null,
  on_branch: false,
  ...extra,
});

/** Rows as nested ids, branch rows as `[id, children]`. */
const ids = (rows: TreeRow[]): unknown[] => rows.map((r) => (r.children ? [r.id, ids(r.children)] : r.id));

describe("shapeTree", () => {
  it("keeps a chain flat and nests only the other branches, the active one continuing", () => {
    // u1 → a1 → u2 → a2 (old branch) and a1 → u3 → a3 (rewound to a1, the active branch).
    const on = { on_branch: true };
    const tree = {
      chat: "c",
      file: "c.jsonl",
      leaf: "nav",
      nodes: [
        n("u1", null, "user", "hi", on),
        n("a1", "u1", "assistant", "hello", on),
        n("u2", "a1", "user", "first idea"),
        n("a2", "u2", "assistant", "done that"),
        n("u3", "a1", "user", "second idea", on),
        n("a3", "u3", "assistant", "better", on),
        n("lab", "a3", "label"),
        n("nav", "lab", "botler.nav", "", on),
      ],
    };
    const rows = shapeTree(tree);
    expect(ids(rows)).toEqual(["u1", "a1", ["branch:u2", ["u2", "a2"]], "u3", "a3"]);
    const branch = rows[2]!;
    expect([branch.label, branch.size, branch.active]).toEqual(["Branch: You: first idea", 2, false]);
    // The leaf behind the bookkeeping entries is the last visible one.
    expect(rows.filter((r) => r.leaf).map((r) => r.id)).toEqual(["a3"]);
    expect(rows.filter((r) => r.active).map((r) => r.id)).toEqual(["u1", "a1", "u3", "a3"]);
  });

  it("nests branches within branches and shows labels' entries normally", () => {
    const tree = {
      chat: "c",
      file: "c.jsonl",
      leaf: "b",
      nodes: [n("r", null, "user"), n("a", "r", "assistant"), n("b", "r", "assistant"), n("c", "a", "user"), n("d", "a", "user")],
    };
    // No entry is on the active branch here: the first continuation goes on.
    expect(ids(shapeTree(tree))).toEqual(["r", ["branch:b", ["b"]], "a", ["branch:d", ["d"]], "c"]);
  });

  it("forks only assistant entries", () => {
    expect([forkable(n("x", null, "assistant")), forkable(n("y", null, "user"))]).toEqual([true, false]);
  });
});
