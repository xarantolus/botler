# Claude Code (https://code.claude.com), for jobs that run sessions on the user's subscription
# (jobs/claude-session, skill `claude-code`). Installed at build time from npm, which checks the
# package's integrity; nothing is fetched when it runs. Bump the version here to update it.
FROM botler/agent-base:latest

ARG CLAUDE_CODE_VERSION=2.1.309

RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
 && npm cache clean --force \
 && claude --version

# The image is the update channel. IS_SANDBOX: containers run as (rootless) root, which Claude
# Code otherwise refuses for runs without permission prompts.
ENV DISABLE_UPDATES=1 \
    DISABLE_AUTOUPDATER=1 \
    IS_SANDBOX=1
