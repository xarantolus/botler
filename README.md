# botler

A personal, self-hosted system of autonomous agents. One Rust binary (`botler`) runs on a Linux
machine with rootless Podman; agents are [pi](https://pi.dev) sessions in containers; everything
the system does is defined in a git monorepo that the agents maintain themselves.

- `docs/ARCHITECTURE.md` — the design and the plan
- `docs/INSTALL.md` — running it on a host (systemd user unit in `packaging/`), backup, upgrades
- `docs/spikes/` — verified facts about the tools we build on
- `crates/` — the hub: `botler-core` (pure domain logic), `botler-store` (SQLite runtime state),
  `botler-repo` (the monorepo), `botler-runtime` (containers, pi RPC), `botler-hub` (the `botler` binary)
- `template/` — the monorepo that `botler init` creates
- `images/agent-base/` — the container image every agent builds on

Status: under construction (see the milestones in `docs/ARCHITECTURE.md`).

```sh
cargo test --workspace                                   # unit + integration tests (fake runtime)
BOTLER_PODMAN_TESTS=1 cargo test -p botler-runtime       # also against a real Podman
(cd pi-extension && npm ci --ignore-scripts && npm run typecheck)
```
