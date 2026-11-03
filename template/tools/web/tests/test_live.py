"""Live smoke test against the real web. The gate does not run it (no network expectation); run it
with `WEB_LIVE_TESTS=1 uv run pytest tools/web/tests/test_live.py` after changing the scrapers.
With OPENROUTER_API_KEY set, the search costs about $0.001."""

import os

import pytest
from web._http import WebToolError
from web.fetch import fetch
from web.images import BackendChoice, images
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


def test_live_images() -> None:
    # Wikimedia is the steadier keyless backend, and `auto` must work through at least one of them.
    backends: tuple[BackendChoice, ...] = ("wikimedia", "auto")
    for backend in backends:
        results = images("butterfly", max_results=3, backend=backend).results
        assert len(results) >= 1, backend
        assert all(r.url.startswith("http") for r in results), backend
    # DuckDuckGo and Openverse have the better coverage but rate-limit/are flaky; a failure is not
    # fatal. Watch the stdout for which backend `auto` picked.
    for backend in ("duckduckgo", "openverse"):
        try:
            assert len(images("butterfly", max_results=3, backend=backend).results) >= 1
        except WebToolError:
            pass
