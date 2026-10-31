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
- **Front door = `botler` + topic threads.** `botler` (shown as "Botler", pinned first in the web app; `botler_core::FRONT_DOOR`) is one long-lived, compacting session that acts as router. Bigger topics get their own persistent instances (threads), which you can open directly and `botler` can hand off to. It was called `main` until agents kept confusing it with the main branch: `main` now only ever means the monorepo branch. At boot the hub migrates an old data dir: a hub commit moves `agents/main/` to `agents/botler/` (retargeting `resume = "main"` triggers, only if that adds no validation errors), and instance `main` is renamed with its session, home and clone directories and every live SQLite reference (deliveries, timers, dialogs, proposals, usage, state, breakers, incidents, schedule state, unrouted events). Routed events keep the old name as history.
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

**Agent lifecycle.** An *agent definition* (repo) has *instances* (SQLite rows). `botler` is a persistent instance. Subagents are ephemeral instances spawned via MCP `spawn_agent(def, task)`. Every agent may spawn (it is a base capability, not an escalation); each instance records its parent and spawn depth, and `spawn_agent` is refused beyond `max_spawn_depth` (`botler.toml [defaults]`, default 8) with a hint to do the work itself or report back. An instance is either dormant (session file + home only) or running (container + `pi --mode rpc --session <file>`). The dispatcher wakes a dormant instance for each pending delivery: it starts the container on the **current** image and definition, resumes the session, and prompts with the coalesced deliveries. It uses `steer`/`follow_up` when the agent is busy. Idle timeout makes the instance dormant again. A reboot loses nothing: alarms live in the hub.

**Agent filesystem: one home.** Agent containers are removed when the agent sleeps, so only mounts persist. Every instance gets one persistent host directory, its **home**, mounted read-write at `/home/agent`. It is `$HOME` and the working directory, so git config, `~/.cache`, uv/pip/npm caches, cloned repos and notes all land there and survive sleeping, restarts and archiving. `/tmp` and everything else in the container is wiped on every sleep, and the core prompt says so.
- **Key:** by default the home is the instance's own (`<data>/homes/<instance>`). A definition can name a shared home (`home = "groundstation"`, `<data>/shared-homes/<name>`): every instance of every definition naming it uses the same directory. Shared homes live in their own directory so a name can never collide with an instance id.
- **Workspace inside the home:** `workspace = "monorepo"` agents get their own monorepo clone (`<data>/workspaces/<instance>`) mounted at `/home/agent/monorepo`. It stays a separate host directory because it is per instance even when the home is shared (each instance works on its own branch). `workspace = "volume"` and `"none"` now both mean "home only": the home *is* the private persistent volume. So there is one obvious place (`~`), with the clone in it.
- **Why not `/root`:** agents run as container root (rootless Podman maps it to the hub's user, no keep-id), but `HOME` points at `/home/agent`, an ordinary directory the hub owns per agent. pi's own agent dir (`PI_CODING_AGENT_DIR`) is unaffected.
- **Session cwd:** pi resumes a session in the working directory recorded in its header, and refuses to start if it is missing. When the hub wakes an instance whose session header names another directory (e.g. `/workspace` from before homes), it rewrites that header field to `/home/agent`.
- **Migration:** at boot, `scratch-spaces/chat-<instance>` moves to `homes/<instance>` and `scratch-spaces/<name>` to `shared-homes/<name>`; a volume agent's old `workspaces/<instance>` moves into its home as `workspace/` on its next wake. Definitions still using `scratch =` fail validation with an error naming `home`.

**Local time.** One time zone drives cron triggers, agents' clocks and containers: `botler.toml`'s `timezone`, or else the host's, detected once at startup (`TZ`, then the `/etc/localtime` symlink target under `zoneinfo/`, then `/etc/timezone`, then UTC with a warning) and passed to the definitions loader as its default; `botler status` shows it and where it came from.
- **Agents' clock:** the system prompt stays time-free (prompt caching). Each delivery envelope carries the current local time (`at`, e.g. `Fri 2026-10-30 17:05 Europe/Berlin`) and, more than an hour after the instance's previous delivery, a `gap` note (`first message in 3 days`); the extension renders both at the start of the header, notes included.
- **Containers** (agents, reviewers, jobs, services, gates) get `TZ=<zone>` and the host's zone file mounted read-only at `/etc/localtime` and at `/usr/share/zoneinfo/<zone>`: `TZ` alone falls back to UTC in images without tzdata, and `/etc/localtime` alone is ignored by Node. Without a host zone file they get `TZ` only (warned once).

**Triggers → targets (one model for everything that "starts something").**
- **Triggers** (declared in definitions, except timers):
  - `schedule` (cron/interval, in a file);
  - `timer` (a one-off, runtime row in SQLite);
  - `on` (an event subscription with a filter);
  - `webhook`: `POST /hooks/<name>`, outside user auth, verified with the trigger's `webhook_secret` (`Authorization: Bearer <secret>`, or an HMAC-SHA256 of the body in `X-Hub-Signature-256: sha256=<hex>` or `X-Signature-256`), at most 1 MiB, rate limited per webhook (60/min). The body (parsed JSON, else text) becomes `webhook.received` with `provenance=external`, dispatched to the trigger's target.
- **Targets** (what a trigger does):
  1. `resume`: deliver a message into an existing chat/instance (e.g. `botler`, or a named thread).
  2. `spawn`: start a fresh agent instance from an agent definition and environment (image, mounts, secrets) with a task prompt. It reports back via events and goes dormant or ends.
  3. `run`: run a deterministic Python job in a container. No LLM. It decides itself whether to emit events, which can then trigger 1 or 2.
- **Runnables have a lifetime**:
  - `oneshot`: run to completion, triggered.
  - `service`: long-lived forever. The hub supervises it with restart and backoff, health checks and logs. For frequent pollers, MCP servers, and later apps. A service is just a job whose trigger is "always".
- Every trigger→target pair is a row the reconciler derives from definitions@main. A schedule or webhook never owns logic itself; it points at a target.

**Injected botler SDK (how containers reach us).** Every container the hub starts (jobs, services and agents) gets:
- `BOTLER_URL` and a short-lived, per-run, role-scoped `BOTLER_TOKEN`;
- the pinned SDK (`python/botler-sdk`, embedded in the hub binary) bind-mounted read-only at `/opt/botler/sdk`, with `PYTHONPATH` set. `import botler` is a stdlib-only client of `POST /api/v1/ops/{name}`, so it works in any image without being in the job's dependencies, and the hub version and SDK version always match. It is deliberately not a uv dependency of the monorepo (a path dependency would pin `uv.lock` to one checkout location): pyright finds it through `extraPaths = ["/opt/botler/sdk"]`, Python through `PYTHONPATH`, and packages that declare events or tools depend on `pydantic` themselves.

The SDK surface:
- `botler.ops.<op>(...)`: every hub operation, **generated** from the hub's op registry `ops::OPS` (`tests/sdk.rs` fails when `botler/_ops.py` is stale; `BOTLER_BLESS=1` rewrites it);
- `emit(type, payload)` or `emit(EventInstance)`, `send_message(to, body)`, `set_timer(prompt, at=|after=)`;
- `state.get/set` (a small per-runnable KV in SQLite for poller cursors and dedup keys);
- `log`;
- hub failures raise `BotlerError(code, status, message)`;
- typed events: `botler.Event` (pydantic, `type: ClassVar[str]`); `python -m botler.events manifest` writes `events.manifest.json`;
- `health.ok()/fail(reason)` (service heartbeats, the `health` op);
- later: the `@action` decorator for connectors.

Agents reach the same hub endpoints via MCP. Scripts the agent writes inside its container can also use the SDK.

**Supervision (hub is the single supervisor).**
- **Why not Podman's restart policy:** containers are started with no Podman restart policy, so there's one restart loop rather than two competing ones, and rootless `unless-stopped` doesn't survive a reboot without `podman-restart.service`.
- **Services** (`services.rs`, crash-only): one stateless `reconcile` compares the applied definitions with the runs table and the runtime. It runs at boot, after every apply, on resume, when a service container dies (the event stream only wakes it; the pass inspects the container itself) and every 5 s.
  - **Restart and backoff** are computed from the runs table, never from memory: every exit of a service is a failure (an incident, kind `exit`/`oom`/`unhealthy`/`start`). With `n` failures in `crashloop_window`, the next start waits `min(2^n s, backoff_max)` after the last one ended.
  - **Crash loop:** at `n >= crashloop_count` it reports `crashloop` (the same open incident, routed again) and stops restarting. The open `crashloop` incident *is* the failed state: `resolve_incident` clears it, failures before it stop counting, and the next pass restarts the service.
  - **Restart on change:** a service is stopped (run `cancelled`, not a failure) when its image's content-addressed tag at the applied commit differs from its run's, so only a change to its inputs restarts it. Services removed from the definitions, or all of them while paused, are stopped.
  - **Health:** `[health] heartbeat, grace`. The service calls the `health` op (`botler.health.ok()`); `runs.heartbeat_at` records it. `now > max(started + grace, heartbeat_at) + heartbeat` is unhealthy, and so is `health(ok=false, reason)`: the hub stops it, reports `unhealthy` and restarts it with backoff.
  - **Token:** a service's run token is long-lived and dies with the run (`Caller::resolve` refuses tokens of finished runs).
  - **Network:** services run on a hub-managed user-defined network `botler-<hub id>` (created idempotently at startup and before each start) with a stable container name and the network alias `<service>`. Agents given a service join that network: `mcp = [...]` for MCP servers (written into pi's `mcp.json` as `http://<service>:<port>/mcp`), `services = [...]` or a `service:<name>` grant for anything else (plain HTTP, Chrome DevTools). Every reachable service is listed with its name, current IP and port in `/botler/agent-dir/services.json`, refreshed on each wake; the IP is there because some clients (Chrome DevTools) reject a `Host` header holding a name. Containers reach the hub itself over its Unix socket, independent of their network (below).
  - **Hub socket:** the hub listens on `127.0.0.1:7420` (web app, API; Tailscale serve in front) and on `<data>/run/hub.sock`. The `run` directory is bind-mounted read-only at `/run/botler` into every container that gets a hub token (`BOTLER_SOCKET`). Agents run pi behind a forwarder (`images/agent-base/forward.mjs`, shipped with the hub and mounted at `/botler/forward`) that relays `127.0.0.1:7420` inside the container to the socket, so `BOTLER_URL`, MCP and git smart-HTTP use `http://127.0.0.1:7420` unchanged; the Python SDK in jobs and services talks to the socket directly (no forwarder needed in arbitrary images). Identity stays the bearer token. Spike experiment 2; verified with a `--network none` container (`tests/socket.rs`).
  - **Published ports:** `[[publish]] port, host_port, bind` (default `0.0.0.0`) is rendered as `-p bind:host_port:port`. A new or moved mapping is an escalation; host ports below 1024 or claimed by two services fail validation; the hub's own port and a port taken at runtime fail the start with a message naming the port (an incident). `botler status` and `GET /api/v1/status` (`services`) list the URLs.
- **Hub identity:** every container and network name carries the hub's id (a hash of its data directory: `botler-<id>-agent-<instance>`, `-run-<run>`, `-svc-<name>`, `-gate-<proposal>`), and every container carries the `botler.data` owner label its images already had. Lists, cleanups and container events are filtered by them, so hubs sharing one Podman never touch each other's containers.
- **Stdio MCP servers:** `[mcp_stdio.<name>] command = [...], env = {...}` on an agent is rendered into its `mcp.json`; pi starts it inside the agent's container. Adding or changing one is an escalation (it runs code there).
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
- **See other agents**: `list_agents`, `get_events(filter)`, and `read_session(instance, since)` (a capability that `botler` holds by default). Plus `send_message` with correlated replies, so "ask each agent what it did today" works.
- **Self-scheduling**: `set_timer(at, target)` and proposing a trigger file (cron → `resume botler` with a prompt). For example, the "daily digest" is an agent-written trigger and prompt.
- **Resident anything**: services can have a persistent volume, any image (including a browser image with a logged-in profile), a hub-class or container secret, and the SDK. "A 24/7 container with my mail provider's web app open, emitting `mail.received` events" is a valid thing an agent builds.

**Uploads & share.** Files and images you attach in the web app land in the upload area (`data/uploads/<id>`). The delivery tells the agent the path, mounted read-only at `/inbox/files/` in its container, and the LLM decides whether to read or view it (pi handles images). A **share-to-botler** endpoint (`/share`: a PWA Web Share Target plus a bookmarklet URL) turns shared text, URLs and files into an event to `botler`. No built-in voice.

**Rate limits & breakers (core, everywhere).**
- Token buckets on every MCP tool and SDK endpoint, per instance/runnable and global. Defaults are in core; agents can tighten them in definitions but only raise them through a transaction that needs your approval.
- Breakers for agent↔agent ping-pong (see **Loop defences**), event storms (per-source rate cap) and wake churn (per-instance wakes/hour). A tripped breaker pauses that source and raises an alarm event to you; you resume it in the web app (`POST /api/v1/breakers/clear`).

**Loop defences between agents.** Agents talking to agents must not keep each other busy. Pure rules in `botler_core::loops`, layered cheapest first:
1. **End-of-turn notice, at most once.** When a spawned agent ends a turn, its parent gets `agent.idle` with the child's final answer (`get_last_assistant_text`), routed like a message from the child, *unless the last thing the child did was `send_message` to its parent* (a `TurnLog` per live agent: tool starts reported by pi and messages sent through the hub, in order; the start of the sending tool itself does not count). A message earlier in the turn followed by more work still produces the notice. The notice keeps the `agent.idle` kind (typed `AgentIdle` payload) so the catalog can tell the parent not to thank the child.
2. **Acknowledgements are notes.** An agent-to-agent message (or end-of-turn notice) that is only an acknowledgement or closing ("thanks, no reply needed", "got it 👍", "will do") is classified deterministically (short, no `?`, every word in an acknowledgement vocabulary, at least one acknowledging word or phrase) and delivered *quiet*: it never wakes a sleeping agent, it waits in the queue and rides along with the next real delivery, and for an awake agent the pi extension adds it with `pi.sendMessage(..., { triggerTurn: false })` (after the current turn if one is running), so no turn starts. Exception: if the recipient's last message to the sender asked a question, even "ok" is the answer and wakes it.
3. **Per-pair breaker.** More than 8 agent-to-agent exchanges between one unordered pair within 10 minutes, with no user message or external (non-trusted) event to either in between, trips breaker `pair:<a>|<b>`. Counts come from the event journal (crash-safe, nothing in memory). Further messages between the pair are *held* (delivery status `held`, nothing lost), `breaker.tripped` (typed `BreakerTripped`: the pair and their last messages) goes to `botler` urgently, and the web app lists it under Questions with a Resume button. Resuming delivers what was held and restarts the count from that moment.
4. The correlation-chain hop limit (`MAX_CHAIN_HOPS = 64`) stays as a coarse backstop; it is high enough for deep agent trees reporting up.
An agent messaging itself is refused with a hint (files in its home, `set_timer`).
- A **kill switch** (web button + `botler pause`) freezes scheduling and dispatch, aborts running pi runs and stops services. Resume is explicit.

**Rollback = git.** Reverting is just a revert commit on main through the same transaction path; the reconciler re-applies the state. If definitions@main fail to load at boot (a broken main, or a newer hub validating more strictly), the hub still comes up, degraded: it applies main's valid part, leaving out the agents, jobs and trigger files with errors (and whatever then refers to them, `defs::load_lenient`), or, when an error is elsewhere (`botler.toml`, secrets, events, policies), the last applied commit (or its valid part). It sends the front door an urgent alarm listing the problems, `/api/v1/status` carries them (`definition_problems`, `skipped_definitions`) and the web app shows them on every chat; the template upgrade is still proposed at boot. The first change that loads in full ends the degraded state. Only when nothing loads at all does it refuse to start.

**Debugging changes before merge.**
- `replay_event(event_id, job, branch?)` re-runs a job against a past event (`/botler/event.json`) with main's or the agent's own branch's definition and image, through the trial machinery: only secret groups the agent holds, its own empty `state`, `emit_event`/`send_message`/`set_timer` recorded instead of carried out, and the outcome as `trial.finished`.
- `preview_change(branch)` returns a human-readable **deployment summary** against main. It covers every trigger in plain language (e.g. "at 07:00 on weekdays, Europe/Berlin") with its next five fire times, subscription matches, runnables added/removed/changed, new secret grants, tools and capabilities, and what will need your approval and why. The same summary is returned by `propose_change` and `botler validate`, and is shown to you in approval dialogs.
- `get_run_logs(run)` and `validate(branch)`.

**Python tools for pi (agent-authored tools without TypeScript).**
- **Where they live:** `tools/<pkg>/` in the monorepo, a uv workspace member with functions decorated `@botler.tool(effects=[...])`, named `<package>.<function>`. Type hints and pydantic models give the JSON schema, and the docstring gives the description.
- **Manifest:** `python -m botler.tools manifest` writes `tools.manifest.json` (committed; the gate runs it with `--check`). That makes tools part of the capability diff, with declared `effects` (e.g. `network`, `fs_write`, `secret:<name>`).
- **Registration:** when starting an agent, the hub renders the manifest entries its `tools` globs grant (`tools = ["github.*"]`) into `/botler/agent-dir/tools.json`. At session start the botler pi extension calls `pi.registerTool` for each, named with dots replaced (`github_issues`).
- **Built with the image:** the agent and dev Dockerfiles copy the uv workspace (`pyproject.toml`, `uv.lock`, `packages/`, `tools/`, `connectors/`) and `uv sync --frozen --no-dev --no-editable` it into `/opt/botler/tools-venv`. Those paths are inputs of the images' content-addressed tags, so a tool or lock change rebuilds them and nothing else does; the hub builds the images of every applied commit in the background (a failed build is an incident), so a wake does not wait. Tool code, like pi resources, comes from the applied main commit, never from the agent's own branch.
- **Execution:** `/opt/botler/tools-venv/bin/python -P -m botler.tools call <name> --manifest /botler/agent-dir/tools.json` runs as a subprocess in the agent's container (no `uv`, nothing installed at call time), with JSON arguments on stdin, `{ok, result | error}` on stdout and a timeout. The result is returned to pi; an error becomes a failed tool result.
- **Render hints:** `@tool(render=Links(...))` (also `Table`, `Markdown`, `KeyValue`, `Image`, `Json`) tells the web app how to show the result; it is the `render` field of the manifest entry (`ToolRender` in botler-core), and the session API lists the hints by pi tool name. Without one, the web app guesses from the result's shape.
- **What still needs approval:** tools needing a hub-class secret are `@action`s instead. Their pi tool calls `invoke_action`, so they go through approval.

**pi packages.** Agents can propose adding community pi packages (skills/extensions) to `pi/`, pinned by version, through a normal transaction. The capability diff flags new extensions, since they run with full process rights inside the container.

**Memory (hub-mediated, concurrent-safe).**
- **Storage:** plain Markdown files under `<data>/memory/`, separate from the monorepo and outside transactions, with a non-bare git repo there for history. There's no gate.
- **Namespaces:** `agents/<definition>/` (shared by every instance of a definition, e.g. every `triage`), `projects/<home>/` (agents whose definition names the shared home `<home>`) and `user/` (the user profile and global facts). Agents write their own definition's namespace, their project's and `user/`; the user writes all; jobs read only.
- **Reading:** an agent's namespaces are bind-mounted read-only at `/memory/self`, `/memory/project` (with a shared home) and `/memory/user`, so `rg` and file tools work. The repo's `.git` sits above the namespace directories and is never mounted. Callers without a mount (the web app, jobs) use `memory_read`, `memory_list` and `memory_history` (`git log`, with authors).
- **Writing:** the hub is the only writer. `memory_write(path, content, expected_sha256?)` (no `expected_sha256` means create-only), `memory_append(path, text)` (no version) and `memory_delete(path, expected_sha256)`. A hub mutex serialises writes; each is a temp file plus rename, then `git add -A && git commit --author=<instance>`. A stale sha returns a typed conflict (HTTP 409, `ApiErrorBody.conflict`) with the current content and sha, so concurrent instances never silently overwrite each other. Paths are `.md` only, with no `..` or dot segments, at most 200 characters and 8 levels deep; files hold at most 64 KiB.
- **In the prompt:** the core prompt says what belongs in memory (durable facts, preferences, decisions) versus the monorepo (code, definitions) and the home (working files). The `INDEX.md` of each of the agent's namespaces (first 4 KiB) is appended to the system prompt file when the container starts. It is read once per wake, so the system prompt is fixed for the whole pi session and stays cacheable; changes show from the next wake. Injecting it per prompt (`before_agent_start`) would change the system prompt mid-session and break prompt caching.
- **Search:** later, through the derived full-text index.

**Search (derived, rebuildable).** `<data>/search.db` is an SQLite FTS5 table `docs(source, ref,
owner, at, title, body)` (porter stemming) plus `cursors`, and holds nothing that is not derived:
deleting it (even while the hub runs) rebuilds it, and backups leave it out. An indexer loop in
`search.rs`, woken by hub activity and every minute, reads incrementally: events after the last
id; each session file from a byte offset (pi files are append-only; a rewritten file, i.e. a new
inode, is read again), every entry rather than only the active branch, so compacted history stays
findable; memory Markdown by mtime; the applied monorepo's text files (< 256 KB), replaced when the
applied commit changes. Documents have a rowid derived from `(source, ref, owner)`, so re-indexing
replaces them. One op, `search(query, sources?, scope?, limit?, raw?)`: the query is plain words
(each quoted) unless `raw` asks for FTS5 syntax, whose errors come back as bad requests; results
are `{source, ref, owner, at, title, snippet}` ranked by bm25 (titles weigh more). Permissions:
the user sees everything; an agent sees its own session, other sessions only with
`read_sessions` (the same rule as `read_session`), memory only in its namespaces
(`agents/<definition>`, `projects/<home>`, `user`), and events and the monorepo like
`get_events`; jobs cannot search. `scope="self"` limits an agent to its own session and memory.
A session hit's `ref` is an entry id: `read_session(instance=owner, around=ref, n)` returns the
messages around it.

**Built-in event catalog.** Every hub event type (`runnable.failed`, `proposal.updated`, `dialog.aborted`, ...) has a catalog entry in core with a summary, payload fields, follow-up tools filled in with the event's values (e.g. `get_incident_history(runnable=...)`, `get_run_logs(run_id=...)`, `replay_event`, `resolve_incident`), and guidance. Deliveries are rendered from it. Failure guidance points the agent at incident history (core fact: every incident stores its resolution) *and* its memory notes, and requires `resolve_incident` with the cause and fix.

**Messaging.** Everything is an event `{id, type, source, provenance(trusted|external|untrusted), to?, correlation?, payload}`. Sources include schedules, timers, job output, MCP `send_message`/`emit_event`, user chat, dialog answers, transaction results and hub alarms (job failed, agent crashed, health check failed). Routing uses direct addressing plus subscriptions declared in agent definitions (`[[on]] type = "connector.failed"`), trigger files, and runtime subscriptions (below).

**The per-chat graph.** Every chat is a node with typed, stored edges the hub acts on and the chat can inspect (`list_connections`) and change within policy. The web app shows them in a **Connections** slideover from the chat header (revoke a grant, remove a subscription, jump to parent or children); the Secrets view lists the chats holding each group.
- **Grants** (`grants` table: holder, kind `secret|mount|service|event`, resource, mode, granted_by, revoked_at): resources a chat holds beyond its definition. They come from `spawn_agent(def, task, env = {secrets, mounts})` (granted at once when the user spawns, when the parent holds the resource itself, or when policy says `auto`; otherwise an access request for the child) or from the turn-ending `request_access("<kind>:<name>", reason)`. That opens a Request of kind `access` (*allow for this chat / always allow / deny*); the answer arrives as `dialog.answered`. `policies/grants.toml` (protected) sets per resource `ask` (default) or `auto`; *always allow* writes it with a hub commit (validated before main moves), and a change making something `auto` is a `GrantAuto` escalation in the change summary. **On every wake** the hub builds the container from `Environment::extend(definition env, grants)`, so secret groups and services are re-applied each time; when a chat's live grants differ from those its running container started with, it is restarted (once its turn is over) before its next delivery. Revoking (user: any grant; an agent: its own) sends the chat an `access.changed` event and takes effect from that restart; an `auto` grant does the same. Mount grants are mounted with rclone inside the container (`mounts.rs`, `images/mounts/botler-mounts`; see `docs/spikes/fuse-mounts.md`): a share that fails is reported as an incident and in the next delivery, never as a failed start. Archiving suspends grants (derived from the instance state) and restoring brings them back, checked against the world as it is then (see below).
- **Runtime subscriptions** (`subscriptions` table): `subscribe(type, filter?, wake = true, note?)`, `unsubscribe(id)`, `list_subscriptions()`. The router matches them after definition triggers and trigger files: never for directly addressed events (messages stay private to their recipient) and never back to the chat that emitted the event; one delivery per chat per event. `wake = false` delivers as a quiet note. Deliveries take the same path as every other one (pair breaker, wake rate limit, hop limit). Subscribing checks the type (unknown: the closest names, as for `emit`), the filter fields against the payload schema, that the chat has not emitted the type itself, and that `restricted = true` types (declared in `events/*.toml` or as a pydantic class variable) are covered by an `event:<type>` grant. Archived chats' subscriptions are suspended.
- **Archived chats and disk space.** After archiving, a storage sweep (every 10 minutes, `storage.rs`) deletes what is rebuildable in the chat's own home and workspace (whatever git ignores in each git checkout, listed by `git ls-files -oi --exclude-standard` with the checkout's config neutralised; `~/.cache`), packs the home into `homes/<chat>.tar.zst` and records the bytes freed in the archive report. Shared homes are never cleaned or packed. Waking unpacks the home first (one lock serialises packing, unpacking and clearing against waking). The same sweep asks chats dormant longer than `[defaults] auto_archive_after` (default `30d`, or `"never"`) to hand over, at most four per hour, and archives without a handover a chat that has not answered an archive request within a day; persistent and always-on agents are exempt. Backups leave out the same git-ignored files (`--full` keeps them). The user can see disk use per chat and kind (`storage_usage`, cached until refreshed), clear a sleeping chat's caches, delete an archived chat for good (home, workspace, session, search entries, pending deliveries and timers; events stay) and collect garbage (`collect_garbage`: unused images and exports, old run logs); these ops are user-only, with `botler storage` and `botler gc` as CLI twins.
- **Restoring reconciles.** Restoring an archived chat (a message to it, the web app, a wake) checks its edges against the current definitions first. A definition that is gone stops the restore with `DefinitionGone`: messages to it are held (`restore:<chat>`) and the web app offers to restore it as a generic `chat` agent or delete it. Grants whose resource no longer passes `grants::check` (secret group, service or event type gone) or whose secret group has missing values, and subscriptions no event type matches any more, are marked `stale` with the reason: kept for the record, never applied or routed, shown greyed out with Remove in Connections. The agent gets a quiet `chat.restored` note listing what changed, and the catalog tells it to ask again (`request_access`, `subscribe`) rather than assume old access.
- **Event types**: `list_event_types(query?)` returns built-in and declared types with description, payload schema, producers (`hub`, or jobs and services declaring `emits = [...]` in `job.toml`, validated against declared types) and subscribers (triggers and runtime subscriptions). They are indexed in search as source `event_type`.
- **Merged changes reach running chats.** When an agent's container starts, the hub records what it is made of (`agent_env::AgentEnv`: image tag, effective environment and container-relevant grants, granted tools and their code's tree ids, the `pi/` tree, `AGENTS.md`, the rest of the definition and its model route; not the memory index, the chat's title or secret values). After every apply (and grant change) it compares that with what the agent would get now. A changed agent is restarted between turns, never during one: the images it needs are built first; an idle agent is stopped once no further merge came for 10 s (a burst restarts it once); a delivery to it restarts it first, so the `proposal.updated` of a merged proposal reaches its author in the new container. It resumes the same session file with the same home and grants (always-on agents at once, others on their next message), and its next delivery is preceded by a quiet `environment.updated` note listing what changed (e.g. `new tools: weather.forecast`). The last environment per chat is kept in kv, so a sleeping chat gets the note when it next wakes.
- **SSH**: the agent base image has `openssh-client` and GitHub's and GitLab's published host keys pinned in `/etc/ssh/ssh_known_hosts`; a secret delivered as a file under `~/.ssh` is written 0600 in a 0700 `~/.ssh`, so a granted key (e.g. group `github-ssh` with `file = "/home/agent/.ssh/id_ed25519"`) works with strict host key checking.

**Approvals and dialogs.** `ask_user(question, ctas, about[], ttl, priority)` creates a dialog. An event touching an `about` target aborts it, and a late answer is rejected (DECISIONS 21). `invoke_action(name, args)` runs an **action**: a job with `lifetime = "action"` and an `args` JSON Schema in its `job.toml`, the only runnable that may hold hub-class secret groups. The hub checks the arguments against the schema, creates a proposal (kind `action`) bound to the arguments' hash and an approval request showing them. On approval it runs the job once with exactly those arguments (`/botler/args.json`, `botler.action_args()` in the SDK) and the hub-class secrets; the last line of output (JSON when it parses) comes back to the invoking chat as `action.result`, as does a decline (with the user's reason), a failure or a timeout. Actions are never triggered or started any other way.

**Web app, API types and the live stream.** The web app (`frontend/`, Vue 3 + TypeScript + Nuxt UI used as a plain Vue library, whose chat components render the transcripts) is built with Vite into `frontend/dist`, which is committed and compiled into the binary (`include_dir`); the hub serves it at `/` with an SPA fallback, and the old stopgap page at `/legacy`. Every JSON shape it reads is a named Rust type (`crates/botler-hub/src/wire.rs` for API responses, store and core records, `live.rs` for WebSocket messages, `botler-core` `payloads.rs` for built-in event payloads as a tagged union). The TypeScript types and a typed route table (`routes.ts`: path, params, query, body and response per route) are **generated** from those types with ts-rs into `frontend/src/generated/`; the frontend never declares API shapes itself. Regenerate with `BOTLER_UPDATE_BINDINGS=1 cargo test -p botler --test bindings`; the same test fails when the committed output is stale, `web::tests::embedded_frontend_is_current` fails when `dist` is older than the frontend sources, and a router test checks that every route in the table is served. `GET /api/v1/live` is a WebSocket: the client authenticates with its first message (`{"type":"auth","token":…}`, so tokens stay out of URLs), then receives every event, agent status change (`waking` with its step, `running`, `dormant`, `failed`, `paused`) and dialog, plus raw pi events of the one chat it `watch`es. The app falls back to polling while the socket is down and reconnects with backoff.

**Monorepo transaction.** An agent works on branch `agents/<instance>/<slug>` in its worktree and calls `propose_change(branch, summary)`. The hub then:
1. Rebases onto main or rejects.
2. Runs the **gate** (a job in the dev image: ruff, format, pyright strict, pytest with no network, and the `events.manifest.json`/`tools.manifest.json` staleness checks; the hub validates every definition with its own parser before the gate runs).
3. Computes the **capability diff** in core: new secrets or grants, mounts, network, approval-tagged actions, protected paths, schedules.
4. Runs the **reviewer**: a fresh ephemeral instance of `agents/reviewer`, with no secrets, a read-only diff, the `policies/reviewer.md` policy, and verdict via MCP `submit_review`.
5. Applies the decision table: reviewer approve + no protected or capability changes → auto-merge; otherwise a dialog to you.
6. Fast-forwards main, then reconciles (image rebuilds, schedule table, services; agents pick up new definitions on their next wake, never mid-turn).
7. Push-mirrors to GitLab/Forgejo if configured. Gate failures go back to the agent as an event.

**Template upgrades.** `refs/botler/template` holds pristine snapshots of the hub's built-in template as a linear history; `botler init` makes main's root commit the first one (older repos bootstrap the ref from main's root, which `init` always made from the template). When the built-in template differs from the newest snapshot, the hub commits a new snapshot at boot and git-merges it into main (3-way, so agent edits survive) on branch `hub/template-<snapshot>`, proposed through the normal transaction on behalf of the front door ("Upgrade botler template files (hub vX)"; it touches protected paths, so you approve it). A conflicting merge is committed with its markers on that branch and handed to the front door to resolve and propose. The branch marks an upgrade as handled; a newer template supersedes an open upgrade. `botler template status` shows the state.

**pi integration (reuse first).**
- Agent base image: pinned pi + Node + Python/uv + git + the **botler pi extension**.
- The extension:
  - registers the hub MCP server (`pi.registerMcpServer`, token from env);
  - adds system-prompt sections via `before_agent_start` (identity, inbox semantics, what needs approval, how to propose changes);
  - renders deliveries as `custom_message` entries with provenance labels;
  - records delivery ids for idempotency;
  - registers the turn-ending hub ops (those with `Op::ENDS_TURN`: `ask_user`, `request_secrets`, `propose_change`, `submit_review`, `handover_ready`) as `model-only` tools that call `POST /api/v1/ops/{name}` and return `terminate: true`, so no model call follows (MCP results cannot end a turn). The hub renders their specs from `OPS` into `turn-tools.json` and hides the MCP duplicates via `toolExposure` in `mcp.json`. A failed call does not end the turn;
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
python/botler-sdk/         Python SDK (stdlib runtime; pydantic events and @tool with manifests)
images/agent-base/, images/job-base/
frontend/                  Vue 3 + TypeScript + Nuxt UI web app; src/generated/ is generated
                           from the Rust API types; dist/ is committed and embedded in the binary
web/                       the old dependency-free page, served at /legacy
template/                  the monorepo template (below)
docs/ARCHITECTURE.md, docs/DECISIONS.md (consolidated), docs/legacy/ (current 4 files)
```

**Template monorepo (instantiated by `botler init`, then personalised by `botler` via transactions)**
```
botler.toml                models routing, defaults, mirror, push, budgets
agents/{botler,chat,reviewer,triage,dev}/agent.toml + AGENTS.md   (image, secrets, mounts, network, mcp,
                           subscriptions [[on]], schedules [[schedule]], idle, budget)
jobs/<name>/job.toml Dockerfile pyproject.toml src/ tests/   (lifetime = oneshot|service;
                           [[schedule]] / [[on]] / [[webhook]] triggers with target = run)
triggers/*.toml            standalone trigger→target bindings (e.g. cron → resume botler,
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

**Data dir** (`/var/lib/botler`): `hub.db`, `repo.git`, `worktrees/` (the hub's own temporary worktrees), `sessions/<instance>/`, `homes/<instance>/`, `shared-homes/<name>/`, `workspaces/<instance>/` (monorepo clones), `secrets.key`. Backup = these files (SQLite online backup API).

## Key tech choices

tokio, axum, sqlx (SQLite, WAL), bollard against the rootless Podman socket (`podman system service`, user systemd unit; Docker also works), git CLI wrapper (more predictable than libgit2 for worktrees and rebase), croner, rmcp (official Rust MCP SDK), serde/toml/schemars, tracing, chacha20poly1305 for secret values at rest, rust-embed, thiserror/anyhow. Web: **Vue 3 + Nuxt UI** (as a plain Vue + Vite library, no Nuxt: one design system with chat elements for messages, reasoning, tool calls and the prompt, plus dashboard layout, tables, forms and overlays; icons bundled at build time so the app works offline; pi's `pi-web-ui` is stale), TypeScript types generated from the Rust API types with **ts-rs** (it follows serde's attributes, so tagged enums become discriminated unions). The old dependency-free page (`web/index.html`) stays at `/legacy`. Push: ntfy (self-hostable), with system-written text only.

## Milestones (entire system, in build order)

1. **Foundations.** Restructure the repo, set up the cargo workspace, add CI (fmt, clippy -D warnings, tests). Write `docs/ARCHITECTURE.md` and append DECISIONS 29+ superseding Kestra (9, parts of 4/7/8/10/11), the store model and credentials (13, 24.8). Move the old docs to `docs/legacy`. Verify pi facts in a spike:
   - RPC in a container with an explicit session path and resume;
   - `registerMcpServer` over streamable HTTP with an auth header;
   - an OpenRouter/custom base URL in models config;
   - whether pi-mono still ships web-ui chat components;
   - usage fields in RPC events.
2. **Core + store + repo.** Definition types and schemas, `botler validate`, SQLite migrations (events, timers, schedule_state, deliveries, instances, runs, dialogs, proposals, secrets, usage), bare repo + worktrees, the reconciler skeleton, the scheduler (cron with a missed-run policy, one-off timers), `botler init` from the template.
3. **Agents.** Container driver, pi RPC client, agent base image, botler pi extension, dispatcher (wake/steer/follow-up/coalesce/idle), and MCP tools: `send_message`, `emit_event`, `set_timer`, `cancel_timer`, `spawn_agent`, `list_agents`, `read_session`, `ask_user`, `get_events`. Budgets from usage, with abort. Rate limits on every tool, breakers, the kill switch.
4. **Web app.** Single-user auth (passkey or password + device tokens) over the tailnet. Chat with any instance, a live stream, file/image upload, the `/share` target, the session tree (branch, rewind, labels, resume a branch via pi `fork`/`switch_session`), an inbox of dialogs/approvals (fixed shell), and overviews of agents/jobs/schedules/runs/spend.
5. **Transactions.** `propose_change`, gate job, capability diff, reviewer agent, decision table, approval dialogs, ff-merge, reload, mirror, revert-as-transaction, last-applied-commit fallback, plus the debugging tools `replay_event`, `simulate_triggers`, `get_run_logs` and `validate`.
6. **Jobs, actions, services, secrets.** Trigger→target table (resume/spawn/run), oneshot and service runnables, the injected SDK (+ per-runnable state KV), scheduled job containers, event emission, a webhook endpoint (can ship last within this milestone), `@action` with hub-held secrets + approval, service supervision (Podman event stream, backoff, crash-loop → failed), schedule missed/overlap/timeout policies, incidents + `runnable.failed` routing to the `triage` agent, multi-instance MCP services attached to agents, a secret entry form in the shell, container secrets as env/files.
7. **Push, search, memory.** ntfy push for dialogs. One search across sessions, events, the monorepo and notes: a SQLite FTS index derived from those sources (rebuildable), exposed in the web app and as MCP `search`. Hub-mediated memory (see **Memory**).
8. **First real use (old Phase 0 go/no-go).** You *ask* `botler` for a daily digest and an inbound mail channel, and it builds both with only the primitives: triggers, messaging other agents, a resident service. If either needs a core change, that's a finding about the primitives. Later, in the same machinery: apps behind the proxy, browser instances, host workspaces (ground station), Flutter.

## Verification

- `cargo test` covers the core (schedule math, routing, capability diff, decision table, dialog abort) using fake store/runtime/pi adapters. Store tests run against temp SQLite. Repo tests run against temp bare repos.
- `pi-extension` tests run pi in-process via the SDK against a fake hub MCP.
- An end-to-end script (`just e2e`) on a Docker host:
  1. `botler init`, start the hub, chat with `botler` through the API.
  2. `botler` sets a timer. Kill and restart the hub; the timer still fires and wakes `botler` from dormant.
  3. `botler` sends a message to a spawned subagent, and the reply routes back.
  4. `botler` proposes a change adding a cron job; the gate passes, the reviewer approves, it merges, and the job runs on schedule and emits an event.
  5. An approval-tagged action blocks until it is approved in the web app.
  6. A gate failure returns as feedback.
- Supervision tests:
  - a service that exits is restarted with backoff and raises one incident;
  - a crash-looping service goes `failed`, and `triage` is woken once with logs;
  - reboot the host and the services are back via hub reconcile;
  - a schedule missed during downtime follows its `missed` policy.
- Breaker tests: two agents set to reply to each other trip the pair breaker; acknowledgements do not wake; a turn ending with a report to the parent sends no second notice. A flooding job trips its rate cap. The kill switch stops everything, and resume restores it.
- Rollback test: merge a change, then revert it; the reconciler returns to the previous schedule set. A broken main at boot leaves the last applied commit running and raises an alarm.
- Upload test: an image attached in the web app is readable by the agent at `/inbox/files/` and described correctly.
- Crash tests: kill the hub mid-delivery and assert exactly one visible delivery in the pi session (idempotency).
