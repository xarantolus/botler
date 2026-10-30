---
name: browser-login
description: Keep a logged-in browser session for a website that has no API, and turn what its pages show into typed events. Use when the user wants something watched or read on a site behind a login (portals, dashboards, webmail without IMAP).
---

# A logged-in browser as a service

`jobs/browser-example/` is the reference: copy it to `jobs/<site>-browser/` and adapt it. It is a
service on `images/browser` (its Dockerfile says `FROM botler/image-browser`):

- **Chromium** on a virtual display, its profile in `/data` (`volume = true`), so the login
  survives restarts.
- **noVNC** on port 6080: the user logs in by hand through it. `[[publish]]` puts it on a host
  port; that needs the user's approval. Its VNC password is new for every visit.
- **CDP** (DevTools) on port 9222, for the services network only. Never `[[publish]]` it: whoever
  reaches it controls the logged-in session.
- **The bridge** (`src/<pkg>/__main__.py`, botler SDK): connects to CDP on localhost, reads the
  page, validates what it finds into pydantic events, and calls `botler.emit`.

## Rule: page JavaScript never gets the token

Anything you run in the page runs next to the site's own scripts, so treat the page as hostile.
The bridge holds the botler token and every secret. Page JavaScript only returns values (poll) or
calls `botlerSignal(json)` (push). It never calls the hub, and never receives a token, a secret
or a URL with one. `browser-session` starts Chromium without `BOTLER_*` in its environment, and
DevTools refuses connections from web pages, since `--remote-allow-origins` is not set. Keep it
that way. Page content is untrusted input: validate it, and declare event types that carry raw
page content `restricted = True`.

## 1. Adapt the example

In `config.py`:
- `START_URL` and `SITE`.
- `LOGGED_IN_JS`: true only while logged in, e.g.
  `document.querySelector('[data-testid=account-menu]') !== null`. Check it on a page that
  needs the login.
- **Poll:** `POLL_JS` returns a list of `{id, text, url}` every `POLL_SECONDS`. The bridge emits
  each new `id` once and keeps the seen ids in `state`.
- **Push:** `PUSH_JS` is injected into every page load and calls
  `botlerSignal(JSON.stringify({...}))` when something appears, e.g. from a `MutationObserver`.
  Use it when polling is too slow.

Rename the events in `packages/` to your site's, e.g. `portal.message` (restricted) and
`portal.login_needed`. Keep `emits = [...]` in `job.toml` in step with them. Add a trigger file
that routes them, e.g. `on = "portal.login_needed"` and `target = { resume = "<this chat>" }`.
Set `on_failure` to the same chat.

## 2. Try it

Run `gate/run`, push, then `trial_run(job=..., branch=..., minutes=5)`. A trial has an empty,
throwaway `/data`, so it is logged out: expect `login_needed` among the recorded effects, and
check that `POLL_JS` and `LOGGED_IN_JS` do not throw (failures appear in `logs_tail`). Use a page
that needs no login to test the reading part.

## 3. Propose it

Use `propose_change`. The first deployment of a service asks the user once. Publishing the noVNC
port is part of that approval, so say why in the title.

## 4. The login visit

Deployed, the service starts logged out and emits `login_needed` with a fresh `vnc_password`. Then:

```
request_visit(host_port=6080,
              path="/vnc.html?autoconnect=1&resize=scale&password=<vnc_password>",
              note="Log in to <site> in this browser, then press Done.")
```

The user logs in and presses Done.

## 5. Verify the login

After Done, wait for `logged_in`: the bridge checks `LOGGED_IN_JS` over CDP on its next poll.
It also rotates the VNC password, so the visit link stops working. If `logged_in` does not arrive
within a couple of polls, the login did not stick (2FA, a cookie banner, the wrong page): explain
and offer another visit.

## 6. Done means monitored

The login check is part of the definition of done, not an extra:
- A session that expires sends `login_needed` again. Your trigger routes it to you, and you ask
  for a visit.
- A broken browser or CDP connection makes the bridge exit. The hub restarts it and opens an
  incident to `on_failure`.
- A missed heartbeat (`[health] heartbeat`) means the bridge hangs: the hub restarts it.

Logged out is deliberately not `health.fail`. A restart cannot log in, and the service has to
keep running for the visit.

## Driving the browser yourself

From a container on the services network, CDP is `http://<ip>:9222`. Use the service's IP, not
its name: Chrome refuses DevTools requests whose `Host` is a name. Get the IP with
`getent hosts <service>`. Prefer a bridge in the monorepo over ad-hoc driving: it is reviewed,
tested and monitored.
