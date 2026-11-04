# Install

botler runs on a Linux machine as your own unprivileged user: one binary, one data directory
(`~/.local/share/botler`) and rootless Podman. There is no setup step: the first `botler serve`
creates everything it needs.

## Requirements

- **Linux with rootless Podman 4.x or newer** (`sudo apt install podman`, `sudo dnf install
  podman`, ...). Docker also works (`--runtime docker`), but Podman is the target.
- **subuid/subgid ranges** for your user; most distributions add them when the user is created.
- `git` and `tar`; `zstd` is optional (smaller backups).
- To build it: **Rust 1.90+** ([rustup](https://rustup.rs)) and **Node.js 22+** with npm (the
  build compiles the web app into the binary).
- An [OpenRouter](https://openrouter.ai) API key: agents use models through it, and you set a
  daily budget per agent.

`botler service check` checks the host and tells you what is missing (`botler service install`,
below, runs the same check first).

## Install

```sh
cargo install --locked --git https://github.com/xarantolus/botler botler
```

That puts `botler` in `~/.cargo/bin`. Add `--root <dir>` to put it elsewhere, e.g.
`--root ~/apps/botler` gives `~/apps/botler/bin/botler`.

## Run it

As a user service that starts at boot and keeps running after you log out:

```sh
botler service install
journalctl --user -u botler -f        # the sign-in link is logged here
```

It checks the host first (rootless Podman, subordinate ids, git, lingering) and prints the fix
for anything missing. Then it writes `~/.config/systemd/user/botler.service` for this binary, and
enables and starts it. `botler service uninstall` removes the service again; your data stays.

To try it in the foreground instead, run `botler serve`.

The first start creates `~/.local/share/botler` with your monorepo, builds the agent image
(a few minutes) and logs a sign-in link:

```
no device is signed in yet: open http://127.0.0.1:7420/#code=K7QM2XPA
```

Open it on the same machine, then open **Secrets** and paste your OpenRouter API key into
`OPENROUTER_API_KEY` (create one at [openrouter.ai/keys](https://openrouter.ai/keys)), and say
hello to Botler. The code is valid for 10 minutes;
`botler login-code` prints a new one. Sign in other devices with a code from **Settings →
Devices**.

`systemctl --user stop botler` stops the hub; it stops its agent containers before exiting.
`botler status`, `botler pause` (the kill switch) and `botler resume` work while it runs.

### Configuration (optional)

Everything has a default. `~/.config/botler/config.toml` can set:

```toml
data = "~/.local/share/botler"     # the data directory
listen = "127.0.0.1:7420"          # the web app and API
runtime = "podman"                 # or "docker"
# A git remote the monorepo's main is pushed to after every change, e.g. a private repository
# (pushed with your git and SSH configuration). Used when the monorepo is created; change it
# later in Settings.
mirror = "git@github.com:you/botler-monorepo.git"
```

Flags (`--data`, `--listen`, `--runtime`) and `BOTLER_DATA`, `BOTLER_LISTEN`, `BOTLER_RUNTIME`
override it. Each file in `~/.config/botler/secrets/` sets the secret it is named after on every
start (`OPENROUTER_API_KEY`, or any other secret the monorepo declares); the hub stores secrets
encrypted, and you can also enter them in the web app (**Secrets**) or with
`botler secret set <NAME>`.

## Exposing it privately

The hub listens on `127.0.0.1:7420` (plain HTTP, this machine only). Containers don't use that
port: they reach the hub over its Unix socket (`$XDG_RUNTIME_DIR/botler-<hub id>/hub.sock`, else
`/tmp/botler-<uid>-<hub id>/`, mounted at `/run/botler`),
which works on any network setup, even for containers without network. To use the web app from
your other devices with HTTPS, which the PWA and the share target need, put
[Tailscale](https://tailscale.com) in front of it (enable MagicDNS and HTTPS certificates in the
tailnet admin console first):

```sh
tailscale serve --bg 7420     # https://<host>.<tailnet>.ts.net -> http://127.0.0.1:7420
```

To listen elsewhere, pass `--listen` (or set `BOTLER_LISTEN`), e.g. `--listen 0.0.0.0:7420` for
your LAN. Don't expose the port to the internet.

## Backup and restore

```sh
botler backup ~/botler-$(date +%F).tar.zst            # safe while the hub runs
botler backup --no-key ~/botler-$(date +%F).tar.zst   # without secrets.key
botler backup --full ~/botler-$(date +%F).tar.zst     # also what git ignores (see below)
```

The archive holds the database (an online snapshot), `secrets.key`, the monorepo (`repo.git`),
`memory` (agent memory with its git history), pi sessions, agent workspaces and homes (archived
chats' homes as their packed `homes/<chat>.tar.zst`), and uploads. It leaves out what the hub
rebuilds (exports, worktrees, rendered agent dirs, unpacked assets, images, `search.db`) and run
logs. Inside homes, shared homes and workspaces it also leaves out what git ignores in each git
checkout (`node_modules`, `target`, `.venv`, ...; nested `.gitignore` files and `!` negations
count, files outside a checkout are always kept); `--full` includes them. It is compressed with
zstd if installed, else gzip, and created with mode 0600: **the key plus the database decrypt
every secret**, so store it accordingly.

To restore, stop the hub and unpack into an empty data directory:

```sh
systemctl --user stop botler
mkdir ~/.local/share/botler            # must not exist yet (move the old one aside)
tar -xf botler-2026-10-30.tar.zst -C ~/.local/share/botler
systemctl --user start botler
```

A `--no-key` backup restores without `secrets.key`; the hub makes a new key, so enter every secret
again with `botler secret set <NAME>` (`botler secret list` shows them) and issue a new
`botler token` (tokens are signed with the key).

## Disk space

`botler storage` lists disk use by chat (home, workspace, session) and by kind (shared homes,
images, exports, run logs, search index, memory), largest first; the web app's Storage view shows
the same and can archive a chat, clear a sleeping chat's caches, delete an archived chat for good
and collect garbage. `botler gc` removes images and exports nothing uses and run logs older than
`--logs-older-than` (default `30d`).

Chats dormant longer than `auto_archive_after` (`botler.toml` `[defaults]`, default `30d`,
`"never"` to turn it off) are asked to hand over and archived, at most four per hour; persistent
and always-on agents never are. Archiving deletes what is rebuildable in the chat's home (git-ignored
files, `~/.cache`) and packs the rest into `homes/<chat>.tar.zst`, which needs `zstd`. Messaging
the chat unpacks it before it wakes.

## Upgrading

```sh
botler backup ~/botler-before-upgrade.tar.zst
cargo install --locked --git https://github.com/xarantolus/botler botler   # the same --root as before
systemctl --user restart botler
```

On start the hub migrates the database, unpacks the new version's pi extension and SDK, and
builds the agent base image for the new version (`botler gc` removes images nothing uses any
more). When the new version ships a newer monorepo template, the hub proposes merging it into your
monorepo; it shows up like any other change for you to approve.
