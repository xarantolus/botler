# jobs/

Deterministic containers (no LLM). Each job is a directory with `job.toml`, a `Dockerfile`, a
`pyproject.toml`, `src/` and `tests/`. `lifetime = "oneshot"` jobs run when triggered;
`lifetime = "service"` jobs run forever and are restarted by the hub. Jobs talk to the hub through
the `botler` SDK, which the hub mounts at /opt/botler/sdk and puts on `PYTHONPATH` at run time
(so a job's image build must not install it): `botler.emit(MyEvent(...))`, `send_message`,
`set_timer`, `state.get/set`, `log`, and every other hub operation in `botler.ops`. Failures raise
`botler.BotlerError`. Typed events (`botler.Event` subclasses) live in `packages/`.

A job's image is built from the repo root, but the build context holds only `jobs/<name>/`,
`packages/`, `tools/`, `connectors/`, `pyproject.toml` and `uv.lock` (at their repo paths, e.g.
`COPY jobs/<name>/ ...`). The image is rebuilt only when one of those changes.
