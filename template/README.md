# Monorepo

Everything botler runs is defined here. The hub applies `main`; agents change it by pushing a
branch and calling `propose_change` (gate → reviewer → user if needed → merge).

| Path | What |
|---|---|
| `botler.toml` | Instance settings: time zone, models, defaults |
| `secrets.toml` | Secret declarations (values live in the hub, never here) |
| `agents/<name>/` | Agent definitions (`agent.toml`) and their prompts (`AGENTS.md`) |
| `jobs/<name>/` | Deterministic containers: oneshot or long-running services |
| `mounts/` | Network shares (SMB, SFTP, WebDAV) granted to chats and jobs |
| `triggers/` | When something happens, do something |
| `events/` | Declared event types |
| `tools/`, `connectors/`, `packages/` | Python code agents and jobs use |
| `pi/` | pi skills, prompts and extensions loaded into agent sessions |
| `images/` | Container images (`FROM botler/agent-base`, or `FROM botler/image-<x>` to build on `images/<x>/`: the hub builds bases first and rebuilds what builds on a changed one; extend an existing image rather than repeating its setup); the build context is `images/<x>/` plus the uv workspace (`pyproject.toml`, `uv.lock`, `packages/`, `tools/`, `connectors/`), which they install into `/opt/botler/tools-venv` for Python tools |
| `policies/` | The reviewer's policy and what needs the user's approval |
| `gate/` | The deterministic checks every change must pass |

Integrations that ship here: `jobs/claude-session/` runs live Claude Code sessions on your own Claude
subscription that take your messages while they work and message you back (skill `claude-code`; it
needs a token from `claude setup-token`, and a GitHub token if it should push and open PRs).
