# Botler architecture

## Context

`PLAN.md`, `DECISIONS.md` (1–28), `OPEN_QUESTIONS.md` (1–60) and `REQUESTS.md` (R1–R27) describe a personal autonomous agent system. They are strong on policy (approval, reviewer, gate, dialogs, apps) but the mechanism was built on Kestra plus a custom "our store", with many state systems. We start over with these mechanism decisions (made 2026-10-30):

- **pi owns everything about sessions**: agent loop, tools, MCP client, model providers, transcripts (JSONL session trees with usage/cost, compaction entries, custom entries), branching, resume. We write **one pi extension + one MCP server + system-prompt additions**, not an agent runtime.
- **A small Rust hub** is the only always-on, non-agent-editable part. It owns what pi cannot do: surviving reboots, timers and schedules, the event/message queue, waking the right agent, containers, the monorepo transaction, approvals, and the web API.
- **Kestra/Windmill are dropped.** The hub runs scheduled "job containers" (Dockerfile + pyproject, DECISIONS 14).
- **State**: definitions live in the **monorepo** (git, hub-owned bare repo with an optional mirror). Transcripts are **pi session files**. Only runtime facts that are not definitions go in **one SQLite file**: one-off timers, the event log, the delivery queue, dialogs, proposals, runs and secret values.
- **The monorepo transaction is a core concept**: branch → gate → reviewer → (you, if needed) → fast-forward main → the hub reloads and reconciles.
- **Credentials come in two classes**, chosen when you enter a secret:
  - `container`: a plain env var or mounted file per container, declared in the monorepo. For read access, pushing to your own repos, and so on.
  - `hub`: never enters a container. Only used by approval-gated hub actions (anything that reaches other people).

  This supersedes most of DECISIONS 13/24.8.
- **Main chat = `main` + topic threads.** `main` is one long-lived, compacting session that acts as router. Bigger topics get their own persistent instances (threads/workspaces), which you can open directly and `main` can hand off to.
- **Delivery to a busy agent**: normal messages are queued as pi `followUp`. Urgent messages and your own chat messages are delivered as `steer`. `send_message` never blocks; replies come back as correlated events.
- **Host**: one Linux server, **rootless Podman as primary target** (Docker API-compatible socket via bollard). Known gaps to handle: catatonit/`--init` for reaping, the `kernel.keys` quota under many containers, and the browser sandbox later.
- **The first full build includes**: hub-enforced outbound approval, the reviewer agent and the deterministic gate. The web app comes first; Flutter comes later.

## Architecture principles (what makes it last)

1. **Crash-only hub.** Startup *is* recovery: load definitions@main, open SQLite, recompute the next fire times, re-queue undelivered deliveries, reconcile containers. No in-memory state that matters.
2. **Declarative reconcile.** Desired state is definitions@main. The reconciler converges images, schedules, always-on agents and services. The same code path runs at boot and after every transaction.
3. **Derived state is rebuildable.** Next fire times, the search index and usage rollups can all be rebuilt from repo + SQLite + session files. Only facts are stored, never caches you can't throw away.
4. **At-least-once delivery, idempotent consumers.** An event is journaled before it is acted on (outbox pattern). Deliveries carry ids, and the pi extension records delivered ids as `custom` entries so a redelivery after a crash is dropped.
5. **Ports and adapters.** `botler-core` is pure (no IO): definitions, schedule math, routing, capability diff, policy decisions. Store, git, containers and pi are traits with real and fake adapters, so the core logic is tested deterministically without Docker.
6. **Enforcement by withholding, not by asking nicely.** An agent can do anything its container's secrets allow. The approval guarantee (DECISIONS 1) holds because send/deploy-class secrets are **hub-held** and only used by hub-executed *actions* after an approval. That line is explicit in every definition.
7. **Single writer.** Only the hub writes SQLite and `main`. Everyone else goes through the MCP or HTTP API.
8. **Versioned contracts.** Definition files have JSON Schemas generated from Rust types (`schemars`) and a `version` field. MCP tool schemas are versioned. The pi version is pinned in the agent base image and bumped deliberately.
9. **Small closed core, editable everything else.** Core = hub + pi extension + base images + Python SDK. Everything else (agents, prompts, skills, jobs, connectors, events, gate config, policies) is monorepo content and changes through transactions. Protected paths (policies, gate, capability config, secret grants) force your approval.
10. **Portable host contract.** The hub targets any Linux machine with one Rust binary, a data directory and rootless Podman. Nothing is tuned to a specific host. Only a few things are auto-detected at startup and recorded in `botler status`: the Podman socket path, rootless or not, and the network backend. Agents run as container root, which rootless Podman maps to the hub's unprivileged user, so there's no `keep-id` dependency. The host filesystem is only reachable by agents through declared container mounts.

## System shape

```
 Web app ──HTTP/WS──┐
 ntfy push  ◄───────┤
                    ▼
 ┌──────────────── botler-hub (Rust, systemd) ─────────────────┐
 │ API (axum) · MCP server (rmcp, streamable HTTP, per-instance │
 │ token → role-scoped tools) · Scheduler (cron + timers)       │
 │ Router (events → deliveries) · Dispatcher (wake/steer/prompt)│
 │ Reconciler · Transactions (gate→review→merge) · Actions      │
 │ Store (SQLite/sqlx) · Repo (bare git + worktrees, git CLI)   │
 │ Runtime (bollard: Docker/Podman) · pi RPC client             │
 └──────┬──────────────────┬───────────────────┬────────────────┘
        │ docker           │ docker            │ docker
  agent container    job container        service container
  pi --mode rpc      python (uv) run      long-running (MCP srv…)
  + botler ext       emits events →hub    health-checked
  sessions volume    hub-held secrets only for approved actions
```

**Agent lifecycle.** An *agent definition* (repo) has *instances* (SQLite rows). `main` is a persistent instance. Subagents are ephemeral instances spawned via MCP `spawn_agent(def, task)`. Workspaces are persistent instances with a named volume. An instance is either dormant (session file + volume only) or running (container + `pi --mode rpc --session <file>`). The dispatcher wakes a dormant instance for each pending delivery: it starts the container on the **current** image and definition, resumes the session, and prompts with the coalesced deliveries. It uses `steer`/`follow_up` when the agent is busy. Idle timeout makes the instance dormant again. A reboot loses nothing: alarms live in the hub.

**Triggers → targets (one model for everything that "starts something").**
- **Triggers** (declared in definitions, except timers):
  - `schedule` (cron/interval, in a file);
  - `timer` (a one-off, runtime row in SQLite);
  - `on` (an event subscription with a filter);
  - `webhook` (later, but designed in now: `POST /hooks/<name>`, verified by a per-hook HMAC or token secret, payload becomes an event with `provenance=external`).
- **Targets** (what a trigger does):
  1. `resume`: deliver a message into an existing chat/instance (e.g. `main`, or a named workspace).
  2. `spawn`: start a fresh agent instance from an agent definition and environment (image, mounts, secrets) with a task prompt. It reports back via events and goes dormant or ends.
  3. `run`: run a deterministic Python job in a container. No LLM. It decides itself whether to emit events, which can then trigger 1 or 2.
- **Runnables have a lifetime**:
  - `oneshot`: run to completion, triggered.
  - `service`: long-lived forever. The hub supervises it with restart and backoff, health checks and logs. For frequent pollers, MCP servers, and later apps. A service is just a job whose trigger is "always".
- Every trigger→target pair is a row the reconciler derives from definitions@main. A schedule or webhook never owns logic itself; it points at a target.

**Injected botler SDK (how containers reach us).** Every container the hub starts (jobs, services and agents) gets:
- `BOTLER_URL` and a short-lived, per-run, role-scoped `BOTLER_TOKEN`;
- the pinned SDK bind-mounted read-only at `/opt/botler/sdk`, with `PYTHONPATH` set. It's a stdlib-only HTTP client, so it works in any image without being in the job's dependencies, and the hub version and SDK version always match. A `uv` path dependency on the same package is available for type checking in the monorepo.

The SDK surface:
- `emit(type, payload)`, `send_message(to, body)`, `set_timer(at, event)`;
- `state.get/set` (a small per-runnable KV in SQLite for poller cursors and dedup keys);
- `health.ok()/fail(reason)` (service heartbeats);
- `log`;
- the `@action` decorator and manifest helpers for connectors.

Agents reach the same hub endpoints via MCP. Scripts the agent writes inside its container can also use the SDK.

**Supervision (hub is the single supervisor).**
- **Why not Podman's restart policy:** containers are started with no Podman restart policy, so there's one restart loop rather than two competing ones, and rootless `unless-stopped` doesn't survive a reboot without `podman-restart.service`.
- **Services:** the hub subscribes to the Podman event stream (`died`, `oom`, health-status changes) and reconciles desired vs actual services on boot, after every transaction and on a periodic tick (to catch missed events). It restarts with exponential backoff. A crash loop (N exits in a window) puts the service in `failed`, stops restarting it, and escalates.
- **Schedules** (oneshot runs):
  - each declares a `missed` policy (`skip` | `run_once`) for downtime and reboots;
  - an `overlap` policy (`skip` | `queue` | `allow`);
  - a `timeout`;
  - `last_fired` lives in SQLite.

  A non-zero exit, a timeout or OOM is a failure.
- **Every failure becomes an incident, and incidents go to an agent.** The hub opens (or updates) an **incident** row in SQLite, so 50 crashes become one incident with a count, not 50 events. It emits `runnable.failed {runnable, kind (exit|oom|timeout|unhealthy|crashloop), exit_code, restart_count, logs_tail, image, commit, incident_id}`.
- **Routing:** the event goes to the runnable's declared `on_failure` target, defaulting to a template **`triage` agent** (spawn or resume). Triage has:
  - `get_run_logs`, `get_incident`;
  - a monorepo worktree;
  - its notes/memory (past incidents and fixes);
  - web access for docs;
  - `replay_event`;
  - `propose_change`.

  It then decides to fix (proposal), retry, silence or ask you. Incident resolution is recorded, and a recurrence reopens the incident with history.
- **Hub self-failures** (the image build failed, the definition for a commit is invalid, the Podman socket is gone) go through the same incident path. Things like the daily digest, the email channel and voice are *not* built in. The core gives agents enough primitives to build them as monorepo content:
- **See other agents**: `list_agents`, `get_events(filter)`, and `read_session(instance, since)` (a capability that `main` holds by default). Plus `send_message` with correlated replies, so "ask each agent what it did today" works.
- **Self-scheduling**: `set_timer(at, target)` and proposing a trigger file (cron → `resume main` with a prompt). For example, the "daily digest" is an agent-written trigger and prompt.
- **Resident anything**: services can have a persistent volume, any image (including a browser image with a logged-in profile), a hub-class or container secret, and the SDK. "A 24/7 container with my mail provider's web app open, emitting `mail.received` events" is a valid thing an agent builds.

**Uploads & share.** Files and images you attach in the web app land in a scratch space (`data/uploads/<id>`). The delivery tells the agent the path, mounted read-only at `/inbox/files/` in its container, and the LLM decides whether to read or view it (pi handles images). A **share-to-botler** endpoint (`/share`: a PWA Web Share Target plus a bookmarklet URL) turns shared text, URLs and files into an event to `main`. No built-in voice.

**Rate limits & breakers (core, everywhere).**
- Token buckets on every MCP tool and SDK endpoint, per instance/runnable and global. Defaults are in core; agents can tighten them in definitions but only raise them through a transaction that needs your approval.
- Breakers for agent↔agent ping-pong (message-cycle detection on correlation chains), event storms (per-source rate cap) and wake churn (per-instance wakes/hour). A tripped breaker pauses that source and raises an alarm event to you.
- A **kill switch** (web button + `botler pause`) freezes scheduling and dispatch, aborts running pi runs and stops services. Resume is explicit.

**Rollback = git.** Reverting is just a revert commit on main through the same transaction path; the reconciler re-applies the state. If definitions@main fail to load at boot, the hub refuses to apply and stays on the last applied commit (recorded in SQLite), then alarms.

**Debugging changes before merge.**
- `replay_event(event_id, branch)` re-runs a job/spawn target against a past event, using the branch's definitions and images. Outbound actions are forced to dry-run, and results are returned to the agent.
- `preview_change(branch)` returns a human-readable **deployment summary** against main. It covers every trigger in plain language (e.g. "at 07:00 on weekdays, Europe/Berlin") with its next five fire times, subscription matches, runnables added/removed/changed, new secret grants, tools and capabilities, and what will need your approval and why. The same summary is returned by `propose_change` and `botler validate`, and is shown to you in approval dialogs.
- `get_run_logs(run)` and `validate(branch)`.

**Python tools for pi (agent-authored tools without TypeScript).**
- **Where they live:** `tools/<pkg>/` in the monorepo, a uv workspace member with functions decorated `@tool` (from the botler SDK). Type hints and pydantic models give the JSON schema, and the docstring gives the description.
- **Manifest:** the gate runs `python -m botler.tools manifest`, which writes `tools.manifest.json`. That makes tools part of the capability diff, with declared `effects` (e.g. `network`, `fs_write`, `secret:<name>`).
- **Registration:** the core botler pi extension reads the manifest at session start and calls `pi.registerTool` for each tool the agent definition grants (`tools = ["github.*"]`).
- **Execution:** `python -m botler.tools call <name>` runs as a subprocess in the agent's container, with JSON in and JSON out and a timeout. The result is returned to pi.
- **What still needs approval:** tools needing a hub-class secret are `@action`s instead. Their pi tool calls `invoke_action`, so they go through approval.

**pi packages.** Agents can propose adding community pi packages (skills/extensions) to `pi/`, pinned by version, through a normal transaction. The capability diff flags new extensions, since they run with full process rights inside the container.

**Memory (hub-mediated, concurrent-safe).**
- **Storage:** Markdown notes in a hub-owned `memory.git`, separate from the monorepo. Every write is auto-committed with the author instance, and there's no gate.
- **Namespaces:** per agent definition (shared by all its instances, e.g. every `triage`), per workspace/project, and a global `user` profile.
- **Reading:** an agent's namespaces are mounted read-only, so plain file tools and `grep` work.
- **Writing:** writes go through MCP `memory_write(path, content, expected_version)` with optimistic concurrency. A stale version returns a conflict with the current content, so concurrent instances never silently overwrite each other.
- **Search:** memory is covered by the derived search index.

**Built-in event catalog.** Every hub event type (`runnable.failed`, `proposal.updated`, `dialog.aborted`, ...) has a catalog entry in core with a summary, payload fields, follow-up tools filled in with the event's values (e.g. `get_incident_history(runnable=...)`, `get_run_logs(run_id=...)`, `replay_event`, `resolve_incident`), and guidance. Deliveries are rendered from it. Failure guidance points the agent at incident history (core fact: every incident stores its resolution) *and* its memory notes, and requires `resolve_incident` with the cause and fix.

**Messaging.** Everything is an event `{id, type, source, provenance(trusted|external|untrusted), to?, correlation?, payload}`. Sources include schedules, timers, job output, MCP `send_message`/`emit_event`, user chat, dialog answers, transaction results and hub alarms (job failed, agent crashed, health check failed). Routing uses direct addressing plus subscriptions declared in agent definitions (`[[on]] type = "connector.failed"`).

**Approvals and dialogs.** `ask_user(question, ctas, about[], ttl, priority)` creates a dialog. An event touching an `about` target aborts it, and a late answer is rejected (DECISIONS 21). `invoke_action(name, args)` on an action tagged `approval` creates a proposal bound to the args hash. You approve in the web app's fixed shell, then the hub runs the action in a job container with the hub-held secret and returns the result as an event.

**Web app, API types and the live stream.** The web app (`frontend/`, Vue 3 + TypeScript + Naive UI) is built with Vite into `frontend/dist`, which is committed and compiled into the binary (`include_dir`); the hub serves it at `/` with an SPA fallback, and the old stopgap page at `/legacy`. Every JSON shape it reads is a named Rust type (`crates/botler-hub/src/wire.rs` for API responses, store and core records, `live.rs` for WebSocket messages, `botler-core` `payloads.rs` for built-in event payloads as a tagged union). The TypeScript types and a typed route table (`routes.ts`: path, params, query, body and response per route) are **generated** from those types with ts-rs into `frontend/src/generated/`; the frontend never declares API shapes itself. Regenerate with `BOTLER_UPDATE_BINDINGS=1 cargo test -p botler --test bindings`; the same test fails when the committed output is stale, `web::tests::embedded_frontend_is_current` fails when `dist` is older than the frontend sources, and a router test checks that every route in the table is served. `GET /api/v1/live` is a WebSocket: the client authenticates with its first message (`{"type":"auth","token":…}`, so tokens stay out of URLs), then receives every event, agent status change (`waking` with its step, `running`, `dormant`, `failed`, `paused`) and dialog, plus raw pi events of the one chat it `watch`es. The app falls back to polling while the socket is down and reconnects with backoff.

**Monorepo transaction.** An agent works on branch `agents/<instance>/<slug>` in its worktree and calls `propose_change(branch, summary)`. The hub then:
1. Rebases onto main or rejects.
2. Runs the **gate** (a job in the dev image: ruff, format, pyright strict, pytest with no network, plus `botler validate`, which parses every definition with the hub's own parser and emits the action/capability manifest).
3. Computes the **capability diff** in core: new secrets or grants, mounts, network, approval-tagged actions, protected paths, schedules.
4. Runs the **reviewer**: a fresh ephemeral instance of `agents/reviewer`, with no secrets, a read-only diff, the `policies/reviewer.md` policy, and verdict via MCP `submit_review`.
5. Applies the decision table: reviewer approve + no protected or capability changes → auto-merge; otherwise a dialog to you.
6. Fast-forwards main, then reconciles (image rebuilds, schedule table, services; agents pick up new definitions on their next wake, never mid-turn).
7. Push-mirrors to GitLab/Forgejo if configured. Gate failures go back to the agent as an event.

**pi integration (reuse first).**
- Agent base image: pinned pi + Node + Python/uv + git + the **botler pi extension**.
- The extension:
  - registers the hub MCP server (`pi.registerMcpServer`, token from env);
  - adds system-prompt sections via `before_agent_start` (identity, inbox semantics, what needs approval, how to propose changes);
  - renders deliveries as `custom_message` entries with provenance labels;
  - records delivery ids for idempotency;
  - can block tool calls via `tool_call` (defence in depth only).
- pi's own `AGENTS.md` loading carries each agent's prompt (`agents/<name>/AGENTS.md`). Agent-authored pi skills, prompts and extensions live in the monorepo `pi/` directory and are loaded per agent. This is the agent-editable layer.
- The hub reads usage/cost from the RPC `message_end` events for budgets and sends `abort` when a budget is exceeded (stop and report, never truncate).
- Models: `botler.toml [models]` routing table, rendered into pi settings/models config per agent. The provider key is a container secret (provider-side spend cap stays external, DECISIONS 6).

**pi spike findings** (see `docs/spikes/pi-1.4.md`):
- the pi agent dir is a tmpfs with read-only rendered config files on top;
- `PI_OFFLINE=1` is always set;
- `maxTokens` is rendered per route;
- the RPC client fails fast on `success:false`;
- delivery uses an extension `input` transform with dedup via `appendEntry`.

## Repository layouts

**This repo (`botler/`)**
```
Cargo.toml                 workspace
crates/botler-core/        pure domain: definitions (serde+schemars), events, cron/timer math,
                           routing, capability diff, transaction decision table, dialog rules
crates/botler-store/       SQLite via sqlx, migrations, repositories per aggregate
crates/botler-repo/        bare repo, worktrees, branch/rebase/ff, load definitions @commit
crates/botler-runtime/     ContainerDriver (bollard) + PiRpc client (JSONL over attach) + fakes
crates/botler-hub/         binary: scheduler, router, dispatcher, reconciler, transactions,
                           actions, axum API/WS, rmcp MCP server, embedded web assets
crates/botler-cli/         `botler init|validate|status|secret set|logs|dev`
pi-extension/              TypeScript @botler/pi-extension (tests with pi SDK in-process)
python/botler-sdk/         job/connector SDK: emit events, @action decorator, health check, manifest
images/agent-base/, images/job-base/
frontend/                  Vue 3 + TypeScript + Naive UI web app; src/generated/ is generated
                           from the Rust API types; dist/ is committed and embedded in the binary
web/                       the old dependency-free page, served at /legacy
template/                  the monorepo template (below)
docs/ARCHITECTURE.md, docs/DECISIONS.md (consolidated), docs/legacy/ (current 4 files)
```

**Template monorepo (instantiated by `botler init`, then personalised by `main` via transactions)**
```
botler.toml                models routing, defaults, mirror, push, budgets
agents/{main,reviewer,triage,dev}/agent.toml + AGENTS.md   (image, secrets, mounts, network, mcp,
                           subscriptions [[on]], schedules [[schedule]], idle, budget)
jobs/<name>/job.toml Dockerfile pyproject.toml src/ tests/   (lifetime = oneshot|service;
                           [[schedule]] / [[on]] / [[webhook]] triggers with target = run)
triggers/*.toml            standalone trigger→target bindings (e.g. cron → resume main,
                           cron → spawn agent "researcher" with task)
connectors/<name>/         python package: client, @action fns (secret class, approval tag), health
packages/shared/           uv workspace member (reuse-first clients)
events/*.toml              event type declarations + JSON schema
pi/{extensions,skills,prompts}/
images/{agent,dev}/Dockerfile   (FROM botler agent-base)
policies/reviewer.md, policies/capabilities.toml
gate/                      gate script + ruff/pyright/pytest config
pyproject.toml, uv.lock    uv workspace
```

**Data dir** (`/var/lib/botler`): `hub.db`, `repo.git`, `worktrees/`, `sessions/<instance>/`, `volumes/<instance>/`, `secrets.key`. Backup = these files (SQLite online backup API).

## Key tech choices

tokio, axum, sqlx (SQLite, WAL), bollard against the rootless Podman socket (`podman system service`, user systemd unit; Docker also works), git CLI wrapper (more predictable than libgit2 for worktrees and rebase), croner, rmcp (official Rust MCP SDK), serde/toml/schemars, tracing, chacha20poly1305 for secret values at rest, rust-embed, thiserror/anyhow. Web: **Vue 3 + Naive UI** (good dark mode and data tables; pi's `pi-web-ui` is stale), TypeScript types generated from the Rust API types with **ts-rs** (it follows serde's attributes, so tagged enums become discriminated unions). The old dependency-free page (`web/index.html`) stays at `/legacy`. Push: ntfy (self-hostable), with system-written text only.

## Milestones (entire system, in build order)

1. **Foundations.** Restructure the repo, set up the cargo workspace, add CI (fmt, clippy -D warnings, tests). Write `docs/ARCHITECTURE.md` and append DECISIONS 29+ superseding Kestra (9, parts of 4/7/8/10/11), the store model and credentials (13, 24.8). Move the old docs to `docs/legacy`. Verify pi facts in a spike:
   - RPC in a container with an explicit session path and resume;
   - `registerMcpServer` over streamable HTTP with an auth header;
   - an OpenRouter/custom base URL in models config;
   - whether pi-mono still ships web-ui chat components;
   - usage fields in RPC events.
2. **Core + store + repo.** Definition types and schemas, `botler validate`, SQLite migrations (events, timers, schedule_state, deliveries, instances, runs, dialogs, proposals, secrets, usage), bare repo + worktrees, the reconciler skeleton, the scheduler (cron with a missed-run policy, one-off timers), `botler init` from the template.
3. **Agents.** Container driver, pi RPC client, agent base image, botler pi extension, dispatcher (wake/steer/follow-up/coalesce/idle), and MCP tools: `send_message`, `emit_event`, `set_timer`, `cancel_timer`, `spawn_agent`, `list_agents`, `read_session`, `ask_user`, `get_events`. Budgets from usage, with abort. Rate limits on every tool, breakers, the kill switch.
4. **Web app.** Single-user auth (passkey or password + device tokens) over the tailnet. Chat with any instance, a live stream, file/image upload to the scratch space, the `/share` target, the session tree (branch, rewind, labels, resume a branch via pi `fork`/`switch_session`), an inbox of dialogs/approvals (fixed shell), and overviews of agents/jobs/schedules/runs/spend.
5. **Transactions.** `propose_change`, gate job, capability diff, reviewer agent, decision table, approval dialogs, ff-merge, reload, mirror, revert-as-transaction, last-applied-commit fallback, plus the debugging tools `replay_event`, `simulate_triggers`, `get_run_logs` and `validate`.
6. **Jobs, actions, services, secrets.** Trigger→target table (resume/spawn/run), oneshot and service runnables, the injected SDK (+ per-runnable state KV), scheduled job containers, event emission, a webhook endpoint (can ship last within this milestone), `@action` with hub-held secrets + approval, service supervision (Podman event stream, backoff, crash-loop → failed), schedule missed/overlap/timeout policies, incidents + `runnable.failed` routing to the `triage` agent, multi-instance MCP services attached to agents, a secret entry form in the shell, container secrets as env/files.
7. **Push, search, memory.** ntfy push for dialogs. One search across sessions, events, the monorepo and notes: a SQLite FTS index derived from those sources (rebuildable), exposed in the web app and as MCP `search`. Hub-mediated memory (see **Memory**).
8. **First real use (old Phase 0 go/no-go).** You *ask* `main` for a daily digest and an inbound mail channel, and it builds both with only the primitives: triggers, messaging other agents, a resident service. If either needs a core change, that's a finding about the primitives. Later, in the same machinery: apps behind the proxy, browser instances, host workspaces (ground station), Flutter.

## Verification

- `cargo test` covers the core (schedule math, routing, capability diff, decision table, dialog abort) using fake store/runtime/pi adapters. Store tests run against temp SQLite. Repo tests run against temp bare repos.
- `pi-extension` tests run pi in-process via the SDK against a fake hub MCP.
- An end-to-end script (`just e2e`) on a Docker host:
  1. `botler init`, start the hub, chat with `main` through the API.
  2. `main` sets a timer. Kill and restart the hub; the timer still fires and wakes `main` from dormant.
  3. `main` sends a message to a spawned subagent, and the reply routes back.
  4. `main` proposes a change adding a cron job; the gate passes, the reviewer approves, it merges, and the job runs on schedule and emits an event.
  5. An approval-tagged action blocks until it is approved in the web app.
  6. A gate failure returns as feedback.
- Supervision tests:
  - a service that exits is restarted with backoff and raises one incident;
  - a crash-looping service goes `failed`, and `triage` is woken once with logs;
  - reboot the host and the services are back via hub reconcile;
  - a schedule missed during downtime follows its `missed` policy.
- Breaker tests: two agents set to reply to each other trip the cycle breaker. A flooding job trips its rate cap. The kill switch stops everything, and resume restores it.
- Rollback test: merge a change, then revert it; the reconciler returns to the previous schedule set. A broken main at boot leaves the last applied commit running and raises an alarm.
- Upload test: an image attached in the web app is readable by the agent at `/inbox/files/` and described correctly.
- Crash tests: kill the hub mid-delivery and assert exactly one visible delivery in the pi session (idempotency).
