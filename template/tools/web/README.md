# tools/web

Web tools for agents. Each function is decorated with `@botler.tool`, so its type hints are the
argument schema and its docstring the description; `tools.manifest.json` is the agent-facing list and
agents get a tool when their `agent.toml` grants a matching glob (`tools = ["web.*"]`).

- **`web.search`** (`src/web/search.py`, `src/web/openrouter.py`) — search the web. `backend` is
  `duckduckgo` (a scraper of its no-JavaScript HTML endpoint, free but bot-checked after a few
  searches in a row), `openrouter` (its web plugin, about $0.001 per search, needs
  `OPENROUTER_API_KEY`), or `auto` (default: DuckDuckGo, falling back to OpenRouter). Returns
  `{backend, cost_usd, note, results: [{title, url, snippet}]}`.
- **`web.images`** (`src/web/images.py`) — search for image assets. `backend` is `duckduckgo`
  (widest coverage, via its `i.js` API; see the vqd note below), `openverse` (the Creative Commons
  index), `wikimedia` (Wikimedia Commons), or `auto` (default: DuckDuckGo, then Openverse, then
  Wikimedia, skipping a backend that fails). All are keyless. Returns `{backend, note, results:
  [{title, url, thumbnail, width, height, source, license, creator}]}`, where `url` is the full
  image, `thumbnail` a smaller version, `source` the page it came from, and `license`/`creator` are
  filled in when the backend knows them (DuckDuckGo does not).
- **`web.fetch`** (`src/web/fetch.py`) — a URL as readable text: HTML becomes markdown (trafilatura
  extracts the main content), text and JSON pass through, other types are metadata only.

## Failure behaviour

Each backend is tried on its own, so one failing never kills the tool: in `auto` mode a later one is
used and `note` says which were skipped and why. Asking for a single backend reports that backend's
error. Errors are `WebToolError` with a machine-readable `code` (`blocked`, `network`, `http`,
`no_key`, `markup_changed`, ...) and a message written for the next agent.

### DuckDuckGo's vqd

DDG's image API is a two-step scrape: `GET https://duckduckgo.com/?q=<query>&iax=images&ia=images`
returns a page containing a per-query `vqd` token (`vqd="4-..."`, `vqd='4-...'` or `vqd=4-...`), then
`GET https://duckduckgo.com/i.js?l=us-en&o=json&q=<q>&vqd=<vqd>&f=,,,&p=1` with an XHR
`Accept: application/json`, `Referer` and `X-Requested-With` headers. DDG rate-limits and sometimes
serves its bot check: a missing `vqd`, a non-200 or a non-JSON `i.js` is reported as this backend's
failure (code `blocked`) and `auto` falls through to the next backend. It is never retried in a loop.

## Tests

Offline tests run against saved responses in `tests/fixtures/`; no network. When a site or API
changes shape, save the new response over its fixture and fix the parser until the tests pass.
`WEB_LIVE_TESTS=1 uv run pytest tools/web/tests/test_live.py` runs a live smoke test (the search one
costs about $0.001 with an OpenRouter key).

## Keep it healthy

The `web-health` job runs these tools daily against the live web and opens an incident for `triage`
when one breaks. Fixes belong here: update the parser, the fixture and the tests on a branch, run
`gate/run`, and `propose_change`. Regenerate `tools.manifest.json` with
`uv run python -m botler.tools manifest` after changing a tool's signature or description.
