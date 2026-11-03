#!/usr/bin/env bash
# Prepare the workspace after the container starts. Non-fatal by design.
set -uo pipefail

REPO="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO"

(cd frontend && npm ci --no-audit --no-fund) || echo "==> npm ci in frontend/ failed"
(cd pi-extension && npm ci --ignore-scripts --no-audit --no-fund) || echo "==> npm ci in pi-extension/ failed"
cargo fetch --locked || echo "==> cargo fetch failed"

cat <<'TXT'

botler dev container. Common commands:
  cargo test --workspace                          tests (fake runtime)
  cargo run -p botler -- --data /tmp/dev serve --listen 0.0.0.0:7431
                                                  a hub of your own, at http://127.0.0.1:7431 on the host
  cd frontend && BOTLER_DEV_HUB=http://127.0.0.1:7431 npm run dev -- --host 0.0.0.0
  cd frontend && npm run e2e                      end-to-end run with real agents (Podman)
TXT
