# botler agent base image: pinned pi + Node + Python/uv + git.
# The botler pi extension and SDK are NOT baked in; the hub bind-mounts them read-only so their
# version always matches the running hub. Monorepo images extend this with `FROM botler/agent-base`.
FROM docker.io/library/node:24-bookworm-slim

ARG PI_VERSION=1.4.0
ARG UV_VERSION=0.9.26

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl python3 python3-venv ripgrep \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent@${PI_VERSION} \
 && npm cache clean --force

RUN curl -LsSf https://astral.sh/uv/${UV_VERSION}/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh

# Fixed paths the hub mounts into every agent container.
#   /botler/agent-dir   rendered pi config (models.json, mcp.json, tools.json, AGENTS.md)  ro
#   /botler/extension   botler pi extension                                                    ro
#   /botler/applied     applied main commit, when the agent is granted Python tools            ro
#   /opt/botler/sdk     botler Python SDK                                                      ro
#   /botler/sessions    this instance's pi session files                                       rw
#   /home/agent         the agent's persistent home ($HOME, working directory)                 rw
#   /home/agent/monorepo  the agent's monorepo clone (workspace = "monorepo")                  rw
#   /inbox/files        uploads delivered to this instance                                     ro
# Everything else (including /tmp) is wiped when the agent sleeps: containers are removed.
ENV PI_CODING_AGENT_DIR=/botler/agent-dir \
    PI_TELEMETRY=0 \
    PI_SKIP_VERSION_CHECK=1 \
    PYTHONPATH=/opt/botler/sdk \
    HOME=/home/agent

# Agents run as container root. Under rootless Podman that is the unprivileged host user, so bind-
# mounted session files and homes stay owned by the user who runs the hub (no keep-id needed).
# HOME is not /root: it is an ordinary directory the hub mounts and keeps per agent.
RUN mkdir -p /botler /home/agent /inbox
WORKDIR /home/agent
# No init here: the hub starts every container with `--init`, whose init is PID 1 and reaps zombies.
CMD ["pi", "--mode", "rpc"]
