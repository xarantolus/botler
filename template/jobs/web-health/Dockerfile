# The build context is the monorepo root. The botler SDK is mounted at run time, not installed.
FROM botler/agent-base:latest
WORKDIR /app
COPY pyproject.toml uv.lock /app/
COPY tools/web /app/tools/web
COPY jobs/web-health /app/jobs/web-health
RUN uv sync --frozen --no-dev --package web-health
