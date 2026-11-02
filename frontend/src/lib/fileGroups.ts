// The files of a turn shown together: every `data-files` part between two texts (the images and
// files of a run of tool calls) is one gallery, shown where the first of them is.

import type { FileRef } from "./attachments";
import type { TranscriptPart } from "./transcript";

/** By part index: the files that part shows (its run's, at the first), or `null` (shown earlier). */
export function fileGroups(parts: TranscriptPart[]): Map<number, FileRef[] | null> {
  const out = new Map<number, FileRef[] | null>();
  let first: FileRef[] | null = null;
  parts.forEach((p, i) => {
    if (p.type === "text") first = null;
    else if (p.type === "data-files" && first) {
      first.push(...p.data.files);
      out.set(i, null);
    } else if (p.type === "data-files") out.set(i, (first = [...p.data.files]));
  });
  return out;
}
