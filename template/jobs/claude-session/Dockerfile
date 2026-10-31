# The build context is the monorepo root. The botler SDK is mounted at run time, not installed.
# `botler/image-claude-code` is images/claude-code/ (Claude Code, pinned) at the same commit.
FROM botler/image-claude-code
WORKDIR /app
COPY pyproject.toml uv.lock /app/
COPY packages/shared /app/packages/shared
COPY jobs/claude-session /app/jobs/claude-session
RUN uv sync --frozen --no-dev --package claude-session
