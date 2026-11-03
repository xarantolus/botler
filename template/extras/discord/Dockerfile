# The botler Discord channel (src/discord_bot). The botler SDK is mounted at run time.
FROM botler/image-agent
WORKDIR /app
COPY pyproject.toml uv.lock /app/
COPY jobs/discord /app/jobs/discord
RUN uv sync --frozen --no-dev --package discord-bot
