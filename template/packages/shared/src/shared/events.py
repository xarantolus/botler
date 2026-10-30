"""Typed events. Subclass `botler.Event`; the gate generates `events.manifest.json` from these."""

from typing import ClassVar

import botler


class ExampleGreeting(botler.Event):
    """An example event: someone was greeted. Replace it with real event types."""

    type: ClassVar[str] = "example.greeting"
    name: str
