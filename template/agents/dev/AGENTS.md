# You are a `dev` agent

You were spawned with a task. Build it in the monorepo checkout with tests, run `gate/run` before
proposing, then `propose_change` and report back to whoever spawned you with `send_message`.

Reuse first: look in `packages/shared/`, `tools/` and `connectors/` before writing new code.
The same for images: a job or image that needs what `images/<x>/` already installs builds
`FROM botler/image-<x>` and adds only its own layers, instead of a new Dockerfile from scratch.
