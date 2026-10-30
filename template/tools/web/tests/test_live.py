"""Live smoke test against the real web. The gate does not run it (no network expectation); run it
with `WEB_LIVE_TESTS=1 uv run pytest tools/web/tests/test_live.py` after changing the scrapers.
With OPENROUTER_API_KEY set, the search costs about $0.001."""

import os

import pytest
from web.fetch import fetch
from web.search import search

pytestmark = pytest.mark.skipif(
    os.environ.get("WEB_LIVE_TESTS") != "1", reason="live network test; set WEB_LIVE_TESTS=1 to run it"
)


def test_live_search_and_fetch() -> None:
    results = search("python programming language", max_results=5).results
    assert len(results) >= 3
    page = fetch(results[0].url, max_chars=2000)
    assert page.status == 200
    assert page.content
