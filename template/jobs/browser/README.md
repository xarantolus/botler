# browser

The agents' browser: [Playwright MCP](https://github.com/microsoft/playwright-mcp) attached over
DevTools to the Chromium of `images/browser` (a real X display, not headless; ads and trackers
blocked through the pinned hosts list). Skill `browser` says how chats use it.

- **Isolation:** every MCP session (one per chat connection) is its own in-memory browser context:
  cookies, storage and tabs are never shared. A context closes after 10 minutes without a tool call
  and starts fresh on the next one; a chat that sleeps reconnects with a new, empty context.
- **Shared:** one Chromium process, its cache-free defaults and the blocklist. Logged-in sessions
  that must persist are a different pattern: skill `browser-login`.
- **Updating:** Playwright MCP's version is in the Dockerfile; Chromium follows Debian's security
  updates in `images/browser`; the blocklist commit is pinned there too.
