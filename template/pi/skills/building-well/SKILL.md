---
name: building-well
description: Design rules for anything built in the monorepo (services, tools, jobs, images, MCP servers). Several chats use it at once, it restarts at any time, and it must clean up after itself. Read before building or changing a service, tool or job.
---

# Building things that hold up

What you build is shared. Several chats use a tool or service at the same time. The hub
restarts services whenever it likes: crashes, deploys, hub restarts. The next agent reads your
error messages with no context. Design for that from the start; it is much harder to add later.

## Many chats at once

Every chat granted a service reaches the same process. Unless the user asked for shared state
(a single logged-in session they set up once), one chat's work must never affect another's.

- **Give each caller its own session.**
  - `open_session()` returns an id, every call takes it, and `close_session(id)` ends it.
  - Keep each session's state (pages, cursors, temp files, credentials) under that id, never in
    globals.
- **Share the expensive part and isolate the rest.** Start one heavy process and give each
  session an isolated slice of it, rather than one process per chat.
  - Example: a Chrome service for agents runs one Chromium, and each session gets its own
    *browser context* (CDP `Target.createBrowserContext`): its own cookies, storage, tabs and
    downloads directory. A session never sees another session's tabs.
  - If the user deliberately keeps one logged-in profile (skill `browser-login`), say so in the
    tool's description. Hand sessions *new* contexts, or serialize access with a lock and a
    clear "busy, retry in N s" error.
- **Limits and timeouts.**
  - Cap concurrent sessions and expire idle ones (e.g. 10 minutes).
  - When full, refuse with a clear error ("3 sessions open, the limit; close one or retry")
    instead of slowing everyone down.
  - Reap anything without a live session at startup and on a timer.
- **No caller identity unless you build it.** Chats reach a service directly over the
  services network, and the hub does not tell it who is calling. Isolation by session id keeps
  chats from getting in each other's way; it is not a security boundary. The grant (who may
  reach the service at all) is that boundary.
- **Test it concurrently.** Two sessions doing conflicting things at the same moment (logging
  in, navigating, writing the same file) must both succeed with their own results. Write that
  test.

## Restarts and crashes

- **No promise that lives only in memory.**
  - Persist cursors and progress (`botler.state`, the job's volume) so a restart resumes where
    it stopped instead of repeating or skipping work.
  - Make handlers idempotent, keyed by message id or event id.
- **Sessions after a restart.** In-memory sessions are gone, so a call with an unknown id gets
  "session expired; open a new one", which the agent can act on, never a crash.
- **Stopping and starting.** Exit promptly on SIGTERM. Start in seconds, and report health
  (`botler.health.ok()` / `health.heartbeat`) only once you can actually serve.

## Errors written for the next agent

- **Say what went wrong, why, and what to do next.** For example: "unknown folder `Inbx`;
  folders: INBOX, Archive, Sent". Not just "error 400".
- **Separate the caller's mistakes from your own failures.** Bad input returns an explanation.
  A broken dependency is logged with `botler.log` and surfaces as unhealthy, so `triage` hears
  about it.
- **Never put secrets in errors, logs or events.**

## Small, typed, reused

- **Reuse first:** `packages/shared/`, `tools/`, `connectors/`, existing images
  (`FROM botler/image-<x>`), and existing event types.
- **Keep interfaces small and typed:** a few tools with typed parameters (`@tool` type hints,
  pydantic events) beat one tool taking free-form JSON. Tool descriptions say what the tool
  is for and what it costs (slow, rate-limited).
- **Configuration and secrets:** configuration that the user may want to change belongs in the
  definition, not in code constants. Secrets come from a secret group.
- **A README in the job or tool directory:** what it is for, who uses it, how sessions work,
  and its limits.

## Before proposing

- **Run the tests,** including the concurrent one and a restart in the middle of work.
- **Try a service** with `trial_run`: two sessions at once, then a restart.
- **Write the deployment summary** to state what is shared between chats and what is not.
