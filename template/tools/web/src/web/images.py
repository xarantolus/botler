"""`web.images`: image search over keyless, mostly CC-licensed catalogues. Use it when you need an
*image asset* (a game overlay, a logo, a reference picture), not a page: it returns the full image
URL, a thumbnail, its size, the page it came from, and the license and creator when known.

Backends (all keyless, each tried on its own so one failing never kills the call):

- DuckDuckGo (`https://duckduckgo.com/i.js`, see `duckduckgo()`): the broadest image index. Needs a
  per-query `vqd` token scraped from the image search page; it rate-limits and bot-checks often, so
  it is first in `auto` but must never break the other backends. It carries no license or creator.
- Openverse (`https://api.openverse.org/v1/images/`): the Creative Commons search index. Each result
  has the image URL, a thumbnail, width/height, the source page, and license + creator.
- Wikimedia Commons (`action=query&generator=search&gsrnamespace=6&prop=imageinfo`): the image URL,
  a thumbnail rendered at `iiurlwidth`, width/height, the file page, and the license and author from
  `extmetadata` (HTML stripped).

`auto` (the default) tries DuckDuckGo, then Openverse, then Wikimedia; `note` says which were skipped
and why. Wikimedia's API asks for a descriptive User-Agent, which we send. The markup/data changes now
and then: `tests/fixtures/ddg_images.html`, `ddg_images.json`, `openverse_images.json` and
`wikimedia_images.json` are saved responses to test against, and parse errors point back here.
"""

from __future__ import annotations

import html
import re
from typing import Annotated, Any, Literal, cast

import botler
import httpx
from botler.tools import Table
from pydantic import BaseModel, Field, ValidationError

from . import _http
from ._http import WebToolError, fix_hint

OPENVERSE_URL = "https://api.openverse.org/v1/images/"
WIKIMEDIA_URL = "https://commons.wikimedia.org/w/api.php"
DDG_SEARCH_URL = "https://duckduckgo.com/"
DDG_IMAGES_URL = "https://duckduckgo.com/i.js"
# Wikimedia's API asks for a descriptive User-Agent with a contact, not a browser one. The shared
# client's browser `Accept: text/html` makes Openverse's DRF endpoint serve its browsable HTML API,
# so ask for JSON explicitly (`format=json` below is the second belt).
API_USER_AGENT = "botler-web-images/0.1 (monorepo tools/web; https://github.com/xarantolus/botler)"
API_HEADERS = {"User-Agent": API_USER_AGENT, "Accept": "application/json"}
# The shared client's 20s read timeout is tight for Openverse; these APIs can take longer.
API_TIMEOUT = httpx.Timeout(45.0, connect=10.0)
# The width of the thumbnails Wikimedia renders for us.
THUMB_WIDTH = 320
# Openverse and Wikimedia both accept a larger page, but this is plenty for an agent.
MAX_RESULTS = 30
# DDG's i.js wants a browser-like request with an XHR Accept. `vqd` is a per-query token scraped
# from the image search page; DDG writes it as vqd="4-...", vqd='4-...' or vqd=4-...
DDG_IMAGE_HEADERS = {
    "Accept": "application/json",
    "Referer": "https://duckduckgo.com/",
    "X-Requested-With": "XMLHttpRequest",
}
_VQD = re.compile(r"""vqd\s*=\s*["']?([A-Za-z0-9_-]+)""")
_TAGS = re.compile(r"<[^>]+>")


class ImageResult(BaseModel):
    title: str
    url: str
    thumbnail: str | None
    width: int | None
    height: int | None
    source: str | None
    license: str | None
    creator: str | None


Backend = Literal["duckduckgo", "openverse", "wikimedia"]
BackendChoice = Literal["auto", "duckduckgo", "openverse", "wikimedia"]
# `auto` order: DuckDuckGo has the widest coverage but rate-limits most, then Openverse, Wikimedia.
BACKENDS: tuple[Backend, ...] = ("duckduckgo", "openverse", "wikimedia")


class ImagesResponse(BaseModel):
    backend: Backend
    note: str | None = None
    results: list[ImageResult]


def _error(code: str, what: str, backend: str) -> WebToolError:
    return WebToolError(code, f"{backend} image search failed: {what}. {fix_hint('images')}")


def _get(url: str, params: dict[str, str | int], backend: str) -> Any:  # noqa: ANN401 (JSON)
    try:
        with _http.client() as http:
            resp = http.get(url, params=params, headers=API_HEADERS, timeout=API_TIMEOUT)
    except httpx.HTTPError as e:
        raise _error("network", f"request failed: {type(e).__name__}: {e}", backend) from e
    if resp.status_code != 200:
        raise _error("http", f"answered HTTP {resp.status_code}: {resp.text[:300]}", backend)
    try:
        return resp.json()
    except ValueError as e:
        raise _error("markup_changed", f"the response is not JSON: {e}", backend) from e


def _text(value: Any) -> str | None:  # noqa: ANN401 (JSON)
    """Whitespace-collapsed plain text, with any HTML that `extmetadata` carries stripped."""
    if value is None:
        return None
    text = " ".join(html.unescape(_TAGS.sub("", str(value))).split())
    return text or None


class _ExtMeta(BaseModel):
    value: Any = None


class _OpenverseItem(BaseModel):
    title: str | None = None
    url: str | None = None
    thumbnail: str | None = None
    width: int | None = None
    height: int | None = None
    foreign_landing_url: str | None = None
    license: str | None = None
    license_version: str | None = None
    creator: str | None = None


class _OpenverseResponse(BaseModel):
    results: list[_OpenverseItem]


class _WikimediaInfo(BaseModel):
    url: str | None = None
    thumburl: str | None = None
    width: int | None = None
    height: int | None = None
    descriptionurl: str | None = None
    extmetadata: dict[str, _ExtMeta] | None = None


class _WikimediaPage(BaseModel):
    title: str | None = None
    imageinfo: list[_WikimediaInfo] | None = None


class _WikimediaQuery(BaseModel):
    pages: list[_WikimediaPage]


class _WikimediaResponse(BaseModel):
    query: _WikimediaQuery


class _DuckDuckGoItem(BaseModel):
    title: str | None = None
    image: str | None = None
    thumbnail: str | None = None
    width: int | None = None
    height: int | None = None
    url: str | None = None


class _DuckDuckGoResponse(BaseModel):
    results: list[_DuckDuckGoItem]


def _extmeta(info: _WikimediaInfo, key: str) -> str | None:
    meta = (info.extmetadata or {}).get(key)
    return _text(meta.value) if meta is not None else None


def _openverse_license(value: str | None, version: str | None) -> str | None:
    if not value:
        return None
    key = value.lower()
    if key == "cc0":
        return "CC0"
    if key == "pdm":
        return "Public Domain Mark"
    name = f"CC {value.upper()}"
    return f"{name} {version}" if version else name


def parse_openverse(data: Any) -> list[ImageResult]:  # noqa: ANN401 (JSON)
    try:
        response = _OpenverseResponse.model_validate(data)
    except ValidationError as e:
        raise _error("markup_changed", f"unexpected response shape: {e}", "Openverse") from e
    out: list[ImageResult] = []
    for item in response.results:
        if not item.url:
            continue  # a result without an image is no use to the caller
        out.append(
            ImageResult(
                title=item.title or item.url,
                url=item.url,
                thumbnail=item.thumbnail,
                width=item.width,
                height=item.height,
                source=item.foreign_landing_url,
                license=_openverse_license(item.license, item.license_version),
                creator=item.creator,
            )
        )
    return out


def parse_wikimedia(data: Any) -> list[ImageResult]:  # noqa: ANN401 (JSON)
    try:
        response = _WikimediaResponse.model_validate(data)
    except ValidationError as e:
        raise _error("markup_changed", f"unexpected response shape: {e}", "Wikimedia Commons") from e
    out: list[ImageResult] = []
    for page in response.query.pages:
        if not page.imageinfo:
            continue
        info = page.imageinfo[0]
        if not info.url:
            continue
        title = _text(page.title) or info.url
        if title.startswith("File:"):
            title = title[len("File:") :]
        out.append(
            ImageResult(
                title=_extmeta(info, "ObjectName") or title,
                url=info.url,
                thumbnail=info.thumburl,
                width=info.width,
                height=info.height,
                source=info.descriptionurl,
                license=_extmeta(info, "LicenseShortName") or _extmeta(info, "UsageTerms"),
                creator=_extmeta(info, "Artist"),
            )
        )
    return out


def parse_duckduckgo(data: Any) -> list[ImageResult]:  # noqa: ANN401 (JSON)
    try:
        response = _DuckDuckGoResponse.model_validate(data)
    except ValidationError as e:
        raise _error("markup_changed", f"unexpected response shape: {e}", "DuckDuckGo") from e
    out: list[ImageResult] = []
    for item in response.results:
        if not item.image:
            continue  # a result without an image is no use to the caller
        out.append(
            ImageResult(
                title=item.title or item.image,
                url=item.image,
                thumbnail=item.thumbnail,
                width=item.width,
                height=item.height,
                source=item.url,  # the page the image is shown on
                license=None,  # DDG's index does not carry licensing or authorship
                creator=None,
            )
        )
    return out


def openverse(query: str, max_results: int) -> list[ImageResult]:
    params: dict[str, str | int] = {"q": query, "page_size": max_results, "format": "json"}
    return parse_openverse(_get(OPENVERSE_URL, params, "Openverse"))


def wikimedia(query: str, max_results: int) -> list[ImageResult]:
    params: dict[str, str | int] = {
        "action": "query",
        "generator": "search",
        "gsrnamespace": 6,
        "gsrsearch": query,
        "gsrlimit": max_results,
        "prop": "imageinfo",
        "iiprop": "url|size|extmetadata",
        "iiurlwidth": THUMB_WIDTH,
        "format": "json",
        "formatversion": 2,
    }
    return parse_wikimedia(_get(WIKIMEDIA_URL, params, "Wikimedia Commons"))


def _ddg_vqd(http: httpx.Client, query: str) -> str:
    try:
        resp = http.get(
            DDG_SEARCH_URL,
            params={"q": query, "iax": "images", "ia": "images"},
            timeout=API_TIMEOUT,
        )
    except httpx.HTTPError as e:
        raise _error(
            "network", f"the image search page request failed: {type(e).__name__}: {e}", "DuckDuckGo"
        ) from e
    if resp.status_code != 200:
        raise _error("blocked", f"the image search page answered HTTP {resp.status_code}", "DuckDuckGo")
    match = _VQD.search(resp.text)
    if match is None:
        raise _error(
            "blocked",
            "no vqd token on the image search page; DuckDuckGo served its bot check or changed markup",
            "DuckDuckGo",
        )
    return match.group(1)


def _ddg_images(http: httpx.Client, query: str, vqd: str) -> Any:  # noqa: ANN401 (JSON)
    params = {"l": "us-en", "o": "json", "q": query, "vqd": vqd, "f": ",,,", "p": "1"}
    try:
        resp = http.get(DDG_IMAGES_URL, params=params, headers=DDG_IMAGE_HEADERS, timeout=API_TIMEOUT)
    except httpx.HTTPError as e:
        raise _error("network", f"the i.js request failed: {type(e).__name__}: {e}", "DuckDuckGo") from e
    if resp.status_code != 200:
        raise _error("blocked", f"i.js answered HTTP {resp.status_code}", "DuckDuckGo")
    try:
        data: Any = resp.json()
    except ValueError as e:
        snippet = " ".join(resp.text[:200].split())
        raise _error(
            "blocked", f"i.js did not return JSON ({e}); body starts: {snippet}", "DuckDuckGo"
        ) from e
    if not isinstance(data, dict):
        raise _error("markup_changed", "i.js returned JSON that is not an object", "DuckDuckGo")
    if "challenge" in data:
        # DDG answers HTTP 202 with {"challenge": {...}} when it flags the host, and a 200 with the
        # same body is possible too; treat it as the bot check, not a markup change.
        raise _error(
            "blocked", "i.js returned a challenge (DuckDuckGo's bot check); wait some minutes", "DuckDuckGo"
        )
    return cast("Any", data)  # pyright narrows `Any` to dict[Unknown, Unknown] after isinstance


def duckduckgo(query: str, max_results: int) -> list[ImageResult]:
    with _http.client() as http:
        vqd = _ddg_vqd(http, query)
        data = _ddg_images(http, query, vqd)
    return parse_duckduckgo(data)[:max_results]


def _run(backend: Backend, query: str, max_results: int) -> ImagesResponse:
    if backend == "duckduckgo":
        return ImagesResponse(backend=backend, results=duckduckgo(query, max_results))
    if backend == "openverse":
        return ImagesResponse(backend=backend, results=openverse(query, max_results))
    return ImagesResponse(backend=backend, results=wikimedia(query, max_results))


@botler.tool(
    effects=["network"],
    render=Table(
        items="results",
        columns=["title", "width", "height", "license", "creator", "source"],
    ),
)
def images(
    query: Annotated[str, Field(min_length=1)],
    max_results: Annotated[int, Field(ge=1, le=MAX_RESULTS)] = 10,
    backend: BackendChoice = "auto",
) -> ImagesResponse:
    """Search for images and get assets back, not pages. Returns {backend, note, results: [{title,
    url, thumbnail, width, height, source, license, creator}]}. `url` is the full image, `thumbnail`
    a smaller version, `source` the page it came from; `license` and `creator` are present when the
    backend knows them (DuckDuckGo does not). `backend`: "duckduckgo" (widest coverage, but it
    rate-limits and bot-checks often), "openverse" (Creative Commons' index), "wikimedia" (Wikimedia
    Commons), or "auto" (default): DuckDuckGo, then Openverse, then Wikimedia, skipping a backend
    that fails; `note` says which were skipped and why. All are keyless. Maintained in the monorepo
    (tools/web/); if it breaks, fix it."""
    if backend != "auto":
        return _run(backend, query, max_results)
    failures: list[str] = []
    for name in BACKENDS:
        try:
            response = _run(name, query, max_results)
        except WebToolError as e:
            failures.append(f"{name}: {e}")
            continue
        if failures:
            response.note = "skipped " + "; ".join(failures)
        return response
    raise WebToolError("http", "All image backends failed. " + " ".join(failures))
