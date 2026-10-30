"""Transport to the hub's ops API (`POST /api/v1/ops/{name}`). Standard library only.

Containers reach the hub on its Unix socket (`BOTLER_SOCKET`), which needs no network; without
one (tests, tools run elsewhere) the SDK uses `BOTLER_URL`."""

from __future__ import annotations

import http.client
import json
import os
import socket
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
        message = str(json.loads(raw)["error"])
    except (ValueError, KeyError, TypeError):
        message = raw or reason
    return BotlerError(op, _CODES.get(status, "internal"), message, status)


def _call_socket(op: str, path: str, body: bytes, token: str) -> Any:
    conn = _UnixConnection(path)
    try:
        conn.request(
            "POST",
            f"/api/v1/ops/{op}",
            body=body,
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        )
        resp = conn.getresponse()
        raw = resp.read()
    except OSError as e:
        raise BotlerError(op, "unreachable", f"cannot reach the hub at {path}: {e}") from e
    finally:
        conn.close()
    if resp.status >= 400:
        raise _error(op, resp.status, raw.decode(errors="replace"), resp.reason)
    return json.loads(raw or b"null")


def call(op: str, args: dict[str, Any]) -> Any:
    """Run hub operation `op`. Arguments that are None are left out (the hub applies defaults)."""
    body = json.dumps({k: v for k, v in args.items() if v is not None}).encode()
    sock = os.environ.get("BOTLER_SOCKET")
    if sock:
        return _call_socket(op, sock, body, _env("BOTLER_TOKEN", op))
    url = _env("BOTLER_URL", op).rstrip("/") + f"/api/v1/ops/{op}"
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={"Authorization": f"Bearer {_env('BOTLER_TOKEN', op)}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as resp:
            return json.loads(resp.read() or b"null")
    except urllib.error.HTTPError as e:
        raise _error(op, e.code, e.read().decode(errors="replace"), str(e.reason)) from e
    except (urllib.error.URLError, OSError) as e:
        raise BotlerError(op, "unreachable", f"cannot reach the hub at {url}: {e}") from e
