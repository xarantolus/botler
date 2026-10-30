"""Transport to the hub's ops API (`POST /api/v1/ops/{name}`). Standard library only."""

from __future__ import annotations

import json
import os
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


def call(op: str, args: dict[str, Any]) -> Any:
    """Run hub operation `op`. Arguments that are None are left out (the hub applies defaults)."""
    url = _env("BOTLER_URL", op).rstrip("/") + f"/api/v1/ops/{op}"
    body = json.dumps({k: v for k, v in args.items() if v is not None}).encode()
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
        raw = e.read().decode(errors="replace")
        try:
            message = str(json.loads(raw)["error"])
        except (ValueError, KeyError, TypeError):
            message = raw or e.reason
        raise BotlerError(op, _CODES.get(e.code, "internal"), message, e.code) from e
    except (urllib.error.URLError, OSError) as e:
        raise BotlerError(op, "unreachable", f"cannot reach the hub at {url}: {e}") from e
