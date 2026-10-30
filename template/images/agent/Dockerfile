# The default agent image. Extend it with tools your agents need.
FROM botler/agent-base:latest

# The Python tools (tools/), with the workspace code they use, installed when the image is built:
# a tool call runs /opt/botler/tools-venv/bin/python and installs nothing. These paths are inputs
# of the image's tag, so changing them rebuilds it; the uv cache mount keeps such rebuilds quick.
COPY pyproject.toml uv.lock /opt/botler/workspace/
COPY packages /opt/botler/workspace/packages
COPY tools /opt/botler/workspace/tools
COPY connectors /opt/botler/workspace/connectors
# Every workspace member here (jobs/ are not copied: they have their own images).
RUN --mount=type=cache,target=/root/.cache/uv \
    cd /opt/botler/workspace \
 && UV_PROJECT_ENVIRONMENT=/opt/botler/tools-venv UV_LINK_MODE=copy uv sync --frozen --no-dev --no-editable \
    $(python3 -c 'import glob, tomllib; print(" ".join("--package " + tomllib.load(open(p, "rb"))["project"]["name"] for p in sorted(glob.glob("*/*/pyproject.toml"))))') \
 && rm -rf /opt/botler/workspace
