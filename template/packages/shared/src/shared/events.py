"""Typed events. Subclass `botler.Event`; the gate generates `events.manifest.json` from these."""

from typing import ClassVar

import botler


class ExampleGreeting(botler.Event):
    """An example event: someone was greeted. Replace it with real event types."""

    type: ClassVar[str] = "example.greeting"
    name: str


class ExampleTick(botler.Event):
    """The example watcher service (jobs/example-watcher) ticked: a template for watcher events."""

    type: ClassVar[str] = "example.tick"
    count: int


class ExamplePageItem(botler.Event):
    """Something the example browser service (jobs/browser-example) read from a page. Raw page
    content: restricted, so only chats granted it subscribe."""

    type: ClassVar[str] = "example.page_item"
    restricted: ClassVar[bool] = True
    id: str
    text: str
    url: str


class ExampleLoginNeeded(botler.Event):
    """The example browser service's session is logged out of `site`. Open a login visit with
    `request_visit` and `vnc_password` (valid until the login is back)."""

    type: ClassVar[str] = "example.login_needed"
    restricted: ClassVar[bool] = True
    site: str
    vnc_password: str


class ExampleLoggedIn(botler.Event):
    """The example browser service's session is logged in to `site` (checked over CDP)."""

    type: ClassVar[str] = "example.logged_in"
    site: str
