"""The botler SDK: how jobs, services, tools and agent scripts reach the hub.

The hub mounts this package at /opt/botler/sdk in every container it starts and sets
`BOTLER_SOCKET` (or `BOTLER_URL`), `BOTLER_TOKEN` and `PYTHONPATH`. The runtime part is standard library only;
`Event` and `tool` need pydantic and are imported lazily.

Every hub operation is in `botler.ops` (generated from the hub); the common ones are here.
Failures raise `BotlerError` with the hub's error code and message.
"""

from __future__ import annotations

import sys
from typing import TYPE_CHECKING, Any

from . import _ops as ops
from ._client import BotlerError

if TYPE_CHECKING:
    from .events import Event
    from .settings import Settings
    from .tools import tool

__all__ = [
    "BotlerError",
    "Event",
    "Settings",
    "action_args",
    "emit",
    "health",
    "log",
    "ops",
    "send_message",
    "set_timer",
    "state",
    "tool",
]


def action_args() -> dict[str, Any]:
    """Actions: the arguments the user approved for this run. Print the result as the last line of
    stdout (JSON when it is structured); it reaches the invoking agent as `action.result`."""
    import json
    import os

    path = os.environ.get("BOTLER_ACTION_ARGS")
    if not path:
        raise BotlerError("action_args", "not_configured", "BOTLER_ACTION_ARGS is not set; this is not an action run")
    with open(path, encoding="utf-8") as f:
        args = json.load(f)
    if not isinstance(args, dict):
        raise BotlerError("action_args", "bad_request", f"the approved arguments are not an object: {args!r}")
    return args


def emit(event: Any, payload: dict[str, Any] | None = None, *, provenance: str | None = None) -> Any:
    """Emit an event: `emit(MyEvent(...))` or `emit("mail.received", {...})`. Set
    `provenance="untrusted"` for content from arbitrary outside sources."""
    if isinstance(event, str):
        kind, body = event, payload if payload is not None else {}
    else:
        if payload is not None:
            raise TypeError("pass either an Event instance or a type and a payload, not both")
        kind, body = event.type, event.model_dump(mode="json")
    return ops.emit_event(type=kind, payload=body, provenance=provenance)


def send_message(to: str, body: str, *, urgent: bool = False, reply_to: str | None = None) -> Any:
    """Send a message to an agent instance or chat (e.g. `main`)."""
    return ops.send_message(to=to, text=body, urgent=urgent, reply_to=reply_to)


def set_timer(prompt: str, *, at: str | None = None, after: str | None = None, instance: str | None = None) -> Any:
    """Wake an agent later with `prompt`: `at` an RFC 3339 time or `after` a delay like "2h"."""
    return ops.set_timer(prompt=prompt, at=at, in_=after, instance=instance)


class _State:
    """This job's persistent key-value state (cursors, dedup keys; values up to 64 KiB)."""

    def get(self, key: str) -> str | None:
        value = ops.state_get(key=key)["value"]
        return None if value is None else str(value)

    def set(self, key: str, value: str) -> None:
        ops.state_set(key=key, value=value)


state = _State()


class _Health:
    """Services: report health. Call `ok()` at least as often as `[health] heartbeat` in job.toml;
    `fail(reason)` stops the service, reports it unhealthy and restarts it with backoff."""

    def ok(self) -> None:
        ops.health(ok=True)

    def fail(self, reason: str) -> None:
        ops.health(ok=False, reason=reason)


health = _Health()


def log(message: str, **fields: Any) -> None:
    """A log line on stderr (stdout's last line is the run's output). It ends up in the run logs."""
    extra = "".join(f" {k}={v!r}" for k, v in fields.items())
    print(f"{message}{extra}", file=sys.stderr, flush=True)


def __getattr__(name: str) -> Any:
    # pydantic is only needed for authoring; keep plain `import botler` stdlib-only.
    if name == "Event":
        from .events import Event

        return Event
    if name == "tool":
        from .tools import tool

        return tool
    if name == "Settings":
        from .settings import Settings

        return Settings
    raise AttributeError(f"module 'botler' has no attribute {name!r}")
