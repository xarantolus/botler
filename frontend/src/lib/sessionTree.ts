// A session's entry tree (`session_tree`) shaped for reading: a conversation is a long chain of
// entries, so a chain stays one flat list and only a branch point nests. At a branch point the
// continuation on the active branch (else the first) goes on in the same list; every other
// continuation becomes a "branch" row whose children are that branch's own list. Bookkeeping
// entries (labels, navigation markers) are left out; their children hang on their parent.

import type { SessionTree, TreeNode } from "../generated";

export interface TreeRow {
  /** Entry id, or `branch:<first entry id>` for a branch row. */
  id: string;
  /** The entry (absent for a branch row). */
  node: TreeNode | null;
  label: string;
  /** On the path the agent currently sees. */
  active: boolean;
  /** The entry the conversation continues from. */
  leaf: boolean;
  /** A branch row: the other continuations at this point (its own list). */
  children?: TreeRow[];
  /** For a branch row: how many entries it holds. */
  size?: number;
}

const HIDDEN = new Set(["label", "botler.nav", "session", "model_change", "thinking_level_change"]);

const KIND: Record<string, string> = {
  user: "You",
  assistant: "Agent",
  tool_result: "Tool result",
  note: "Note",
  compaction: "Compaction",
  session_started: "Session started",
};

export function kindLabel(kind: string): string {
  return KIND[kind] ?? kind;
}

function rowLabel(n: TreeNode): string {
  const text = n.text.replace(/\s+/g, " ").trim();
  return `${kindLabel(n.kind)}: ${text || "(empty)"}`;
}

/** Only assistant entries can be forked into a new chat. */
export const forkable = (n: TreeNode) => n.kind === "assistant";

export function shapeTree(tree: SessionTree): TreeRow[] {
  const byId = new Map(tree.nodes.map((n) => [n.id, n]));
  /** The nearest visible ancestor (null at the root). */
  const visibleParent = (n: TreeNode): string | null => {
    let p = n.parent;
    while (p !== null) {
      const pn = byId.get(p);
      if (!pn) return null;
      if (!HIDDEN.has(pn.kind)) return p;
      p = pn.parent;
    }
    return null;
  };
  const visible = tree.nodes.filter((n) => !HIDDEN.has(n.kind));
  const kids = new Map<string | null, TreeNode[]>();
  for (const n of visible) {
    const p = visibleParent(n);
    kids.set(p, [...(kids.get(p) ?? []), n]);
  }
  // The leaf may be a hidden marker: the visible entry it continues from is the leaf shown.
  let leaf = tree.leaf;
  while (leaf !== null && HIDDEN.has(byId.get(leaf)?.kind ?? "")) leaf = byId.get(leaf)?.parent ?? null;

  const row = (n: TreeNode): TreeRow => ({ id: n.id, node: n, label: rowLabel(n), active: n.on_branch, leaf: n.id === leaf });
  const count = (rows: TreeRow[]): number => rows.reduce((s, r) => s + (r.node ? 1 : 0) + (r.children ? count(r.children) : 0), 0);

  function run(start: TreeNode): TreeRow[] {
    const out: TreeRow[] = [];
    let cur: TreeNode | undefined = start;
    while (cur) {
      out.push(row(cur));
      const next: TreeNode[] = kids.get(cur.id) ?? [];
      if (next.length > 1) {
        const main = next.find((k) => k.on_branch) ?? next[0]!;
        for (const alt of next.filter((k) => k !== main)) {
          const children = run(alt);
          out.push({
            id: `branch:${alt.id}`,
            node: null,
            label: `Branch: ${rowLabel(alt)}`,
            active: false,
            leaf: false,
            children,
            size: count(children),
          });
        }
        cur = main;
      } else {
        cur = next[0];
      }
    }
    return out;
  }

  return (kids.get(null) ?? []).flatMap(run);
}
