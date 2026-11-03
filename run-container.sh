#!/usr/bin/env bash
# Build the dev image and open a shell in it; arguments go to `podman run`.
set -euo pipefail

cd "$(dirname "$0")"
NAME=$(basename "$PWD")
IMAGE=$NAME-dev:latest
REPO=/workspaces/$NAME
CLAUDE_STATE="$HOME/claude-state/$NAME"
mkdir -p "$CLAUDE_STATE"

podman build -f .devcontainer/Dockerfile -t "$IMAGE" .

# Its own network namespace (the hub creates a bridge network for its containers in it, which the
# host's namespace would not allow), with the dev hub's and Vite's ports published on localhost.
args=(
  -it --rm
  -p 127.0.0.1:7431:7431 -p 127.0.0.1:5173:5173
  --security-opt label=disable
  -v "$PWD":"$REPO"
  -v "$NAME-cargo":/opt/cargo/registry
  # The container's own build output, apart from the host's target/.
  -v "$NAME-target":"$REPO/target"
  -v "$CLAUDE_STATE":/root/.claude
  -e CLAUDE_CONFIG_DIR=/root/.claude
  -w "$REPO"
)
# Podman inside the container: the hub runs its agents and jobs there (.devcontainer/containers.conf).
args+=(
  -v "$NAME-containers":/var/lib/containers
  --cap-add SYS_ADMIN --cap-add NET_ADMIN
  --security-opt unmask=ALL
  --device /dev/fuse
  --pids-limit 16384
)
[ -d "$HOME/.ssh" ] && args+=(-v "$HOME/.ssh":/root/.ssh)
[ -d "$HOME/.config/gh" ] && args+=(-v "$HOME/.config/gh":/root/.config/gh:ro)

exec podman run "${args[@]}" "$@" "$IMAGE" bash -c 'bash .devcontainer/post-create.sh; exec bash'
