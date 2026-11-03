# syntax=docker/dockerfile:1
# The botler dev container: Rust, Node, uv/Python, Podman (the hub runs its agents in it) and
# Chromium for the web app's end-to-end runs. `run-container.sh` builds and opens it.
FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive
# The frontend's playwright-core (frontend/package-lock.json), so `npm run e2e` finds its browser.
ARG PLAYWRIGHT_VERSION=1.64.1
ENV LANG=C.UTF-8 LC_ALL=C.UTF-8

# Keep apt's debs so the cache mounts below stay useful.
RUN rm -f /etc/apt/apt.conf.d/docker-clean \
    && echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache

# git, tar and zstd: the hub and its tests. podman and its networking: the hub's containers (as
# root in here; .devcontainer/containers.conf).
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config git curl ca-certificates tar zstd xz-utils unzip file \
        openssh-client less procps jq python3 \
        podman crun conmon fuse-overlayfs netavark aardvark-dns iptables uidmap passt slirp4netns
COPY .devcontainer/containers.conf /etc/containers/containers.conf

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/root/.local/bin:$PATH
RUN curl -fsSL -o /tmp/rustup.sh https://sh.rustup.rs \
    && sh /tmp/rustup.sh -y --no-modify-path --profile minimal --component rustfmt,clippy \
    && rm /tmp/rustup.sh && cargo --version

# Node 24. The setup script is downloaded to a file rather than piped: with `curl | bash` a failed
# download exits 0 and apt quietly installs Ubuntu's older nodejs.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    curl -fsSL -o /tmp/nodesource_setup.sh https://deb.nodesource.com/setup_24.x \
    && bash /tmp/nodesource_setup.sh && rm /tmp/nodesource_setup.sh \
    && apt-get install -y --no-install-recommends nodejs && npm --version

# uv: the template monorepo's gate and the Python SDK's tests.
RUN curl -fsSL -o /tmp/uv.sh https://astral.sh/uv/install.sh \
    && UV_INSTALL_DIR=/usr/local/bin sh /tmp/uv.sh && rm /tmp/uv.sh && uv --version

# Chromium and its libraries for `npm run e2e`.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    npx -y "playwright@${PLAYWRIGHT_VERSION}" install --with-deps chromium

# gh and Claude Code.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    curl -fsSL -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
        https://cli.github.com/packages/githubcli-archive-keyring.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
        > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends gh
RUN curl -fsSL -o /tmp/claude.sh https://claude.ai/install.sh && bash /tmp/claude.sh && rm /tmp/claude.sh \
    && ln -sf /root/.local/bin/claude /usr/local/bin/claude

# The workspace is bind-mounted and host-owned, but we run as root.
RUN git config --global --add safe.directory '*'
