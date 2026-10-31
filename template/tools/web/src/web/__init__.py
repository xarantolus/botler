"""Web tools for agents: `web.search` (a scraper of DuckDuckGo's HTML endpoint, with OpenRouter's
web search as fallback) and `web.fetch` (a page as markdown). They are ordinary monorepo code: when a
site changes and they break, fix them here on a branch (`search.py`, `openrouter.py`, `fetch.py`, with
saved responses in `tests/fixtures/`) and `propose_change`. The `web-health` job checks them daily
against the live web."""
