"""The HTTP client and the error both web tools share."""

from __future__ import annotations

import httpx

USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0"
# A browser sends a real `Accept`; httpx defaults to `*/*`, which DuckDuckGo's bot check flags.
ACCEPT = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
# The headers a Firefox navigation sends. A sparse set (User-Agent only, or `Accept: */*`)
# is an easy bot signal; care is still needed, DuckDuckGo also rate-limits per IP.
BROWSER_HEADERS = {
    "User-Agent": USER_AGENT,
    "Accept": ACCEPT,
    "Accept-Language": "en-US,en;q=0.5",
    "Sec-Fetch-Dest": "document",
    "Sec-Fetch-Mode": "navigate",
    "Sec-Fetch-Site": "none",
    "Sec-Fetch-User": "?1",
    "Upgrade-Insecure-Requests": "1",
}
TIMEOUT = httpx.Timeout(20.0, connect=10.0)


class WebToolError(RuntimeError):
    """A web tool failed. `code` is machine-readable; the message says what to do about it."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def fix_hint(module: str) -> str:
    """Where the code lives and how to repair it, appended to errors that mean the code is wrong."""
    return (
        f"The code lives at tools/web/src/web/{module}.py in the monorepo, with saved pages in "
        "tools/web/tests/fixtures/. To fix it: save the current page as a fixture, update the parser "
        "and its tests on a branch, run gate/run, and propose_change."
    )


def client() -> httpx.Client:
    """A client with a browser's headers and timeouts. Tests replace this to serve saved pages."""
    return httpx.Client(headers=BROWSER_HEADERS, timeout=TIMEOUT, follow_redirects=True)
