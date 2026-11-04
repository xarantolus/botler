# botler
botler is a self-hosted personal assistant that extends itself. You talk to **Botler** in a web
app (or on Discord, Mattermost, or wherever else it builds a channel to). When you ask for
something it can't do yet, like a scraper, a daily digest, a bot or a small app, an agent writes
it as code in your own git repository. The code is tested, reviewed by a second agent and, where it
matters, approved by you before it runs.

It is one Rust binary on a Linux machine you own. Agents are [pi](https://pi.dev) sessions in
rootless Podman containers, and models come through [OpenRouter](https://openrouter.ai).

### Features
- Chat with Botler from the web app on your desktop or phone (installable as a PWA, with push
  notifications), or over Discord (ready to turn on) and other chat platforms it builds a bridge to
- Larger work goes to a `dev` agent in its own chat, which you can open and message while it
  works; it can also start a Claude Code session on your subscription
- Everything the agents build is plain code in a git monorepo: jobs, services (Dockerfiles),
  schedules, event triggers, tools and agent definitions
- Every change goes through checks (lint, types, tests), a reviewer agent, and your approval when
  it widens access (a new secret, a published port, sending to other people)
- Agents ask you questions with clear options, and you answer from the Requests page
- Secrets that can act on your behalf stay in the hub, every agent has a daily budget, loops
  between agents trip breakers, and there is a kill switch
- Out of the box it can search the web, read pages, drive a real Chromium, keep notes and memory,
  set timers, read and send files, and mount your network shares

Some things it has been used for, mostly built by its own agents from a chat message: a Discord
bot to DM it from your phone, a self-hosted Mattermost with a bridge, an apartment search that
scrapes listing sites daily and posts only the good finds, and tools for querying your own
services.

### Screenshots
|                 Chat                 |               Phone (dark)                |
| :----------------------------------: | :---------------------------------------: |
| ![A chat](docs/screenshots/chat.png) | ![A phone](docs/screenshots/phone.png)    |

|                  A question from an agent                   |                   A proposed change                   |
| :---------------------------------------------------------: | :---------------------------------------------------: |
| ![A question, waiting for you](docs/screenshots/requests.png) | ![A proposed change](docs/screenshots/proposal.png) |

### Warning about LLM-generated code
This project is significantly vibe-coded / created using LLMs. Most of its code has not been
thoroughly reviewed by a human. It also runs AI agents that write and run code on your machine:
the hub limits what they can reach, but read what you approve.

### Setup
You need a Linux machine that stays on (a home server or a small VPS), and an
[OpenRouter](https://openrouter.ai) account with some credit.

1. **Install the requirements.** Rootless [Podman](https://podman.io) 4.x or newer (Docker also
   works), `git`, and to build botler [Rust](https://rustup.rs) 1.90+ and Node.js 22+ with npm.
   For example on Debian or Ubuntu:

   ```sh
   sudo apt install podman git zstd curl nodejs npm
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Install botler.** This builds the hub and its web app, and puts `botler` in `~/.cargo/bin`:

   ```sh
   cargo install --locked --git https://github.com/xarantolus/botler botler
   ```

3. **Give it your OpenRouter API key.** Create a key at
   [openrouter.ai/keys](https://openrouter.ai/keys), then paste it here, press Enter and Ctrl-D
   (you can also skip this; the web app asks for it):

   ```sh
   mkdir -p ~/.config/botler/secrets
   install -m 600 /dev/stdin ~/.config/botler/secrets/OPENROUTER_API_KEY
   ```

4. **Start it as a service.** This checks the host first and prints the fix for anything missing
   (subordinate ids, lingering, ...), then runs botler as a systemd user service that starts at
   boot:

   ```sh
   botler service install
   ```

5. **Sign in.** The first start creates your monorepo in `~/.local/share/botler` and builds the
   agent image, which takes a few minutes. Then it logs a sign-in link:

   ```sh
   journalctl --user -u botler -f
   # no device is signed in yet: open http://127.0.0.1:7420/#code=K7QM2XPA
   ```

   Open it on the same machine and say hello to Botler. `botler login-code` prints a new code if
   it ran out (they last 10 minutes).

6. **Use it from your phone (optional).** The hub only listens on `127.0.0.1`. To reach it from
   your other devices over HTTPS (which the PWA and notifications need), put
   [Tailscale](https://tailscale.com) in front of it, after enabling MagicDNS and HTTPS
   certificates in the tailnet admin console:

   ```sh
   tailscale serve --bg 7420
   ```

   Then open `https://<host>.<tailnet>.ts.net` on your phone and sign it in with a code from
   **Settings → Devices**. Don't expose the port to the internet.

7. **Ask for things.** For example "remind me tomorrow at 8 to call the landlord", "send me a short
   briefing every weekday morning", or "set up the Discord bot": ready integrations like Discord
   live in `extras/` and Botler walks you through their account steps.

`botler status`, `botler pause` (the kill switch) and `botler resume` work while it runs;
`botler service uninstall` removes the service and keeps your data. Configuration, backups, disk
space and private access are explained in [`docs/INSTALL.md`](docs/INSTALL.md).

### Updates
Back up, install the new version the same way, and restart:

```sh
botler backup ~/botler-before-upgrade.tar.zst
cargo install --locked --git https://github.com/xarantolus/botler botler
systemctl --user restart botler
```

The hub migrates its database and rebuilds the agent image on start. When the new version ships
changes to the monorepo template, Botler proposes merging them, and you approve them like any
other change.

### How it works
- **The hub** (`botler serve`) is the only fixed part: it wakes agents for messages, runs
  schedules and timers, supervises services, holds secrets and budgets, and serves the web app.
- **The monorepo** is everything else: agent definitions and prompts, jobs and services, triggers,
  tools and skills. Agents change it on a branch and call `propose_change`; after the checks, the
  review and your approval where needed, the hub applies `main`.

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) has the full design and
[`docs/PRIMITIVES.md`](docs/PRIMITIVES.md) the few concepts everything is built from.

### Development
`./run-container.sh` builds the dev container (Rust, Node, uv, Podman for the hub's agents and
Chromium for end-to-end runs; `.devcontainer/` has the same for editors) and opens a shell in it.
Inside it, or on a host with the same tools:

```sh
cargo test --workspace                                   # unit + integration tests (fake runtime)
BOTLER_PODMAN_TESTS=1 cargo test -p botler-runtime       # also against a real Podman
(cd frontend && npm test && npm run e2e)                 # web app; e2e drives a throwaway hub
(cd pi-extension && npm ci --ignore-scripts && npm run typecheck)
```

| Path | What |
|---|---|
| `crates/` | the hub: `botler-core` (pure domain logic), `botler-store` (SQLite), `botler-repo` (the monorepo), `botler-runtime` (containers, pi RPC), `botler-hub` (the `botler` binary) |
| `frontend/` | the web app (Vue 3, Nuxt UI), compiled into the binary |
| `pi-extension/` | the pi extension every agent session loads |
| `python/botler-sdk/` | the Python SDK jobs and tools use |
| `template/` | the monorepo a new install starts from |
| `images/agent-base/` | the container image every agent builds on |

The TypeScript types for the hub's API are generated from the Rust types: after changing them, run
`BOTLER_UPDATE_BINDINGS=1 cargo test -p botler --test bindings`.

### Limitations
- Linux only, and built for one user: there are no accounts, every signed-in device is you.
- Models come through OpenRouter (or another OpenAI-compatible endpoint set in `botler.toml`), and
  you pay for what the agents use; daily budgets per agent cap it.
- There are no prebuilt binaries: installing and updating builds from source, which takes a while.
- The web app is meant for a private network (Tailscale or your LAN), not the open internet.
- Agents can make mistakes. Changes that widen access wait for you, but approving them is on you.

### License
botler is licensed under the [MIT License](LICENSE).
