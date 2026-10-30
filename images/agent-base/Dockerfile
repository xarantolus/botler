# botler agent base image: pinned pi + Node + Python/uv + git.
# The botler pi extension and SDK are NOT baked in; the hub bind-mounts them read-only so their
# version always matches the running hub. Monorepo images extend this with `FROM botler/agent-base`.
FROM docker.io/library/node:24-bookworm-slim

ARG PI_VERSION=1.4.0
ARG UV_VERSION=0.9.26

RUN apt-get update \
 && apt-get install -y --no-install-recommends git openssh-client ca-certificates curl python3 python3-venv ripgrep \
 && rm -rf /var/lib/apt/lists/*

# SSH host keys of GitHub and GitLab, pinned from their published fingerprints
# (docs.github.com "GitHub's SSH key fingerprints", docs.gitlab.com "SSH host keys fingerprints"):
# a granted key (a file secret at ~/.ssh/id_ed25519) works with strict host key checking and
# nothing is trusted on first use. Other hosts: add their keys to ~/.ssh/known_hosts.
RUN printf '%s\n' \
    'github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl' \
    'github.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=' \
    'github.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=' \
    'gitlab.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAfuCHKVTjquxvt6CM6tdG4SLp1Btn/nOeHHE5UOzRdf' \
    'gitlab.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBFSMqzJeV9rUzU4kWitGjeR4PWSa29SPqJ1fVkhtj3Hw9xjLVXVYrU9QlYWrOLXBpQ6KWjbjTDTdDkoohFzgbEY=' \
    'gitlab.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCsj2bNKTBSpIYDEGk9KxsGh3mySTRgMtXL583qmBpzeQ+jqCMRgBqB98u3z++J1sKlXHWfM9dyhSevkMwSbhoR8XIq/U0tCNyokEi/ueaBMCvbcTHhO7FcwzY92WK4Yt0aGROY5qX2UKSeOvuP4D6TPqKF1onrSzH9bx9XUf2lEdWT/ia1NEKjunUqu1xOB/StKDHMoX4/OKyIzuS0q/T1zOATthvasJFoPrAjkohTyaDUz2LN5JoH839hViyEG82yB+MjcFV5MU3N1l1QL3cVUCh93xSaua1N85qivl+siMkPGbO5xR/En4iEY6K2XPASUEMaieWVNTRCtJ4S8H+9' \
    > /etc/ssh/ssh_known_hosts

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
