# The build context is the monorepo root. The botler SDK is mounted at run time, not installed.
# `botler/image-browser` is images/browser/ at the same commit, built first by the hub.
FROM botler/image-browser
WORKDIR /app
COPY pyproject.toml uv.lock /app/
COPY packages/shared /app/packages/shared
COPY jobs/browser-example /app/jobs/browser-example
RUN uv sync --frozen --no-dev --package browser-example
