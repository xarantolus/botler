// Copying text. `navigator.clipboard` exists only on secure pages (https, localhost): a hub opened
// over plain http on the LAN gets the older selection copy instead.

export async function copyText(text: string): Promise<void> {
  if (navigator.clipboard) return navigator.clipboard.writeText(text);
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  try {
    if (!document.execCommand("copy")) throw new Error("the browser refused to copy");
  } finally {
    area.remove();
  }
}
