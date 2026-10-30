"""Web tools for agents: `web.search` (scrapes DuckDuckGo's HTML endpoint) and `web.fetch` (a page as
markdown). They are ordinary monorepo code: when a site changes and they break, fix them here on a
branch (`search.py`, `fetch.py`, with saved pages in `tests/fixtures/`) and `propose_change`. The
`web-health` job checks them daily against the live web."""
