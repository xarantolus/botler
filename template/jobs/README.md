# jobs/

Deterministic containers (no LLM). Each job is a directory with `job.toml`, a `Dockerfile`, a
`pyproject.toml`, `src/` and `tests/`. `lifetime = "oneshot"` jobs run when triggered;
`lifetime = "service"` jobs run forever and are restarted by the hub. Jobs talk to the hub through
the `botler` SDK (mounted at /opt/botler/sdk): `emit`, `send_message`, `set_timer`, `state`, `health`.

A job's image is built from the repo root, but the build context holds only `jobs/<name>/`,
`packages/`, `tools/`, `connectors/`, `pyproject.toml` and `uv.lock` (at their repo paths, e.g.
`COPY jobs/<name>/ ...`). The image is rebuilt only when one of those changes.
