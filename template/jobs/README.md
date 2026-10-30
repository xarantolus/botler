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
in `mcp = [...]` reach its MCP server at `http://<name>:8000/mcp` on the hub's container network.
`[[publish]] port = 8000, host_port = 8080` (optional `bind = "127.0.0.1"`, default `0.0.0.0`)
makes it reachable from the user's network at `<host>:8080`; that needs the user's approval, and
host ports must be 1024 or above. Agents can also run stdio MCP servers in their own container:
`[mcp_stdio.<name>] command = [...]` in `agent.toml`.

A job's image is built from the repo root, but the build context holds only `jobs/<name>/`,
`packages/`, `tools/`, `connectors/`, `pyproject.toml` and `uv.lock` (at their repo paths, e.g.
`COPY jobs/<name>/ ...`). The image is rebuilt only when one of those changes.
