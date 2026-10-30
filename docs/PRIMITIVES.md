# Primitives: how botler stays small and extensible

This brings together the user's feedback so far into one model. **The goal:** most requests become *monorepo content built by agents*, not hub features. `ARCHITECTURE.md` describes the mechanisms. This document says which few concepts exist and where new work belongs.

## The test for "does this need a core feature?"

A request needs a hub change only if it needs at least one of these:

1. **A new trust boundary or escalation:** something the agent must not be able to grant itself. Examples: opening a port to the network, a new secret class, a new kind of mount.
2. **New hub-held state:** a fact that has to survive agents, held by the single writer.
3. **A new channel to the user:** for example push notifications, or a new kind of request.

Everything else is composed from the primitives below by agents, through transactions. Examples:
- daily digest
- inbound mail
- web search backends
- todo lists
- connectors
- apps
- self-hosted helpers the agent decides to run

## The eight primitives

### 1. Definitions (monorepo, changed only by transactions)

Everything that should exist is described here: agents, jobs, services, triggers, event types, tools, pi extensions and skills, policies, images.

- **One path to change them:** branch → gate → summary → reviewer → (user) → merge → reconcile.
- **Template upgrades** use the same path. Git merges new template snapshots into the user's monorepo.
- **The deployment summary** is the safety net. It is written for humans: what starts, stops and changes, and what to check.

### 2. Runnables, each in an Environment

There is one notion of "a container the hub runs". It comes in three lifetimes:

| lifetime | what runs | ends when |
|---|---|---|
| agent | pi session (`pi --mode rpc`) | idle, waiting on the user, or archived; it wakes on any delivery |
| job | deterministic program | it exits (oneshot) |
| service | anything long-running | never; supervised with backoff, crash loop → incident |

Every runnable gets **one Environment spec**, shared by all three:

```
image, home (persistent, /home/agent; own or shared by name), workspace (monorepo clone),
secret grants (env/file), network grants (services it may reach), published ports (services
only; escalation), tools (granted Python tools), mcp (stdio or service), TZ + localtime,
SDK + hub token, /memory mounts
```

- **Where the spec comes from:** an Environment comes from the definition and can be **narrowed or extended at spawn time** (`spawn_agent(def, task, env={secrets: [...], home: ...})`).
- **Extending needs a grant:** anything a spawn adds beyond the definition goes through Grants (5).
- **Apps** are services with published ports.
- **Self-hosted helpers** are services an agent defines when it decides to.

### 3. Events (one journal, typed)

Everything that happens is an event in one journal:
- messages, end-of-turn notices, timers, schedules, webhooks;
- job output, failures, request answers, proposal progress, breakers.

**Types:** built-in event types are Rust types, and TypeScript is generated from them. Agent-defined event types are pydantic models, and their JSON Schema is generated. `emit` validates the payload and returns feedback if it doesn't match.

**Consumers** of the journal:
- the router (deliveries to agents)
- the UI's live stream
- incidents
- the search index

**Delivery rules (core, generic):**
- **Timestamps:** every delivery carries the local time and a gap note.
- **Acknowledgements:** short acknowledgements are delivered without waking the agent.
- **End of turn:** a child's end of turn notifies its parent, unless its last action was messaging the parent.
- **Loop safety** is generic: the pair breaker, the hop backstop and the spawn depth limit.

### 4. Triggers → Targets

Triggers start things:
- `schedule` (cron or interval)
- `timer` (one-off; runtime)
- `on` (event subscription)
- `webhook`

**Targets:** `resume` a chat, `spawn` an agent (with an environment), or `run` a job.

Recurring behaviour is a trigger file in the monorepo (reviewed, with a summary). One-off behaviour is a timer.

### 5. Grants (one policy engine for every escalation)

Anything an agent can't give itself is a **grant** of a *resource* to a *holder*:

| resource | examples | holder |
|---|---|---|
| secret group | `github-ssh`, `gitlab-x-ssh` | chat (instance), or a definition for fixed grants |
| service access | reach `botler-svc-x` | definition or chat |
| published port | `0.0.0.0:8080` | service definition |
| capability | read other sessions, hub-class actions | definition |

**How grants arise:**
- **Definitions:** reviewed, and escalations require the user's approval.
- **Spawn:** `spawn_agent(env=...)`.
- **Later request:** `request_access(resource, reason)`. A chat sleeps after its turn and wakes with the grant applied.

**One policy file** (`policies/grants.toml`, a protected path) says, per resource, whether to `ask` (default) or `auto` (allow without asking, still logged). The choice "always allow for …" in an access request writes that policy through a hub commit.

**Which** secret group fits a situation is *knowledge*, so it lives in memory. For example: "GitHub repo work → `github-ssh`; GitLab host X → `gitlab-x-ssh`". The spawning agent reads it and asks for the right grant. The hub only enforces what the user approved.

### 6. Requests (the only way to involve the user)

There is one typed, asynchronous primitive. It has kinds:
- **question:** free text or multiple choice
- **credentials:** a typed form of env and file fields, with file upload, binary-safe
- **approval:** a change summary, or an action with arguments
- **access:** a grant (5)

**Properties:**
- Every request has an id and can be cancelled. It can be aborted automatically when the thing it is about changes.
- It is shown in the Requests inbox *and* inline in the chat that asked.
- Asking ends the agent's turn, through a terminating tool.
- A chat that is only waiting sleeps after 2 minutes. The answer arrives as an event and wakes it.

New request kinds count as core features under rule 3 of the test. Everything else reuses these kinds.

### 7. Knowledge

| store | holds | written by |
|---|---|---|
| monorepo | code, definitions, prompts | transactions |
| memory | durable facts, preferences, decisions (Markdown; per definition, per project, user) | `memory_write` with an expected version |
| home | working files, clones, scratch | the agent, freely |
| sessions | transcripts (pi files, append-only; compaction keeps history) | pi |
| journal | events | the hub |

**One search index** (SQLite FTS) is derived from all of these and can be rebuilt. It is exposed as `search`, scoped by the same capabilities as `read_session`. An agent searching its own history is the same tool with `scope=self` and few results. There are no interim stopgaps: features built on search wait for the index.

### 8. Extension points (agent-editable, no hub change)

| want | build it as |
|---|---|
| a new tool for agents | `tools/<pkg>` with Python `@tool` (pi tool, schema from type hints) |
| a new event type | pydantic model in `packages/`, which generates the manifest |
| agent behaviour | `agents/<x>/AGENTS.md`, pi skills and extensions in `pi/` |
| something periodic | `triggers/*.toml` |
| a poller or long-running helper | `jobs/<x>` with `lifetime = "service"` |
| something reachable from the user's network | a service with `publish` (escalation) |
| an integration that sends to others | a hub-held action job (approval per call) |

**Precedent:**
- `web.search` and `web.fetch` are tools agents fix when they break.
- `todo` is a pi extension in the monorepo.
- `web-health` is a job whose failure reaches triage.

## Closed core (non-negotiable, small)

- **Hub:** scheduler, router, supervisor, transactions, requests, grants, secrets store, memory store, search index, web/API.
- **pi extension:** delivery envelopes and dedup, terminating ops, Python tool bridge.
- **Python SDK:** generated from the op registry.
- **Base images.**
- **UI shell:** the approval and secret surfaces must never be agent-authored.

**Self-hosting:** the core runs nothing self-hosted beyond what it needs. Agents may run services for the user.

## Structural changes this implies (relative to today's code)

1. **One op registry.**
   - Each op is defined once with typed input and output (schemars + ts-rs).
   - MCP tools, the HTTP ops endpoint, the Python SDK, TypeScript bindings and the terminating pi tools are all generated from it.
   - Today `ops.rs` repeats each op in three places and is the main merge hotspot.
2. **One `Environment` type** shared by agents, jobs and services. Spawn-time overrides are merged in, and anything beyond the definition goes through grant checks. Today the fields are spread over `Agent` and `Job`.
3. **One grants table and policy:**
   - It replaces per-definition secret lists as the only mechanism; fixed grants become definition-held grants.
   - It adds `request_access`.
   - Escalation detection in `change.rs` becomes "diff of grants".
4. **Handover as a primitive:** `handover(target)` covers archive → most relevant agent, and Botler rolling over into a fresh session (target = self).
5. **Hub reached over a Unix socket** mounted into containers. The hub can then listen on localhost only, which makes Grants the only way to reach the network.

## Build order from here

1. **In flight:** template upgrades, services and published ports, memory, the search fallback.
2. **Op registry (1) and Environment (2).** These are refactors with no behaviour change, done before more ops land.
3. **Grants and `request_access` (3).** Secret grants per chat at spawn; memory-guided.
4. **Search index:** sessions, journal, monorepo, memory; `search` with self scope.
5. **Handover primitive (4):** Botler rollover.
6. **Unix-socket hub (5)** plus binding to localhost.
7. **Uploads, share target, ntfy:** a new user channel, so rule 3 applies.
8. **Session tree** (rewind and fork), **actions**, **webhooks**, `replay_event`.
9. **Apps extras:** the user's login in front of apps, starting on the first request, previews. Basic apps already work as published services.
