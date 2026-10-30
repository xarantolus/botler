"""`web.fetch`: a URL as readable text. HTML becomes markdown (trafilatura extracts the main content
and drops navigation and boilerplate); plain text and JSON pass through; anything else is reported
by its metadata only. `tests/fixtures/article.html` is a saved page to test against."""

from __future__ import annotations

from typing import Annotated
from urllib.parse import urlsplit

import botler
import httpx
import trafilatura
from pydantic import BaseModel, Field

from . import _http
from ._http import WebToolError, fix_hint

# The most of a response body that is downloaded; longer bodies are cut here.
MAX_BYTES = 5_000_000
_HTML = ("text/html", "application/xhtml+xml")
_TEXT_TYPES = ("application/json", "application/xml", "application/javascript", "application/x-ndjson")


class FetchResult(BaseModel):
    # Field order is output order: metadata first, so a cut-off result still shows it.
    url: str
    status: int
    content_type: str
    title: str | None
    total_chars: int
    truncated: bool
    note: str | None
    content: str


def _kind(content_type: str) -> str:
    mime = content_type.split(";")[0].strip().lower()
    if mime in _HTML:
        return "html"
    if mime.startswith("text/") or mime in _TEXT_TYPES or mime.endswith(("+json", "+xml")):
        return "text"
    return "other"


def to_markdown(html: str, url: str) -> tuple[str | None, str, str | None]:
    """(title, markdown, note) for an HTML page."""
    try:
        meta = trafilatura.extract_metadata(html)
        text = trafilatura.extract(
            html,
            url=url,
            output_format="markdown",
            include_links=True,
            include_tables=True,
            include_comments=False,
            favor_recall=True,
        )
        note = None
        if not text:
            # No main content found (link lists, app shells): fall back to all visible text.
            text = trafilatura.html2txt(html)
            note = "no main content detected; showing all text of the page"
    except Exception as e:
        raise WebToolError(
            "convert_failed",
            f"Converting the HTML of {url} to markdown failed: {type(e).__name__}: {e}. {fix_hint('fetch')}",
        ) from e
    if not text.strip():
        return meta.title if meta else None, "", "the page has no text; it may be rendered by JavaScript"
    return (meta.title if meta else None), text, note


def _download(url: str) -> tuple[httpx.Response, bytes, bool]:
    try:
        with _http.client() as http, http.stream("GET", url) as resp:
            if _kind(resp.headers.get("content-type", "")) == "other":
                return resp, b"", False
            body = bytearray()
            for chunk in resp.iter_bytes():
                body += chunk
                if len(body) >= MAX_BYTES:
                    return resp, bytes(body[:MAX_BYTES]), True
            return resp, bytes(body), False
    except httpx.HTTPError as e:
        raise WebToolError("network", f"Fetching {url} failed: {type(e).__name__}: {e}") from e


@botler.tool(effects=["network"])
def fetch(
    url: str,
    max_chars: Annotated[int, Field(ge=1)] = 50_000,
    raw: bool = False,
    offset: Annotated[int, Field(ge=0)] = 0,
) -> FetchResult:
    """Fetch a URL (following redirects) and return its content as text: HTML pages as markdown of the
    main content, plain text and JSON as they are, other types (PDF, images, ...) as metadata only.
    `raw` returns HTML source instead of markdown. Long content is cut at `max_chars`; read on with
    `offset`. The result has the final URL and the HTTP status (error pages are returned, not raised).
    Maintained in the monorepo (tools/web/); if it breaks, fix it."""
    if urlsplit(url).scheme not in ("http", "https"):
        raise WebToolError(
            "bad_url", f"{url!r} is not an http(s) URL; pass a full URL like https://example.com/"
        )
    resp, body, cut = _download(url)
    content_type = resp.headers.get("content-type", "")
    final = str(resp.url)
    kind = _kind(content_type)
    title: str | None = None
    note: str | None = None
    if kind == "other":
        size = resp.headers.get("content-length", "unknown")
        text = ""
        note = f"content type {content_type or 'unknown'} ({size} bytes) is not text; only metadata returned"
    else:
        text = body.decode(resp.encoding or "utf-8", errors="replace")
        if kind == "html" and not raw:
            title, text, note = to_markdown(text, final)
        if cut:
            note = f"{note}; " if note else ""
            note += f"the response was larger than {MAX_BYTES} bytes and was cut there"
    total = len(text)
    content = text[offset : offset + max_chars]
    truncated = offset + max_chars < total
    if truncated:
        end = offset + len(content)
        more = f"showing characters {offset}-{end} of {total}; call again with offset={end} for more"
        note = f"{note}; {more}" if note else more
    return FetchResult(
        url=final,
        status=resp.status_code,
        content_type=content_type,
        title=title,
        total_chars=total,
        truncated=truncated,
        note=note,
        content=content,
    )
