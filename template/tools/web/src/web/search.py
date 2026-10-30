"""`web.search`: OpenRouter's web search (`openrouter.py`) or a scraper of DuckDuckGo's no-JavaScript
HTML endpoint (here). `auto` tries OpenRouter first: DuckDuckGo blocks a host with its bot check after a
few searches, while OpenRouter costs about $0.001 per search, little next to a model call.

DuckDuckGo changes its markup now and then. When parsing fails, `parse_page` raises with a pointer
here; the fixture `tests/fixtures/ddg_results.html` is a saved results page to test against.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Literal
from urllib.parse import parse_qs, urlsplit

import botler
import httpx
from pydantic import BaseModel, Field
from selectolax.lexbor import LexborHTMLParser, LexborNode

from . import _http, openrouter
from ._http import WebToolError, fix_hint

DDG_URL = "https://html.duckduckgo.com/html/"
# DuckDuckGo serves about ten results per page.
MAX_PAGES = 3


class SearchResult(BaseModel):
    title: str
    url: str
    snippet: str


Backend = Literal["openrouter", "duckduckgo"]
BackendChoice = Literal["auto", "openrouter", "duckduckgo"]


class SearchResponse(BaseModel):
    backend: Backend
    cost_usd: float | None = None
    note: str | None = None
    results: list[SearchResult]


@dataclass
class Page:
    results: list[SearchResult]
    # The hidden fields of the "Next" form, or None on the last page.
    next_form: dict[str, str] | None


def _markup_error(what: str) -> WebToolError:
    return WebToolError(
        "markup_changed", f"DuckDuckGo result markup not recognised: {what}. {fix_hint('search')}"
    )


def _text(node: LexborNode | None) -> str:
    return " ".join(node.text(separator="").split()) if node is not None else ""


def unwrap(href: str) -> str:
    """Resolve DuckDuckGo's redirect links (`//duckduckgo.com/l/?uddg=<target>`) to the target."""
    if href.startswith("//"):
        href = "https:" + href
    parts = urlsplit(href)
    if parts.netloc.endswith("duckduckgo.com") and parts.path.startswith("/l/"):
        target = parse_qs(parts.query).get("uddg")
        if not target:
            raise _markup_error(f"redirect link without a uddg target: {href}")
        return target[0]
    if parts.scheme not in ("http", "https"):
        raise _markup_error(f"result link is not an absolute http(s) URL: {href}")
    return href


def _blocked(tree: LexborHTMLParser) -> bool:
    return tree.css_first("#challenge-form, .anomaly-modal__title, form[action*='anomaly']") is not None


def _blocked_error() -> WebToolError:
    return WebToolError(
        "blocked",
        "DuckDuckGo answered with its bot check (a captcha page) instead of results. This is usually "
        "temporary rate limiting after many searches: wait some minutes before searching again. If it "
        "persists for hours, the scraper needs another approach (another endpoint or search engine). "
        + fix_hint("search"),
    )


def parse_page(html: str) -> Page:
    """Parse one results page. Raises `WebToolError` if DuckDuckGo blocked us or the markup changed."""
    tree = LexborHTMLParser(html)
    if _blocked(tree):
        raise _blocked_error()
    links = tree.css_first("#links")
    if links is None:
        raise _markup_error("no #links container on the page")
    results: list[SearchResult] = []
    for node in links.css("div.result"):
        if "result--ad" in (node.attributes.get("class") or ""):
            continue
        if node.css_first(".no-results") is not None:
            continue
        a = node.css_first("a.result__a")
        href = a.attributes.get("href") if a is not None else None
        if a is None or not href:
            raise _markup_error("a div.result without an a.result__a link")
        results.append(
            SearchResult(title=_text(a), url=unwrap(href), snippet=_text(node.css_first(".result__snippet")))
        )
    if not results and tree.css_first(".no-results") is None:
        raise _markup_error("no results and no 'No results' notice")
    return Page(results=results, next_form=_next_form(tree))


def _next_form(tree: LexborHTMLParser) -> dict[str, str] | None:
    for form in tree.css(".nav-link form"):
        if form.css_first("input[type=submit][value=Next]") is None:
            continue
        fields: dict[str, str] = {}
        for i in form.css("input[type=hidden]"):
            name = i.attributes.get("name")
            if name:
                fields[name] = i.attributes.get("value") or ""
        return fields
    return None


def _post(http: httpx.Client, form: dict[str, str]) -> str:
    try:
        resp = http.post(DDG_URL, data=form)
    except httpx.HTTPError as e:
        raise WebToolError("network", f"DuckDuckGo request failed: {type(e).__name__}: {e}") from e
    if resp.status_code in (202, 403, 429):
        # DuckDuckGo serves its bot check with HTTP 202.
        if _blocked(LexborHTMLParser(resp.text)):
            raise _blocked_error()
        raise WebToolError(
            "blocked",
            f"DuckDuckGo refused the search (HTTP {resp.status_code}, rate limiting or bot check). "
            "Wait some minutes before searching again.",
        )
    if resp.status_code != 200:
        raise WebToolError("http", f"DuckDuckGo answered HTTP {resp.status_code} to the search request.")
    return resp.text


def duckduckgo(query: str, max_results: int, region: str | None) -> list[SearchResult]:
    form = {"q": query, "b": ""}
    if region:
        form["kl"] = region
    results: list[SearchResult] = []
    seen: set[str] = set()
    with _http.client() as http:
        for _ in range(MAX_PAGES):
            page = parse_page(_post(http, form))
            for r in page.results:
                if r.url not in seen:
                    seen.add(r.url)
                    results.append(r)
            if len(results) >= max_results or page.next_form is None or not page.results:
                break
            form = page.next_form
    return results[:max_results]


def _run(backend: Backend, query: str, max_results: int, region: str | None) -> SearchResponse:
    if backend == "duckduckgo":
        return SearchResponse(backend=backend, results=duckduckgo(query, max_results, region))
    found, cost = openrouter.search(query, max_results)
    return SearchResponse(
        backend=backend, cost_usd=cost, results=[SearchResult.model_validate(r) for r in found]
    )


@botler.tool(effects=["network", "secret:OPENROUTER_API_KEY"])
def search(
    query: Annotated[str, Field(min_length=1)],
    max_results: Annotated[int, Field(ge=1, le=MAX_PAGES * 10)] = 10,
    region: str | None = None,
    backend: BackendChoice = "auto",
) -> SearchResponse:
    """Search the web. Returns {backend, cost_usd, note, results: [{title, url, snippet}]}; use web.fetch
    to read a result. `backend`: "openrouter" (OpenRouter's web search, about $0.001 per search),
    "duckduckgo" (free scraping, often blocked by its bot check), or "auto" (default): OpenRouter, and
    DuckDuckGo if that fails; `note` says why a backend was skipped. `region` is a DuckDuckGo region
    code such as "us-en" or "de-de"; OpenRouter ignores it. Maintained in the monorepo (tools/web/);
    if it breaks, fix it."""
    if backend != "auto":
        return _run(backend, query, max_results, region)
    try:
        return _run("openrouter", query, max_results, region)
    except WebToolError as first:
        try:
            response = _run("duckduckgo", query, max_results, region)
        except WebToolError as second:
            raise WebToolError(
                second.code, f"Both search backends failed. openrouter: {first} duckduckgo: {second}"
            ) from second
        response.note = f"openrouter failed, used duckduckgo: {first}"
        return response
