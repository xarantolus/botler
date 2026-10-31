---
name: apps
description: Give a service a web UI the user opens inside botler (Apps in the menu), e.g. a dashboard, a list to curate, a settings screen of its own. Use when the user wants to see or work with something in a page rather than in chat.
---

# Apps

An app is a service's web UI. Add `[app]` to the service's `job.toml`:

```toml
lifetime = "service"
command = ["/app/.venv/bin/python", "-m", "listings"]
port = 8080

[app]
title = "Listings"
path = "/"          # what opens first (optional)
icon = "house"      # a Lucide icon name (optional)
```

The user opens it under **Apps**. The hub signs them in and proxies to your port; the page runs
on the hub's apps port (the web app's port + 1), never on the web app's own origin. Adding an app
asks the user the first time.

## What the hub gives you, and what it doesn't

- **Sign-in is done.** Every request that reaches you comes from the signed-in user. Build no
  login, and never ask for botler's token in the UI.
- **A path prefix.** You are served under `/apps/<name>/`, and the `X-Forwarded-Prefix` header
  says so. Use **relative URLs** (`href="items"`, `fetch("api/items")`), never `/items`.
- **Cookies** you set stay under your prefix. botler's own cookies never reach you.
- **Plain HTTP only:** no WebSockets through the proxy yet. Poll, or use server-sent events
  from a normal GET.
- **No hub token in the browser.** The page talks to your service, and your service talks to the
  hub with the SDK (`botler.state`, `botler.emit`, `botler.ops.*`). Keep hub calls on the server.

## Building one

- Keep it small: a Python service (Starlette, FastAPI or `http.server`) serving HTML and a JSON
  API, or a static build plus an API. Reuse `packages/` for shared models.
- **The data stays where it was.** The app shows and edits what the service already has
  (`botler.state`, its volume). An action in the UI that should reach an agent emits a typed
  event, and a trigger routes it.
- Read skill `building-well`: the app is shared by every browser tab and survives restarts.
- Test the HTTP handlers directly (no browser needed), then `trial_run` the service. Ask the user
  to open the app after deploying.
