// Spans of days for the Costs page. Days are `YYYY-MM-DD` in the hub's time zone.

export type Preset = "7d" | "30d" | "month" | "last-month";

export const PRESETS: { value: Preset; label: string }[] = [
  { value: "7d", label: "Last 7 days" },
  { value: "30d", label: "Last 30 days" },
  { value: "month", label: "This month" },
  { value: "last-month", label: "Last month" },
];

/** Today in `timeZone`. */
export function todayIn(timeZone: string, now = new Date()): string {
  // en-CA writes dates as YYYY-MM-DD.
  return new Intl.DateTimeFormat("en-CA", { timeZone, year: "numeric", month: "2-digit", day: "2-digit" }).format(now);
}

export function addDays(day: string, n: number): string {
  const d = new Date(`${day}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + n);
  return d.toISOString().slice(0, 10);
}

/** The days of `preset`, ending today (or with last month's last day). */
export function presetRange(preset: Preset, today: string): { from: string; to: string } {
  const month = today.slice(0, 8); // YYYY-MM-
  switch (preset) {
    case "7d":
      return { from: addDays(today, -6), to: today };
    case "30d":
      return { from: addDays(today, -29), to: today };
    case "month":
      return { from: `${month}01`, to: today };
    case "last-month": {
      const to = addDays(`${month}01`, -1);
      return { from: `${to.slice(0, 8)}01`, to };
    }
  }
}

/** `Mon 3 Nov` (no year: the span is on the page). */
export function dayLabel(day: string): string {
  return new Date(`${day}T00:00:00Z`).toLocaleDateString(undefined, { timeZone: "UTC", weekday: "short", day: "numeric", month: "short" });
}

/** Token counts in millions or thousands. */
export function tokens(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${Math.round(n / 1e3)}k`;
  return String(n);
}
