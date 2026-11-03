# The agents' browser (job.toml): Playwright MCP on the browser image (images/browser: Chromium on
# a virtual display, the blocklist, noVNC). Bump the version here to update it.
FROM botler/image-browser
ARG PLAYWRIGHT_MCP_VERSION=0.0.83
RUN npm install -g "@playwright/mcp@${PLAYWRIGHT_MCP_VERSION}" && npm cache clean --force \
 && playwright-mcp --version

# The supervisor runs Playwright MCP as a child, smoke-tests the MCP endpoint, and watches the
# browser (job.toml [health]). It is stdlib-only; the botler SDK is mounted at run time.
COPY jobs/browser/supervise.py /app/supervise.py
