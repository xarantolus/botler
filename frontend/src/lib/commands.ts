// Slash commands typed into a chat's composer: run by the app, never sent to the agent.

export interface Command {
  name: string;
  description: string;
}

export const COMMANDS: Command[] = [
  { name: "compact", description: "Summarize the older conversation now and go on from the summary" },
  { name: "new", description: "Fresh session: the agent hands over a report, then starts from it" },
  { name: "clear", description: "Fresh session with nothing carried over" },
];

/** The command `text` is (`/compact`), if it is exactly one. */
export function commandOf(text: string): Command | null {
  const m = /^\/(\w+)\s*$/.exec(text.trim());
  return (m && COMMANDS.find((c) => c.name === m[1]!.toLowerCase())) ?? null;
}

/** The commands a draft that starts one (`/co`) may complete to. */
export function suggestions(text: string): Command[] {
  const m = /^\/(\w*)$/.exec(text);
  return m ? COMMANDS.filter((c) => c.name.startsWith(m[1]!.toLowerCase())) : [];
}
