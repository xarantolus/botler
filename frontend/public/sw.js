// The installed app's service worker. Everything comes from the hub live (no offline copy that
// could go stale); a page load while the hub is unreachable shows a short note instead of the
// browser's error page.
const OFFLINE = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>botler</title><body style="font:16px system-ui;background:#18181b;color:#e4e4e7;display:grid;place-items:center;height:100vh;margin:0">
<div style="text-align:center"><p>The hub is unreachable.</p><button onclick="location.reload()">Try again</button></div>`;

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));
self.addEventListener("fetch", (e) => {
  if (e.request.mode !== "navigate") return;
  e.respondWith(
    fetch(e.request).catch(() => new Response(OFFLINE, { headers: { "Content-Type": "text/html; charset=utf-8" } })),
  );
});
