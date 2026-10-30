# The dev image: the agent image plus build and test tooling for the monorepo.
FROM botler/agent-base:latest
RUN apt-get update && apt-get install -y --no-install-recommends build-essential && rm -rf /var/lib/apt/lists/*
