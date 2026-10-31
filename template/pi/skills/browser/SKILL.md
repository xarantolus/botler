---
name: browser
description: Browse the web in a real Chromium: open pages, read them, click, fill forms, take screenshots. Use when a page needs JavaScript, interaction or a screenshot (plain reading of static pages is `web_fetch`).
---

# The browser

`jobs/browser` is a real desktop Chromium (not headless, so fewer sites block it) with ads and
trackers blocked, driven through [Playwright MCP](https://github.com/microsoft/playwright-mcp).

## Getting it

If you don't have `browser_*` tools, call `request_access(resource="service:browser", reason=...)`.
It is granted without asking the user. You get the tools when you wake, after this turn.

## Using it

- `browser_navigate(url)`, then `browser_snapshot()`: the page as an accessibility tree. Each
  element has a `ref`, which you pass to `browser_click`, `browser_type`, `browser_fill_form` or
  `browser_select_option`. Prefer snapshots to screenshots for reading: they are text and much
  cheaper.
- `browser_take_screenshot()` when the user should *see* the page. The screenshot shows in the
  chat. `attach_files` sends a saved file to another chat.
- `browser_tabs` lists, opens and closes tabs; `browser_wait_for` waits for text or a delay;
  `browser_evaluate` runs JavaScript in the page; `browser_console_messages` and
  `browser_network_requests` help when something does not load.

## What to expect

- **Your session is yours alone.** Your connection has its own browser context, with its own
  cookies, storage and tabs. Other chats using the browser at the same time never see them, and
  you never see theirs.
- **It does not last.** The context closes after 10 minutes without a browser tool call, and
  whenever you sleep. You then start fresh and logged out. Finish a flow in one go, and keep
  anything you need (text, a screenshot path) in your home.
- **Logins:** for a site the user stays logged into over time (a portal, webmail), don't type
  their password here. Use skill `browser-login`: a separate service keeps that session, and the
  user logs in once through a visit.
- **Blocked domains:** ad and tracker domains don't resolve. If a page breaks because of it,
  that is the list (`images/browser`, pinned StevenBlack hosts). Say so rather than working
  around it.

## From a job (deterministic scraping)

A job or service can use the same browser: `services = ["browser"]` in its `job.toml` (the
first time asks the user), then talk MCP to `http://browser:8000/mcp`. Each MCP session is its
own context, so a job never disturbs chats. In Python, the `mcp` package's client:

```python
from mcp import ClientSession
from mcp.client.streamable_http import streamablehttp_client

async with streamablehttp_client("http://browser:8000/mcp") as (read, write, _):
    async with ClientSession(read, write) as s:
        await s.initialize()
        await s.call_tool("browser_navigate", {"url": url})
        page = await s.call_tool("browser_evaluate", {"function": "() => document.body.innerText"})
```

Keep the parsing, dedupe (cursor in `botler.state`) and typed events in the job. The browser only
loads pages.
