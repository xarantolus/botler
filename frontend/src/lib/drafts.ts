// What was typed into a chat's composer (and the files already uploaded for it), kept in this
// browser so a closed tab or a reload does not lose it. Cleared when the message is sent.

export interface DraftFile {
  name: string;
  size: number;
  /** The upload (`<id>/<name>`). */
  file: string;
}
export interface Draft {
  text: string;
  files: DraftFile[];
}

const key = (instance: string) => `botler.draft.${instance}`;

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function loadDraft(instance: string): Draft {
  try {
    const d = JSON.parse(storage()?.getItem(key(instance)) ?? "null") as Partial<Draft> | null;
    return { text: typeof d?.text === "string" ? d.text : "", files: Array.isArray(d?.files) ? d.files : [] };
  } catch {
    return { text: "", files: [] };
  }
}

export function saveDraft(instance: string, d: Draft): void {
  try {
    if (!d.text && !d.files.length) storage()?.removeItem(key(instance));
    else storage()?.setItem(key(instance), JSON.stringify(d));
  } catch {
    // Storage full or disabled: the draft lives as long as the tab.
  }
}
