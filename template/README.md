# Monorepo

Everything botler runs is defined here. The hub applies `main`; agents change it by pushing a
branch and calling `propose_change` (gate → reviewer → user if needed → merge).

| Path | What |
|---|---|
| `botler.toml` | Instance settings: time zone, model routes, defaults |
| `secrets.toml` | Secret declarations (values live in the hub, never here) |
| `agents/<name>/` | Agent definitions (`agent.toml`) and their prompts (`AGENTS.md`) |
| `jobs/<name>/` | Deterministic containers: oneshot or long-running services |
| `mounts/` | Network shares (SMB, SFTP, WebDAV) granted to chats and jobs |
| `triggers/` | When something happens, do something |
| `events/` | Declared event types |
| `tools/`, `connectors/`, `packages/` | Python code agents and jobs use |
| `pi/` | pi skills, prompts and extensions loaded into agent sessions |
| `images/` | Container images (`FROM botler/agent-base`); the build context is `images/<x>/` plus the uv workspace (`pyproject.toml`, `uv.lock`, `packages/`, `tools/`, `connectors/`), which they install into `/opt/botler/tools-venv` for Python tools |
| `policies/` | The reviewer's policy and what needs the user's approval |
| `gate/` | The deterministic checks every change must pass |
