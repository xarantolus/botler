# jobs/

Deterministic containers (no LLM). Each job is a directory with `job.toml`, a `Dockerfile`, a
`pyproject.toml`, `src/` and `tests/`. `lifetime = "oneshot"` jobs run when triggered;
`lifetime = "service"` jobs run forever and are restarted by the hub. Jobs talk to the hub through
the `botler` SDK, which the hub mounts at /opt/botler/sdk and puts on `PYTHONPATH` at run time
(so a job's image build must not install it): `botler.emit(MyEvent(...))`, `send_message`,
`set_timer`, `state.get/set`, `log`, and every other hub operation in `botler.ops`. Failures raise
`botler.BotlerError`. Typed events (`botler.Event` subclasses) live in `packages/`.

Services: an exit is a failure, restarted with exponential backoff (`[restart]`); too many exits in
`crashloop_window` stop it until its incident is resolved. With `[health] heartbeat = "1m"` the
service must call `botler.health.ok()` that often (`health.fail(reason)` restarts it). A change to
its image inputs restarts it; other merges do not. `port = 8000` lets agents that list the service
in `mcp = [...]` reach its MCP server at `http://<name>:8000/mcp` on the hub's container network;
`services = [...]` (or a `service:<name>` grant) reaches anything else it serves there (HTTP,
CDP), listed with its IP in the agent's `/botler/agent-dir/services.json`.
`[[publish]] port = 8000, host_port = 8080` (optional `bind = "127.0.0.1"`, default `0.0.0.0`)
makes it reachable from the user's network at `<host>:8080`; that needs the user's approval, and
host ports must be 1024 or above. Agents can also run stdio MCP servers in their own container:
`[mcp_stdio.<name>] command = [...]` in `agent.toml`.

Before proposing a job or service, `trial_run(job, branch, minutes)` runs it from your branch with
its effects recorded instead of carried out (skill `background-service`). The first deployment of
a service needs the user's approval; `enabled = false` keeps a job defined but never run
(`jobs/example-watcher/` is such a template).

A job or service reaches another service with `services = ["<name>"]`: it joins the services
network and finds it at `http://<name>:<port>` (e.g. the shared browser's MCP at
`http://browser:8000/mcp`, skill `browser`). The first time asks the user.

A job or service that needs a model (to summarize, classify, extract) lists it:
`models = ["default"]` (or OpenRouter ids), and calls `botler.chat(messages, model=...)`. The
hub holds the key, meters each call and stops at the job's `budget` (default
`defaults.budget`). Never put a provider key in a job. Adding a model asks the user.

`[app]` gives a service a web UI the user opens inside botler (skill `apps`).

`channel = true` makes a service a channel: it carries the user's messages from Telegram, email
and the like (skill `channels`, template `jobs/example-channel/`). Every change to a channel needs
the user's approval.

A job's image is built from the repo root, but the build context holds only `jobs/<name>/`,
`packages/`, `tools/`, `connectors/`, `pyproject.toml` and `uv.lock` (at their repo paths, e.g.
`COPY jobs/<name>/ ...`). The image is rebuilt only when one of those changes. `FROM
botler/image-<x>` builds on `images/<x>/` at the same commit (e.g. `images/browser`, a Chromium
desktop: skill `browser-login`, example `jobs/browser-example/`).

An off-the-shelf server (a database, Mattermost, Gitea) needs no Dockerfile: `from =
"docker.io/library/postgres:16-alpine"` (a version tag or digest, never `latest`) runs that image
as it is, with its own command unless `command` is set, and `environment = { KEY = "value" }` for
its plain settings (secrets stay in `secrets` groups). Such an image cannot call
`botler.health.ok()`, so `[health] command = ["pg_isready"]` lets the hub run that check inside the
container instead: each success is a heartbeat, and a failure is retried (three in a row, with
the heartbeat overdue, make it unhealthy). Its volume is handed to the image's own user at
start, so images that do not run as root can write it. Skill `compose-apps` translates a
docker-compose file.

`monorepo = true` (the user approves it once) lets a job's runs clone this monorepo from the hub
(`$BOTLER_MONOREPO_URL`, plain git over HTTP with the run's token) and push branches under
`$BOTLER_PUSH_PREFIX` (`agents/<chat>/`, the chat whose event started the run), which that chat
then proposes. The job needs a `command` and `node` in its image (the hub's forwarder runs it).
