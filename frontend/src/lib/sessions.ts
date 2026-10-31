// A chat's session files (one per handover to a fresh session) as the picker shows them.

export interface SessionOption {
  value: string;
  label: string;
  /** When the session started, if its file name says (`<chat>-YYYYMMDD-HHMMSS-mmm.jsonl`). */
  started: Date | null;
}

/** When a session file was started, from its name; null for the first session (no date in it). */
export function sessionStart(file: string): Date | null {
  const m = /-(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})(?:-\d+)?\.jsonl$/.exec(file);
  if (!m) return null;
  const [, y, mo, d, h, mi, s] = m.map(Number) as [number, number, number, number, number, number, number];
  return new Date(Date.UTC(y, mo - 1, d, h, mi, s));
}

/** The picker's options, newest first: "Current session", then "Session n · started <when>". */
export function sessionOptions(files: string[], format: (d: Date) => string): SessionOption[] {
  return files
    .map((value, i) => {
      const started = sessionStart(value);
      const current = i === files.length - 1;
      const when = started ? ` · started ${format(started)}` : i === 0 ? " · the first" : "";
      return { value, started, label: current ? `Current session${when}` : `Session ${i + 1}${when}` };
    })
    .reverse();
}
