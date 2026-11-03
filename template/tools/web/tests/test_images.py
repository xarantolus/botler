"""Offline tests for `web.images` against saved responses (`fixtures/ddg_images.html`,
`fixtures/ddg_images.json`, `fixtures/openverse_images.json`, `fixtures/wikimedia_images.json`).
When a backend's markup/data changes, save the new response over its fixture and fix the parser.
The opt-in live smoke test lives in `test_live.py` (`WEB_LIVE_TESTS=1`)."""

import json
from collections.abc import Callable
from pathlib import Path

import httpx
import pytest
import web._http
from web._http import WebToolError
from web.images import images, parse_duckduckgo, parse_openverse, parse_wikimedia

FIXTURES = Path(__file__).parent / "fixtures"
DDG_HTML = (FIXTURES / "ddg_images.html").read_text()
DDG_IMAGES = json.loads((FIXTURES / "ddg_images.json").read_text())
OPENVERSE = json.loads((FIXTURES / "openverse_images.json").read_text())
WIKIMEDIA = json.loads((FIXTURES / "wikimedia_images.json").read_text())
ANOMALY = (FIXTURES / "ddg_anomaly.html").read_text()
DDG_CHALLENGE = json.loads((FIXTURES / "ddg_images_challenge.json").read_text())


def serve(monkeypatch: pytest.MonkeyPatch, handler: Callable[[httpx.Request], httpx.Response]) -> None:
    monkeypatch.setattr(
        web._http,
        "client",
        lambda: httpx.Client(transport=httpx.MockTransport(handler), follow_redirects=True),
    )


def routes(
    *,
    ddg_search: httpx.Response | None = None,
    ddg_images: httpx.Response | None = None,
    openverse: httpx.Response | None = None,
    wikimedia: httpx.Response | None = None,
) -> Callable[[httpx.Request], httpx.Response]:
    """A MockTransport handler for all three backends; unset responses use the saved fixtures."""

    def handler(req: httpx.Request) -> httpx.Response:
        if req.url.host == "duckduckgo.com":
            if req.url.path == "/i.js":
                return ddg_images if ddg_images is not None else httpx.Response(200, json=DDG_IMAGES)
            return ddg_search if ddg_search is not None else httpx.Response(200, html=DDG_HTML)
        if req.url.host == "api.openverse.org":
            return openverse if openverse is not None else httpx.Response(200, json=OPENVERSE)
        return wikimedia if wikimedia is not None else httpx.Response(200, json=WIKIMEDIA)

    return handler


# --- DuckDuckGo -------------------------------------------------------------------------------


def test_parse_duckduckgo() -> None:
    results = parse_duckduckgo(DDG_IMAGES)
    assert len(results) == 5
    first = results[0]
    assert first.title == "Pollination - Birds, Flowering Plants, Nectar | Britannica"
    assert first.url == "https://cdn.britannica.com/76/151376-050-13586FE2/monarch-butterfly-flowers-bush.jpg"
    assert first.thumbnail is not None and first.thumbnail.startswith("https://tse3.mm.bing.net/")
    assert (first.width, first.height) == (1600, 1110)
    assert first.source == "https://www.britannica.com/science/pollination/Birds"
    # DDG's index carries no licensing or authorship.
    assert first.license is None and first.creator is None


def test_parse_duckduckgo_skips_results_without_an_image() -> None:
    data = {"results": [{"title": "no image"}, {"title": "ok", "image": "https://x/a.png"}]}
    assert [r.url for r in parse_duckduckgo(data)] == ["https://x/a.png"]


def test_parse_duckduckgo_changed_shape() -> None:
    with pytest.raises(WebToolError, match="DuckDuckGo image search failed") as e:
        parse_duckduckgo({"unexpected": True})
    assert e.value.code == "markup_changed"


def test_duckduckgo_backend(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[httpx.Request] = []

    def handler(req: httpx.Request) -> httpx.Response:
        seen.append(req)
        return routes()(req)

    serve(monkeypatch, handler)
    r = images("butterfly", max_results=3, backend="duckduckgo")
    assert r.backend == "duckduckgo" and r.note is None and len(r.results) == 3
    # First the image search page, then i.js with the vqd scraped from it.
    assert seen[0].url.path == "/" and seen[0].url.params["iax"] == "images"
    js = seen[1]
    assert js.url.path == "/i.js"
    assert js.url.params["vqd"] == "4-108071651293510804248590779239042761161"
    assert js.url.params["o"] == "json" and js.url.params["l"] == "us-en" and js.url.params["f"] == ",,,"
    assert js.headers["accept"] == "application/json"
    assert js.headers["referer"] == "https://duckduckgo.com/"
    assert js.headers["x-requested-with"] == "XMLHttpRequest"


@pytest.mark.parametrize(
    "markup, expected", [('vqd="4-111"', "4-111"), ("vqd='4-222'", "4-222"), ("vqd=4-333", "4-333")]
)
def test_duckduckgo_extracts_vqd_spellings(
    monkeypatch: pytest.MonkeyPatch, markup: str, expected: str
) -> None:
    seen: list[httpx.Request] = []

    def handler(req: httpx.Request) -> httpx.Response:
        seen.append(req)
        if req.url.path == "/i.js":
            return httpx.Response(200, json=DDG_IMAGES)
        return httpx.Response(200, html=f"<html><script>window.x = {{{markup}}};</script></html>")

    serve(monkeypatch, handler)
    r = images("butterfly", max_results=2, backend="duckduckgo")
    assert r.backend == "duckduckgo" and len(r.results) == 2
    assert seen[1].url.params["vqd"] == expected


def test_duckduckgo_missing_vqd_is_blocked(monkeypatch: pytest.MonkeyPatch) -> None:
    # The real bot-check page (also used by the search tests) has no vqd.
    serve(monkeypatch, routes(ddg_search=httpx.Response(200, html=ANOMALY)))
    with pytest.raises(WebToolError, match="no vqd token") as e:
        images("x", backend="duckduckgo")
    assert e.value.code == "blocked"


def test_duckduckgo_non_json_ijson_is_blocked(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(monkeypatch, routes(ddg_images=httpx.Response(200, text="<html>challenge</html>")))
    with pytest.raises(WebToolError, match=r"i\.js did not return JSON") as e:
        images("x", backend="duckduckgo")
    assert e.value.code == "blocked" and "challenge" in str(e.value)


def test_duckduckgo_http_202_is_blocked(monkeypatch: pytest.MonkeyPatch) -> None:
    # DDG answers i.js with HTTP 202 and a challenge body when it flags the host.
    serve(monkeypatch, routes(ddg_images=httpx.Response(202, json=DDG_CHALLENGE)))
    with pytest.raises(WebToolError, match="HTTP 202") as e:
        images("x", backend="duckduckgo")
    assert e.value.code == "blocked"


def test_duckduckgo_challenge_body_is_blocked(monkeypatch: pytest.MonkeyPatch) -> None:
    # The same challenge can arrive with HTTP 200.
    serve(monkeypatch, routes(ddg_images=httpx.Response(200, json=DDG_CHALLENGE)))
    with pytest.raises(WebToolError, match="challenge") as e:
        images("x", backend="duckduckgo")
    assert e.value.code == "blocked"


# --- Openverse and Wikimedia Commons ----------------------------------------------------------


def test_parse_openverse() -> None:
    results = parse_openverse(OPENVERSE)
    assert len(results) == 3
    first = results[0]
    assert first.title == "Butterfly"
    assert first.url == "https://live.staticflickr.com/8633/16221155444_b7df663f37_b.jpg"
    assert (
        first.thumbnail == "https://api.openverse.org/v1/images/6be67a14-b381-487e-858e-7533b6d0496f/thumb/"
    )
    assert (first.width, first.height) == (1024, 640)
    assert first.source == "https://www.flickr.com/photos/84935187@N04/16221155444"
    assert first.license == "CC BY-NC 2.0"
    assert first.creator == "Sunny M5"


def test_parse_openverse_empty_is_fine() -> None:
    assert parse_openverse({"result_count": 0, "results": []}) == []


def test_parse_openverse_skips_results_without_an_image() -> None:
    data = {"results": [{"title": "no image"}, {"title": "ok", "url": "https://example.com/a.png"}]}
    results = parse_openverse(data)
    assert [r.url for r in results] == ["https://example.com/a.png"]
    assert results[0].title == "ok"


def test_parse_openverse_changed_shape() -> None:
    with pytest.raises(WebToolError, match="Openverse image search failed") as e:
        parse_openverse({"unexpected": True})
    assert e.value.code == "markup_changed"
    assert "images.py" in str(e.value) and "propose_change" in str(e.value)


def test_parse_wikimedia() -> None:
    results = parse_wikimedia(WIKIMEDIA)
    assert len(results) == 2
    first = results[0]
    # The "File:" prefix is dropped and the ObjectName (when present) is the title.
    assert first.title == "Charaxes brutus natalensis"
    assert first.url.startswith(
        "https://upload.wikimedia.org/wikipedia/commons/3/3d/Charaxes_brutus_natalensis.jpg"
    )
    assert first.thumbnail is not None and first.thumbnail.startswith("https://thumb.wikimedia.org/")
    assert (first.width, first.height) == (1800, 1200)
    assert first.source == "https://commons.wikimedia.org/wiki/File:Charaxes_brutus_natalensis.jpg"
    assert first.license == "GFDL 1.2"
    # The Artist value is HTML; it comes back as plain text.
    assert first.creator == "Muhammad Mahdi Karim"
    second = results[1]
    assert second.title == "Common brimstone butterfly (Gonepteryx rhamni) male"


def test_parse_wikimedia_changed_shape() -> None:
    with pytest.raises(WebToolError, match="Wikimedia Commons image search failed") as e:
        parse_wikimedia({"batchcomplete": True})
    assert e.value.code == "markup_changed"
    assert "images.py" in str(e.value)


def test_openverse_backend(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[httpx.Request] = []

    def handler(req: httpx.Request) -> httpx.Response:
        seen.append(req)
        return routes()(req)

    serve(monkeypatch, handler)
    r = images("butterfly", max_results=3, backend="openverse")
    assert r.backend == "openverse" and r.note is None and len(r.results) == 3
    assert seen[0].url.params["q"] == "butterfly"
    assert seen[0].url.params["page_size"] == "3"
    assert seen[0].url.params["format"] == "json"
    # A descriptive User-Agent and an explicit JSON Accept (the shared client sends a browser one).
    assert seen[0].headers["user-agent"].startswith("botler-web-images/")
    assert seen[0].headers["accept"] == "application/json"


def test_wikimedia_backend(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[httpx.Request] = []

    def handler(req: httpx.Request) -> httpx.Response:
        seen.append(req)
        return routes()(req)

    serve(monkeypatch, handler)
    r = images("butterfly", max_results=2, backend="wikimedia")
    assert r.backend == "wikimedia" and len(r.results) == 2
    params = seen[0].url.params
    assert params["generator"] == "search" and params["gsrnamespace"] == "6"
    assert params["gsrsearch"] == "butterfly" and params["gsrlimit"] == "2"
    assert params["iiprop"] == "url|size|extmetadata" and params["iiurlwidth"] == "320"
    assert params["formatversion"] == "2"


# --- auto -------------------------------------------------------------------------------------


def test_auto_prefers_duckduckgo(monkeypatch: pytest.MonkeyPatch) -> None:
    hosts: list[str] = []

    def handler(req: httpx.Request) -> httpx.Response:
        hosts.append(req.url.host or "")
        return routes()(req)

    serve(monkeypatch, handler)
    r = images("butterfly", max_results=3)
    assert r.backend == "duckduckgo" and r.note is None and len(r.results) == 3
    assert all(host == "duckduckgo.com" for host in hosts)


def test_auto_skips_duckduckgo_and_uses_openverse(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(monkeypatch, routes(ddg_search=httpx.Response(200, html=ANOMALY)))
    r = images("butterfly", max_results=3)
    assert r.backend == "openverse" and len(r.results) == 3
    assert r.note is not None and "duckduckgo" in r.note and "no vqd" in r.note


def test_auto_falls_through_to_wikimedia(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(
        monkeypatch,
        routes(ddg_search=httpx.Response(200, html=ANOMALY), openverse=httpx.Response(500, text="boom")),
    )
    r = images("butterfly", max_results=2)
    assert r.backend == "wikimedia" and len(r.results) == 2
    assert r.note is not None
    assert "duckduckgo" in r.note and "openverse" in r.note and "HTTP 500" in r.note


def test_auto_reports_all_failures(monkeypatch: pytest.MonkeyPatch) -> None:
    serve(
        monkeypatch,
        routes(
            ddg_search=httpx.Response(200, html=ANOMALY),
            openverse=httpx.Response(500, text="a"),
            wikimedia=httpx.Response(503, text="b"),
        ),
    )
    with pytest.raises(
        WebToolError, match=r"All image backends failed.*duckduckgo.*openverse.*wikimedia"
    ) as e:
        images("x")
    assert e.value.code == "http"


def test_explicit_backend_does_not_fall_through(monkeypatch: pytest.MonkeyPatch) -> None:
    hosts: list[str] = []

    def handler(req: httpx.Request) -> httpx.Response:
        hosts.append(req.url.host or "")
        return httpx.Response(500, text="boom")

    serve(monkeypatch, handler)
    with pytest.raises(WebToolError, match="Openverse"):
        images("x", backend="openverse")
    assert all(host == "api.openverse.org" for host in hosts)
