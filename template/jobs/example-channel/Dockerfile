# The build context is the monorepo root. The botler SDK is mounted at run time, not installed.
FROM botler/agent-base:latest
WORKDIR /app
COPY pyproject.toml uv.lock /app/
COPY packages/shared /app/packages/shared
COPY jobs/example-channel /app/jobs/example-channel
RUN uv sync --frozen --no-dev --package example-channel
