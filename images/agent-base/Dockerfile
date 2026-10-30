# botler agent base image: pinned pi + Node + Python/uv + git.
# The botler pi extension and SDK are NOT baked in; the hub bind-mounts them read-only so their
# version always matches the running hub. Monorepo images extend this with `FROM botler/agent-base`.
FROM docker.io/library/node:24-bookworm-slim

ARG PI_VERSION=1.4.0
ARG UV_VERSION=0.9.26

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl python3 python3-venv ripgrep tini \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent@${PI_VERSION} \
 && npm cache clean --force

RUN curl -LsSf https://astral.sh/uv/${UV_VERSION}/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh

# Fixed paths the hub mounts into every agent container.
#   /botler/agent-dir   rendered pi config (models.json, mcp.json, settings.json, AGENTS.md)  ro
#   /botler/extension   botler pi extension                                                    ro
#   /opt/botler/sdk     botler Python SDK                                                      ro
#   /botler/sessions    this instance's pi session files                                       rw
#   /workspace          monorepo worktree or private volume                                    rw
#   /inbox/files        uploads delivered to this instance                                     ro
ENV PI_CODING_AGENT_DIR=/botler/agent-dir \
    PI_TELEMETRY=0 \
    PI_SKIP_VERSION_CHECK=1 \
    PYTHONPATH=/opt/botler/sdk \
    HOME=/root

# Agents run as container root. Under rootless Podman that is the unprivileged host user, so bind-
# mounted session files and worktrees stay owned by the user who runs the hub (no keep-id needed).
RUN mkdir -p /botler /workspace /inbox
WORKDIR /workspace
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["pi", "--mode", "rpc"]
