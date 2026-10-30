"""Typed events: subclass `Event`, set `type`, and the gate generates `events.manifest.json`.

    class MailReceived(botler.Event):
        \"\"\"A new mail arrived in a watched folder.\"\"\"
        type: ClassVar[str] = "mail.received"
        sender: str
        subject: str

    botler.emit(MailReceived(sender="a@b.c", subject="hi"))

Needs pydantic (authoring and gate time); the rest of the SDK does not.
"""

from __future__ import annotations

import sys
from typing import Any, ClassVar

from pydantic import BaseModel

from ._discover import import_all, manifest_parser, write_manifest


class Event(BaseModel):
    """Base class of typed event payloads. Subclasses set `type` (dotted, e.g. `mail.received`);
    the class docstring becomes the event's description. `restricted = True` (e.g. raw mail
    contents) means chats need an `event:<type>` access grant to subscribe to it."""

    type: ClassVar[str]
    restricted: ClassVar[bool] = False


def _all_subclasses(cls: type[Event]) -> list[type[Event]]:
    out: list[type[Event]] = []
    for sub in cls.__subclasses__():
        out.append(sub)
        out.extend(_all_subclasses(sub))
    return out


def manifest() -> list[dict[str, Any]]:
    """Every imported `Event` subclass in the format of `events.manifest.json`."""
    entries: dict[str, dict[str, Any]] = {}
    for cls in _all_subclasses(Event):
        kind = getattr(cls, "type", None)
        if not isinstance(kind, str):
            raise TypeError(f"{cls.__module__}.{cls.__qualname__} subclasses botler.Event but sets no `type`")
        if kind in entries:
            raise ValueError(f"event type {kind} is declared twice ({cls.__module__}.{cls.__qualname__})")
        schema = cls.model_json_schema()
        entries[kind] = {
            "type": kind,
            "description": (cls.__dict__.get("__doc__") or "").strip(),
            "schema": schema,
        }
        if cls.restricted:
            entries[kind]["restricted"] = True
    return [entries[k] for k in sorted(entries)]


def main(argv: list[str] | None = None) -> int:
    parser, _ = manifest_parser("botler.events", "events.manifest.json")
    args = parser.parse_args(argv)
    import_all(args.root)
    return write_manifest("botler.events", args.root / "events.manifest.json", manifest(), args.check)


if __name__ == "__main__":
    # Run the importable module's main so user code and this command share one `Event` class.
    from botler.events import main as _main

    sys.exit(_main())
