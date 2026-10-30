// How a tool call reads in the transcript: an icon, a one-line summary of its arguments, and its
// arguments and result as text.

const ICONS: [RegExp, string][] = [
  [/^(bash|shell|exec|run|sh)$/, "i-lucide-square-terminal"],
  [/^(read|cat|view|read_file)$/, "i-lucide-file-text"],
  [/^(write|edit|write_file|edit_file|patch|apply_patch)$/, "i-lucide-file-pen"],
  [/^(ls|find|glob|list|grep|rg)$/, "i-lucide-folder-search"],
  [/search/, "i-lucide-search"],
  [/^(fetch|web|http|browse|curl)/, "i-lucide-globe"],
  [/^ask_user$/, "i-lucide-message-circle-question"],
  [/^request_secrets$/, "i-lucide-key-round"],
  [/^propose_change$/, "i-lucide-git-pull-request-arrow"],
  [/^(send_message|message|notify|post_event|emit)/, "i-lucide-send"],
  [/^(spawn|new_chat|handoff|hand_off)/, "i-lucide-split"],
  [/^memory/, "i-lucide-brain"],
  [/^(timer|schedule|sleep|wait)/, "i-lucide-alarm-clock"],
  [/^invoke_action$/, "i-lucide-zap"],
  [/^archive/, "i-lucide-archive"],
];

export function toolIcon(name: string): string {
  return ICONS.find(([re]) => re.test(name))?.[1] ?? "i-lucide-wrench";
}

/** Argument names that usually say what a call does, most telling first. */
const TELLING = ["command", "cmd", "code", "path", "file", "query", "q", "url", "question", "title", "to", "name", "text"];

function oneLine(s: string, max: number): string {
  const line = s.trim().replace(/\s+/g, " ");
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** A short summary of a call's arguments, e.g. the command of `bash`. */
export function argsSummary(input: unknown, max = 80): string {
  if (input === null || input === undefined) return "";
  if (typeof input === "string") return oneLine(input, max);
  if (typeof input !== "object" || Array.isArray(input)) return oneLine(JSON.stringify(input), max);
  const args = input as Record<string, unknown>;
  for (const k of TELLING) {
    const v = args[k];
    if (typeof v === "string" && v.trim()) return oneLine(v, max);
  }
  const first = Object.values(args).find((v) => typeof v === "string" && v.trim());
  if (typeof first === "string") return oneLine(first, max);
  const keys = Object.keys(args);
  return keys.length ? oneLine(keys.join(", "), max) : "";
}

/** A call's arguments for display: code as itself, anything else as indented JSON. */
export function argsText(input: unknown): string {
  if (input === null || input === undefined) return "";
  if (typeof input === "string") return input;
  if (typeof input === "object" && !Array.isArray(input)) {
    const args = input as Record<string, unknown>;
    const keys = Object.keys(args);
    if (keys.length === 1 && typeof args[keys[0]!] === "string" && /^(code|command|cmd)$/.test(keys[0]!)) {
      return args[keys[0]!] as string;
    }
    if (keys.length === 0) return "";
  }
  return JSON.stringify(input, null, 2);
}

/** A tool result for display: JSON indented, anything else as is. */
export function resultText(output: unknown): string {
  if (output === null || output === undefined) return "";
  if (typeof output !== "string") return JSON.stringify(output, null, 2);
  const t = output.trim();
  if (t.startsWith("{") || t.startsWith("[")) {
    try {
      return JSON.stringify(JSON.parse(t), null, 2);
    } catch {
      // Not JSON after all: shown as it is.
      return output;
    }
  }
  return output;
}
