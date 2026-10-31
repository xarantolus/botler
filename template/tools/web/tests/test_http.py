"""The shared HTTP client must look like a browser.

DuckDuckGo's bot check flags requests without browser headers — in particular httpx's default
`Accept: */*`. Keep `_http.client()`'s header set browser-like.
"""

from web import _http


def test_client_sends_browser_headers() -> None:
    with _http.client() as client:
        headers = client.headers
        assert headers["User-Agent"].startswith("Mozilla/5.0 ")
        assert "text/html" in headers["Accept"]
        assert headers["Accept"] != "*/*"
        assert headers["Accept-Language"].startswith("en-US")
        assert headers["Sec-Fetch-Mode"] == "navigate"
