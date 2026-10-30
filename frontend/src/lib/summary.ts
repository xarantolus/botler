// The deployment/change summary (botler-core change.rs `ChangeSummary::to_markdown`, plus the
// reviewer and gate lines the approval dialog appends) as typed sections, so the UI can show
// additions, removals and changes as distinct blocks and likely mistakes as warnings.

export type SectionKind =
  | "check"
  | "escalations"
  | "approval"
  | "start"
  | "stop"
  | "change"
  | "definitions"
  | "reviewer"
  | "gate"
  | "other";

export interface SummaryItem {
  text: string;
  /** Nested bullet lines (e.g. `next: …` fire times). */
  sub: string[];
  /** For "Will change" items: the trigger id and its before/after. */
  change?: { id: string; before: string; after: string };
  /** For "Definitions" items: added / removed / changed. */
  status?: "added" | "removed" | "changed";
}

export interface SummarySection {
  kind: SectionKind;
  title: string;
  /** Text on the heading line after the colon, e.g. `**Gate:** passed`. */
  inline: string;
  items: SummaryItem[];
  /** Non-bullet lines under the heading. */
  text: string;
}

export interface ParsedSummary {
  /** Text before the first heading (e.g. "No effective changes."). */
  preamble: string;
  sections: SummarySection[];
}

const KINDS: [RegExp, SectionKind][] = [
  [/^check this/i, "check"],
  [/^needs the user's approval/i, "escalations"],
  [/^approval$/i, "approval"],
  [/^will start happening$/i, "start"],
  [/^will stop happening$/i, "stop"],
  [/^will change$/i, "change"],
  [/^definitions$/i, "definitions"],
  [/^reviewer$/i, "reviewer"],
  [/^gate$/i, "gate"],
];

export function sectionKind(title: string): SectionKind {
  for (const [re, kind] of KINDS) if (re.test(title.trim())) return kind;
  return "other";
}

const HEADING = /^\*\*(.+?):\*\*\s*(.*)$/;
const CHANGE = /^`([^`]+)`: was "(.*)", becomes "(.*)"$/;
const DEFINITION = /^(added|removed|changed): (.*)$/;

export function parseSummary(markdown: string): ParsedSummary {
  const out: ParsedSummary = { preamble: "", sections: [] };
  let cur: SummarySection | null = null;
  const preamble: string[] = [];
  const text: string[] = [];
  const flushText = () => {
    if (cur) cur.text = text.join("\n").trim();
  };
  for (const line of markdown.split("\n")) {
    const h = HEADING.exec(line);
    if (h) {
      flushText();
      text.length = 0;
      const title = h[1]!;
      cur = { kind: sectionKind(title), title, inline: h[2]!.trim(), items: [], text: "" };
      out.sections.push(cur);
      continue;
    }
    if (!cur) {
      preamble.push(line);
      continue;
    }
    const sub = /^\s{2,}- (.*)$/.exec(line);
    const last = cur.items.at(-1);
    if (sub && last) {
      last.sub.push(sub[1]!);
      continue;
    }
    const item = /^- (.*)$/.exec(line);
    if (item) {
      const t = item[1]!;
      const it: SummaryItem = { text: t, sub: [] };
      if (cur.kind === "change") {
        const c = CHANGE.exec(t);
        if (c) it.change = { id: c[1]!, before: c[2]!, after: c[3]! };
      }
      if (cur.kind === "definitions") {
        const d = DEFINITION.exec(t);
        if (d) {
          it.status = d[1] as SummaryItem["status"];
          it.text = d[2]!;
        }
      }
      cur.items.push(it);
      continue;
    }
    text.push(line);
  }
  flushText();
  out.preamble = preamble.join("\n").trim();
  return out;
}

/** Whether the markdown looks like a change summary at all (else render it as plain markdown). */
export function looksLikeSummary(markdown: string): boolean {
  return parseSummary(markdown).sections.some((s) => s.kind !== "other");
}
