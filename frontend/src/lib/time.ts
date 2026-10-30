export function ago(t: string | null | undefined, now = Date.now()): string {
  if (!t) return "";
  const s = (now - new Date(t).getTime()) / 1000;
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(t).toLocaleString();
}

export function elapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}

export function usd(v: number | null | undefined, digits = 2): string {
  if (v === null || v === undefined) return "";
  if (v > 0 && v < 0.01 && digits <= 2) return "<$0.01";
  return `$${v.toFixed(digits)}`;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
