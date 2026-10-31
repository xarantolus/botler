"""A minimal Chrome DevTools Protocol client: one page target, calls and events."""

import json
import time
import urllib.request
from collections import deque
from typing import Any

from websockets.sync.client import ClientConnection, connect


class CdpError(Exception):
    pass


def page_ws_url(endpoint: str, timeout: float = 60.0) -> str:
    """The WebSocket URL of the first page target at `endpoint` (e.g. http://127.0.0.1:9223),
    waiting for the browser to come up."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            with urllib.request.urlopen(f"{endpoint}/json/list", timeout=5) as r:  # noqa: S310
                targets: list[dict[str, Any]] = json.load(r)
            pages = [t for t in targets if t.get("type") == "page"]
            if pages:
                return str(pages[0]["webSocketDebuggerUrl"])
            problem = "no page target"
        except OSError as e:
            problem = str(e)
        if time.monotonic() > deadline:
            raise CdpError(f"browser at {endpoint} not ready after {timeout}s: {problem}")
        time.sleep(0.5)


class Cdp:
    def __init__(self, ws: ClientConnection) -> None:
        self.ws = ws
        self.next_id = 0
        self.events: deque[dict[str, Any]] = deque()

    @classmethod
    def open(cls, ws_url: str) -> "Cdp":
        return cls(connect(ws_url, max_size=None, open_timeout=30))

    def call(self, method: str, **params: object) -> dict[str, Any]:
        self.next_id += 1
        msg_id = self.next_id
        self.ws.send(json.dumps({"id": msg_id, "method": method, "params": params}))
        while True:
            msg: dict[str, Any] = json.loads(self.ws.recv(timeout=60))
            if msg.get("id") == msg_id:
                if "error" in msg:
                    raise CdpError(f"{method}: {msg['error']}")
                result: dict[str, Any] = msg.get("result", {})
                return result
            if "method" in msg:
                self.events.append(msg)

    def evaluate(self, expression: str) -> object:
        """The value of `expression` in the page (awaited if it is a promise)."""
        r = self.call("Runtime.evaluate", expression=expression, returnByValue=True, awaitPromise=True)
        if "exceptionDetails" in r:
            raise CdpError(f"page script failed: {r['exceptionDetails'].get('text')}")
        return r.get("result", {}).get("value")

    def wait(self, seconds: float) -> list[dict[str, Any]]:
        """Events received within `seconds`."""
        deadline = time.monotonic() + seconds
        while (left := deadline - time.monotonic()) > 0:
            try:
                self.events.append(json.loads(self.ws.recv(timeout=left)))
            except TimeoutError:
                break
        out = list(self.events)
        self.events.clear()
        return out
