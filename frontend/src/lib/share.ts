// What a share (PWA share target) tells botler: title, text and URL, without repeats (apps
// often put the URL into `text` too). Null when nothing was shared.
export function shareText(title: string | null, text: string | null, url: string | null): string | null {
  const parts: string[] = [];
  for (const p of [title, text, url]) {
    const v = p?.trim();
    if (v && !parts.some((x) => x.includes(v))) parts.push(v);
  }
  return parts.length ? `Shared with you:\n\n${parts.join("\n")}` : null;
}
