# Install

botler runs on any Linux host as your own unprivileged user: one binary, one data directory
(default `~/.local/share/botler`, or `$XDG_DATA_HOME/botler`; override with `--data` or
`BOTLER_DATA`) and rootless Podman.

## Requirements

- **Rootless Podman 4.x or newer** (developed on 4.9). `podman info --format '{{.Host.Security.Rootless}}'`
  must print `true`. Docker also works (`botler serve --runtime docker`), but Podman is the target.
- **subuid/subgid ranges** for your user, needed for the agent base image:
  ```sh
  grep "^$USER:" /etc/subuid /etc/subgid      # both must list a range
  # if not:
  sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 "$USER"
  podman system migrate
  ```
- **Lingering**, so your user services keep running after you log out and start at boot:
  `sudo loginctl enable-linger "$USER"`.
- `git`, `tar`, `curl` (for ntfy push) and (optionally) `zstd` on `PATH`. To build: Rust 1.90 or newer.

## Steps

```sh
# 1. Build and install to ~/.local/bin/botler
cargo install --locked --path crates/botler-hub --root ~/.local

# 2. Create the data directory and the monorepo
botler init

# 3. Enter your OpenRouter key (read from stdin: paste, then Enter and Ctrl-D)
botler secret set OPENROUTER_API_KEY

# 4. Run the hub as a user service
mkdir -p ~/.config/systemd/user
cp packaging/botler.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now botler
journalctl --user -u botler -f              # first start builds the agent base image (minutes)

# 5. A user token for the web app and API (default 90 days; --days N)
botler token
```

On the host itself, open `http://127.0.0.1:7420` and paste the token; from other devices, see
below. The unit runs
`~/.local/bin/botler serve --data ~/.local/share/botler`; edit `ExecStart` if you use another data
directory. On `systemctl --user stop botler`, the hub stops its agent containers before exiting.

`botler status`, `botler pause` (kill switch) and `botler resume` work while the hub runs.

## Exposing it privately

The hub listens on `127.0.0.1:7420` (plain HTTP, this machine only). Containers don't use that
port: they reach the hub over its Unix socket (`<data>/run/hub.sock`, mounted at `/run/botler`),
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
```

The archive holds the database (an online snapshot), `secrets.key`, the monorepo (`repo.git`),
`memory` (agent memory with its git history), pi sessions, agent workspaces and homes, and
uploads. It leaves out what the hub
rebuilds (exports, worktrees, rendered agent dirs, unpacked assets, images) and run logs. It is
compressed with zstd if installed, else gzip, and created with mode 0600: **the key plus the
database decrypt every secret**, so store it accordingly.

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

## Upgrading

```sh
botler backup ~/botler-before-upgrade.tar.zst
git pull && cargo install --locked --path crates/botler-hub --root ~/.local
systemctl --user restart botler
```

On start the hub migrates the database, unpacks the new version's pi extension and SDK, and
builds the agent base image for the new version (tagged with it; old tags can be removed with
`podman image prune`).
