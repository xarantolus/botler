// A proposal under review: its diff shaped into rows, and its pipeline as steps.

import type { FileStatus, Hunk, Proposal, ProposalStage } from "../generated";
import type { Color } from "./runs";
import { elapsed } from "./time";

export type RowKind = "hunk" | "context" | "add" | "del" | "note";

/** One line of a unified diff, with its line numbers in the old and new file. */
export interface DiffRow {
  kind: RowKind;
  text: string;
  old?: number;
  new?: number;
}

/** A side-by-side row: removed lines on the left, added on the right (null = blank). */
export interface SplitRow {
  kind: "hunk" | "line";
  text?: string;
  left: DiffRow | null;
  right: DiffRow | null;
}

const PREFIX: Record<string, RowKind> = { " ": "context", "+": "add", "-": "del", "\\": "note" };

export function unifiedRows(hunks: Hunk[]): DiffRow[] {
  const rows: DiffRow[] = [];
  for (const h of hunks) {
    rows.push({ kind: "hunk", text: h.header });
    let o = h.old_start;
    let n = h.new_start;
    for (const line of h.lines) {
      const kind = PREFIX[line[0] ?? " "] ?? "context";
      const text = line.slice(1);
      if (kind === "context") rows.push({ kind, text, old: o++, new: n++ });
      else if (kind === "del") rows.push({ kind, text, old: o++ });
      else if (kind === "add") rows.push({ kind, text, new: n++ });
      else rows.push({ kind, text: text.trim() });
    }
  }
  return rows;
}

/** Pair each run of removed lines with the added lines that follow it. */
export function splitRows(rows: DiffRow[]): SplitRow[] {
  const out: SplitRow[] = [];
  let dels: DiffRow[] = [];
  let adds: DiffRow[] = [];
  const flush = () => {
    for (let i = 0; i < Math.max(dels.length, adds.length); i++) {
      out.push({ kind: "line", left: dels[i] ?? null, right: adds[i] ?? null });
    }
    dels = [];
    adds = [];
  };
  for (const r of rows) {
    if (r.kind === "del") {
      if (adds.length) flush();
      dels.push(r);
    } else if (r.kind === "add") adds.push(r);
    else {
      flush();
      if (r.kind === "hunk") out.push({ kind: "hunk", text: r.text, left: null, right: null });
      else out.push({ kind: "line", left: r, right: r });
    }
  }
  flush();
  return out;
}

export const FILE_STATUS: Record<FileStatus, { label: string; icon: string; text: string }> = {
  added: { label: "added", icon: "i-lucide-file-plus", text: "text-success" },
  modified: { label: "modified", icon: "i-lucide-file-diff", text: "text-warning" },
  deleted: { label: "deleted", icon: "i-lucide-file-minus", text: "text-error" },
  renamed: { label: "renamed", icon: "i-lucide-file-symlink", text: "text-info" },
};

export function inFlight(p: Pick<Proposal, "status">): boolean {
  return p.status === "gating" || p.status === "reviewing" || p.status === "awaiting_user";
}

/** Whether the user can still reject it (until the merge starts). */
export function rejectable(p: Pick<Proposal, "status" | "stage">): boolean {
  return inFlight(p) && p.stage !== "merge";
}

const STAGE: Record<ProposalStage, string> = {
  rebase: "rebasing",
  image: "building",
  gate: "checking",
  review: "in review",
  approval: "Review",
  merge: "merging",
};

/** A short word for where an in-flight proposal is ("Review" when it waits for the user). */
export function stageLabel(p: Pick<Proposal, "status" | "stage">): string {
  return p.status === "awaiting_user" ? "Review" : p.stage ? STAGE[p.stage] : "proposal";
}

const ORDER: ProposalStage[] = ["rebase", "image", "gate", "review", "approval", "merge"];
const at = (p: Pick<Proposal, "stage">) => (p.stage ? ORDER.indexOf(p.stage) : 0);

/** The gate's verdict, once known. */
export function gateVerdict(
  p: Pick<Proposal, "status" | "stage" | "gate_report">,
): { label: string; color: Color } | null {
  const gateAt = ORDER.indexOf("gate");
  if (inFlight(p) && at(p) <= gateAt) return at(p) >= ORDER.indexOf("image") ? { label: "running", color: "info" } : null;
  if (p.status === "failed" && p.stage === "gate" && p.gate_report !== null) return { label: "failed", color: "error" };
  if (at(p) > gateAt || p.status === "merged") return { label: "passed", color: "success" };
  return null;
}

export interface Step {
  title: string;
  description: string;
  icon: string;
}

/** The pipeline as steps, and which one it is at (`steps.length` once merged). */
export function pipeline(p: Proposal, now = Date.now()): { steps: Step[]; current: number } {
  const since = p.stage_at ? ` · ${elapsed(now - new Date(p.stage_at).getTime())}` : "";
  const live = inFlight(p);
  const idx = [0, 1, 1, 2, 3, 4][at(p)] ?? 0;
  const byUser = p.decided_by === "user";
  const gate = gateVerdict(p);
  const gateText =
    live && p.stage === "image"
      ? `building the image${since}`
      : live && p.stage === "gate"
        ? `running checks${since}`
        : (gate?.label ?? (!live && idx === 1 && p.status === "rejected" ? "stopped" : ""));
  const approval = p.status === "awaiting_user"
    ? "waiting for you"
    : byUser
      ? p.status === "rejected" ? "rejected by you" : "approved by you"
      : p.status === "merged" ? "not needed" : "";
  const steps: Step[] = [
    { title: "Rebase", description: live && idx === 0 ? `onto main${since}` : "onto main", icon: "i-lucide-git-branch" },
    { title: "Gate", description: gateText, icon: "i-lucide-shield-check" },
    {
      title: "Review",
      description: p.verdict
        ? p.verdict
        : live && idx === 2
          ? `${p.reviewer ?? "reviewer"} is reviewing${since}`
          : idx > 2 && !p.reviewer
            ? "no reviewer"
            : "",
      icon: "i-lucide-user-check",
    },
    { title: "Approval", description: approval, icon: "i-lucide-hand" },
    {
      title: "Merge",
      description: p.merged_commit ? p.merged_commit.slice(0, 10) : "",
      icon: "i-lucide-git-merge",
    },
  ];
  return { steps, current: p.status === "merged" ? steps.length : idx };
}
