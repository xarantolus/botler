"""Offline tests against saved pages (`fixtures/`). When DuckDuckGo or a site changes, save the new
page over the fixture, then fix the parser until these pass."""

import json
from collections.abc import Callable
from pathlib import Path

import httpx
import pytest
import web._http
from web._http import WebToolError
from web.fetch import fetch
from web.openrouter import parse_response
from web.search import parse_page, search, unwrap

FIXTURES = Path(__file__).parent / "fixtures"
# A recorded OpenRouter web-plugin response (Parallel engine, 5 results), contents trimmed.
OPENROUTER = json.loads((FIXTURES / "openrouter_response.json").read_text())
DDG = (FIXTURES / "ddg_results.html").read_text()
ARTICLE = (FIXTURES / "article.html").read_text()
ANOMALY = (FIXTURES / "ddg_anomaly.html").read_text()


def serve(monkeypatch: pytest.MonkeyPatch, handler: Callable[[httpx.Request], httpx.Response]) -> None:
    monkeypatch.setattr(
        web._http,
        "client",
        lambda: httpx.Client(transport=httpx.MockTransport(handler), follow_redirects=True),
    )


def test_parse_results() -> None:
    page = parse_page(DDG)
    assert [r.url for r in page.results] == [
        "https://www.python-httpx.org/",
        "https://pypi.org/project/httpx/",
        "https://www.python-httpx.org/quickstart/",
    ]
    first = page.results[0]
    assert first.title == "HTTPX"
    assert first.snippet.startswith("A next-generation HTTP client for Python. HTTPX is a fully featured")
    assert page.next_form is not None
    assert page.next_form["q"] == "python httpx"
    assert page.next_form["s"] == "10"


def test_unwrap_redirect() -> None:
    href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1&rut=abc"
    assert unwrap(href) == "https://example.com/a?b=1"
    assert unwrap("https://example.com/") == "https://example.com/"


def test_changed_markup_says_where_to_fix() -> None:
    html = DDG.replace("result__a", "result__link")
    with pytest.raises(WebToolError, match=r"tools/web/src/web/search\.py.*propose_change") as e:
        parse_page(html)
    assert e.value.code == "markup_changed"
    with pytest.raises(WebToolError, match="no #links"):
        parse_page("<html><body><p>hello</p></body></html>")


def test_no_results_is_empty() -> None:
    html = (
        '<div id="links"><div class="result result--no-result">'
        '<div class="no-results">No results.</div></div></div>'
    )
    assert parse_page(html).results == []


def test_blocked(monkeypatch: pytest.MonkeyPatch) -> None:
    # DuckDuckGo's real bot-check page (trimmed), served with HTTP 202.
    with pytest.raises(WebToolError, match="bot check") as e:
        parse_page(ANOMALY)
    assert e.value.code == "blocked"
    serve(monkeypatch, lambda _: httpx.Response(202, html=ANOMALY))
    with pytest.raises(WebToolError, match="bot check"):
        search("x", backend="duckduckgo")


def test_search_follows_next_page(monkeypatch: pytest.MonkeyPatch) -> None:
    forms: list[bytes] = []

    def handler(req: httpx.Request) -> httpx.Response:
        forms.append(req.content)
        # The second page repeats the first; duplicates are dropped and paging stops at MAX_PAGES.
        return httpx.Response(200, text=DDG)

    serve(monkeypatch, handler)
    response = search("python httpx", max_results=5, region="de-de", backend="duckduckgo")
    assert response.backend == "duckduckgo"
    assert len(response.results) == 3
    assert b"kl=de-de" in forms[0]
    assert b"s=10" in forms[1]


def test_search_rate_limited(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(monkeypatch, lambda _: httpx.Response(202, text=""))
    with pytest.raises(WebToolError, match="HTTP 202"):
        search("x", backend="duckduckgo")


def test_parse_openrouter_response() -> None:
    results, cost = parse_response(OPENROUTER)
    assert [r["url"] for r in results][:2] == [
        "https://www.britannica.com/technology/Python-computer-language",
        "https://docs.python.org/3/faq/general.html",
    ]
    assert len(results) == 5
    assert results[1]["title"] == "General Python FAQ — Python 3.15.2 documentation"
    assert results[1]["snippet"].startswith("Python is an interpreted, interactive, object-oriented")
    assert all(len(r["snippet"]) <= 404 for r in results)
    assert cost == pytest.approx(0.001018768)
    with pytest.raises(WebToolError, match="openrouter\\.py") as e:
        parse_response({"choices": []})
    assert e.value.code == "markup_changed"


def ddg_blocked_or_openrouter(openrouter: httpx.Response) -> Callable[[httpx.Request], httpx.Response]:
    return lambda req: openrouter if req.url.host == "openrouter.ai" else httpx.Response(202, html=ANOMALY)


def test_openrouter_backend(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-key")
    sent: list[httpx.Request] = []

    def handler(req: httpx.Request) -> httpx.Response:
        sent.append(req)
        return httpx.Response(200, json=OPENROUTER)

    serve(monkeypatch, handler)
    r = search("python programming language", max_results=5)
    assert r.backend == "openrouter" and r.note is None and len(r.results) == 5
    assert r.cost_usd == pytest.approx(0.001018768)
    assert sent[0].headers["authorization"] == "Bearer test-key"
    body = json.loads(sent[0].content)
    assert body["plugins"] == [{"id": "web", "engine": "parallel", "mode": "turbo", "max_results": 5}]
    assert body["messages"][0]["content"] == "python programming language"


def test_openrouter_without_key(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    with pytest.raises(WebToolError, match=r"OPENROUTER_API_KEY.*job\.toml") as e:
        search("x", backend="openrouter")
    assert e.value.code == "no_key"


def test_auto_falls_back_to_duckduckgo(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-key")
    serve(
        monkeypatch,
        lambda req: (
            httpx.Response(402, json={"error": {"message": "Insufficient credits"}})
            if req.url.host == "openrouter.ai"
            else httpx.Response(200, text=DDG)
        ),
    )
    r = search("python httpx", max_results=3)
    assert r.backend == "duckduckgo" and len(r.results) == 3
    assert r.note is not None and "HTTP 402" in r.note and "out of credits" in r.note


def test_auto_reports_both_failures(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-key")
    serve(monkeypatch, ddg_blocked_or_openrouter(httpx.Response(401, text="no auth")))
    with pytest.raises(
        WebToolError, match=r"Both search backends failed\. openrouter: .*HTTP 401.*bot check"
    ):
        search("x")
    # OpenRouter is not consulted when another backend is asked for.
    serve(monkeypatch, ddg_blocked_or_openrouter(httpx.Response(200, json=OPENROUTER)))
    assert search("x").backend == "openrouter"
    with pytest.raises(WebToolError, match="bot check"):
        search("x", backend="duckduckgo")


def test_fetch_html_to_markdown(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(
        monkeypatch,
        lambda req: (
            httpx.Response(301, headers={"location": "https://www.python-httpx.org/async/"})
            if req.url.path == "/old"
            else httpx.Response(200, html=ARTICLE)
        ),
    )
    r = fetch("https://www.python-httpx.org/old")
    assert r.url == "https://www.python-httpx.org/async/"
    assert r.status == 200
    assert r.title is not None and "Async Support" in r.title
    assert "# Async Support" in r.content
    assert "AsyncClient" in r.content
    assert "Material for MkDocs" not in r.content  # footer boilerplate is dropped
    assert not r.truncated


def test_fetch_truncates_and_pages(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(monkeypatch, lambda _: httpx.Response(200, text="0123456789" * 10))
    r = fetch("https://example.com/a.txt", max_chars=30)
    assert r.content == "012345678901234567890123456789"
    assert r.truncated and r.total_chars == 100
    assert r.note is not None and "offset=30" in r.note
    assert fetch("https://example.com/a.txt", max_chars=30, offset=90).content == "0123456789"


def test_fetch_json_passthrough_and_raw(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(monkeypatch, lambda _: httpx.Response(404, json={"error": "nope"}))
    r = fetch("https://example.com/api")
    assert r.status == 404
    assert r.content == '{"error":"nope"}'
    serve(monkeypatch, lambda _: httpx.Response(200, html=ARTICLE))
    assert "<!doctype html>" in fetch("https://example.com/", raw=True).content[:50]


def test_fetch_binary_is_metadata_only(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(
        monkeypatch,
        lambda _: httpx.Response(200, content=b"%PDF-1.7", headers={"content-type": "application/pdf"}),
    )
    r = fetch("https://example.com/x.pdf")
    assert r.content == ""
    assert r.note is not None and "application/pdf" in r.note


def test_fetch_rejects_non_http() -> None:
    with pytest.raises(WebToolError, match="not an http"):
        fetch("file:///etc/passwd")
