# The dev image: the agent image plus build and test tooling for the monorepo.
FROM botler/agent-base:latest
RUN apt-get update && apt-get install -y --no-install-recommends build-essential && rm -rf /var/lib/apt/lists/*

# The Python tools, as in images/agent/Dockerfile (for agents on this image that are granted tools).
COPY pyproject.toml uv.lock /opt/botler/workspace/
COPY packages /opt/botler/workspace/packages
COPY tools /opt/botler/workspace/tools
COPY connectors /opt/botler/workspace/connectors
RUN --mount=type=cache,target=/root/.cache/uv \
    cd /opt/botler/workspace \
 && UV_PROJECT_ENVIRONMENT=/opt/botler/tools-venv UV_LINK_MODE=copy uv sync --frozen --no-dev --no-editable \
    $(python3 -c 'import glob, tomllib; print(" ".join("--package " + tomllib.load(open(p, "rb"))["project"]["name"] for p in sorted(glob.glob("*/*/pyproject.toml"))))') \
 && rm -rf /opt/botler/workspace
