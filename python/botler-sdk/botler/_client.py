"""Transport to the hub's ops API (`POST /api/v1/ops/{name}`). Standard library only.

Containers reach the hub on its Unix socket (`BOTLER_SOCKET`), which needs no network; without
one (tests, tools run elsewhere) the SDK uses `BOTLER_URL`."""

from __future__ import annotations

import http.client
import json
import os
import socket
import time
import urllib.error
import urllib.request
from typing import Any

# The hub reports failures as an HTTP status plus `{"error": message}`.
_CODES = {
    400: "bad_request",
    401: "unauthorized",
    403: "forbidden",
    404: "not_found",
    409: "conflict",
    422: "invalid_event",
    429: "rate_limited",
    503: "paused",
}

TIMEOUT_SECONDS = 60.0
# How long a call waits for a hub that is not taking connections (it restarts, e.g. for an
# upgrade, in well under this) before raising `unreachable`.
RECONNECT_SECONDS = 90.0


class BotlerError(Exception):
    """A hub call failed. `code` is a stable name (`forbidden`, `invalid_event`, `unreachable`, ...),
    `status` the HTTP status (None if the hub was not reached), `message` the hub's explanation."""

    def __init__(self, op: str, code: str, message: str, status: int | None = None) -> None:
        super().__init__(f"{op}: {code}: {message}")
        self.op = op
        self.code = code
        self.message = message
        self.status = status


def _env(name: str, op: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise BotlerError(op, "not_configured", f"{name} is not set; the hub sets it in containers it starts")
    return value


class _UnixConnection(http.client.HTTPConnection):
    def __init__(self, path: str) -> None:
        super().__init__("hub", timeout=TIMEOUT_SECONDS)
        self._path = path

    def connect(self) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(TIMEOUT_SECONDS)
        self.sock.connect(self._path)


def _error(op: str, status: int, raw: str, reason: str) -> BotlerError:
    try:
        error = json.loads(raw)["error"]
        # The model gateway answers in OpenAI's shape: {"error": {"message": ...}}.
        message = str(error["message"] if isinstance(error, dict) and "message" in error else error)
    except (ValueError, KeyError, TypeError):
        message = raw or reason
    return BotlerError(op, _CODES.get(status, "internal"), message, status)


class _NotConnected(Exception):
    """The hub did not take the connection: the request was never sent, so it can be retried."""


def _call_socket(op: str, route: str, path: str, body: bytes, token: str) -> Any:
    conn = _UnixConnection(path)
    try:
        try:
            conn.connect()
        except OSError as e:
            raise _NotConnected(f"cannot reach the hub at {path}: {e}") from e
        conn.request(
            "POST",
            route,
            body=body,
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        )
        resp = conn.getresponse()
        raw = resp.read()
    except OSError as e:
        raise BotlerError(op, "unreachable", f"lost the hub at {path} during the call: {e}") from e
    finally:
        conn.close()
    if resp.status >= 400:
        raise _error(op, resp.status, raw.decode(errors="replace"), resp.reason)
    return json.loads(raw or b"null")


def _call_url(op: str, route: str, base: str, body: bytes, token: str) -> Any:
    url = base.rstrip("/") + route
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as resp:
            return json.loads(resp.read() or b"null")
    except urllib.error.HTTPError as e:
        raise _error(op, e.code, e.read().decode(errors="replace"), str(e.reason)) from e
    except urllib.error.URLError as e:
        if isinstance(e.reason, (ConnectionRefusedError, FileNotFoundError)):
            raise _NotConnected(f"cannot reach the hub at {url}: {e}") from e
        raise BotlerError(op, "unreachable", f"cannot reach the hub at {url}: {e}") from e
    except OSError as e:
        raise BotlerError(op, "unreachable", f"lost the hub at {url} during the call: {e}") from e


def call(op: str, args: dict[str, Any]) -> Any:
    """Run hub operation `op`. Arguments that are None are left out (the hub applies defaults).

    While the hub restarts it takes no connections; the call then waits for it (up to
    `RECONNECT_SECONDS`) instead of failing, so a service survives hub upgrades."""
    return post(f"/api/v1/ops/{op}", op, {k: v for k, v in args.items() if v is not None})


def post(route: str, op: str, payload: Any) -> Any:
    """POST `payload` as JSON to the hub's `route`; `op` names the call in errors."""
    body = json.dumps(payload).encode()
    sock = os.environ.get("BOTLER_SOCKET")
    token = _env("BOTLER_TOKEN", op)
    base = None if sock else _env("BOTLER_URL", op)
    deadline = time.monotonic() + RECONNECT_SECONDS
    delay = 0.25
    while True:
        try:
            if sock:
                return _call_socket(op, route, sock, body, token)
            return _call_url(op, route, base or "", body, token)
        except _NotConnected as e:
            if time.monotonic() + delay > deadline:
                raise BotlerError(op, "unreachable", f"{e} (waited {RECONNECT_SECONDS:.0f} s)") from e
            time.sleep(delay)
            delay = min(delay * 2, 5.0)
