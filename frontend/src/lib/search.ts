// Search results: highlighting, grouping by source, and where a hit opens.

import type { SearchHit, SearchSource } from "../generated";

/** A snippet split into plain and matched parts (the hub marks matches as `**word**`). */
export function highlight(snippet: string): { text: string; mark: boolean }[] {
  return snippet
    .split("**")
    .map((text, i) => ({ text, mark: i % 2 === 1 }))
    .filter((p) => p.text !== "");
}

export const SOURCE_LABELS: Record<SearchSource, string> = {
  session: "Conversations",
  memory: "Memory",
  event: "Events",
  repo: "Monorepo",
};

/** Hits per source, in a fixed order, keeping the ranking within each source. */
export function groupHits(hits: SearchHit[]): { source: SearchSource; hits: SearchHit[] }[] {
  const order: SearchSource[] = ["session", "memory", "event", "repo"];
  return order
    .map((source) => ({ source, hits: hits.filter((h) => h.source === source) }))
    .filter((g) => g.hits.length);
}

/** Where a hit opens in the web app, if anywhere. */
export function hitLocation(hit: SearchHit): { path: string; query: Record<string, string> } | null {
  switch (hit.source) {
    case "session":
      return { path: `/chat/${encodeURIComponent(hit.owner)}`, query: { entry: hit.ref } };
    case "event":
      return { path: "/events", query: { event: hit.ref } };
    default:
      return null;
  }
}
