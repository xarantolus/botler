# Claude Code (https://code.claude.com), for jobs that run sessions on the user's subscription
# (jobs/claude-session, skill `claude-code`). Installed at build time from npm, which checks the
# package's integrity; nothing is fetched when it runs. Bump the version here to update it.
# The GitHub CLI `gh` (pinned, checked against its release checksums) opens PRs with GITHUB_TOKEN.
FROM botler/agent-base:latest

ARG CLAUDE_CODE_VERSION=2.1.309
ARG GH_VERSION=2.102.0
ARG GH_SHA256_AMD64=bb766f710eef8ede859c18578c72c327597cd4c8a85b06001b1f3843c6019386
ARG GH_SHA256_ARM64=7862c86c72f43df3a2d93ddde6f473285b4e2af61b494849846827e513ef6484

RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
 && npm cache clean --force \
 && claude --version

RUN arch="$(dpkg --print-architecture)" \
 && case "$arch" in amd64) sum="$GH_SHA256_AMD64" ;; arm64) sum="$GH_SHA256_ARM64" ;; *) echo "no gh for $arch" >&2; exit 1 ;; esac \
 && curl -fsSL -o /tmp/gh.tar.gz "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_${arch}.tar.gz" \
 && echo "${sum}  /tmp/gh.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/gh.tar.gz -C /usr/local --strip-components=1 "gh_${GH_VERSION}_linux_${arch}/bin/gh" \
 && rm /tmp/gh.tar.gz \
 && gh --version

# The image is the update channel. IS_SANDBOX: containers run as (rootless) root, which Claude
# Code otherwise refuses for runs without permission prompts. GH_NO_UPDATE_NOTIFIER: the same for gh.
ENV DISABLE_UPDATES=1 \
    DISABLE_AUTOUPDATER=1 \
    IS_SANDBOX=1 \
    GH_NO_UPDATE_NOTIFIER=1
