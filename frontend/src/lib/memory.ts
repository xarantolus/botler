import type { MemoryEntry } from "../generated";

/** The namespace of a memory path: `user`, `agents/<definition>` or `projects/<home>`. */
export function namespaceOf(path: string): string {
  const parts = path.split("/");
  return parts[0] === "user" ? "user" : parts.slice(0, 2).join("/");
}

/** Files grouped by namespace, namespaces sorted with `user` first. */
export function byNamespace(files: MemoryEntry[]): { namespace: string; files: MemoryEntry[] }[] {
  const groups = new Map<string, MemoryEntry[]>();
  for (const f of files) {
    const ns = namespaceOf(f.path);
    groups.set(ns, [...(groups.get(ns) ?? []), f]);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => (a === "user" ? -1 : b === "user" ? 1 : a.localeCompare(b)))
    .map(([namespace, files]) => ({ namespace, files }));
}
