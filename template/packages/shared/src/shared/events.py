"""Typed events. Subclass `botler.Event`; the gate generates `events.manifest.json` from these."""

from typing import ClassVar
from uuid import UUID

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


class ClaudeSessionRequested(botler.Event):
    """Start a Claude Code session on the user's subscription (jobs/claude-session, skill
    `claude-code`), or send one a message: `resume` is its id, and the message reaches it even while
    it works; `interrupt` stops its current work first. `repo` is cloned into a new session's
    working directory (`monorepo`: botler's own, with `monorepo = true` on the job; the session
    pushes `agents/<chat>/claude-…` for the requesting chat to propose);
    `notify` is the chat that hears back (default: the emitter)."""

    type: ClassVar[str] = "claude_session.requested"
    task: str
    repo: str | None = None
    resume: UUID | None = None
    interrupt: bool = False
    notify: str | None = None


class ClaudeSessionFinished(botler.Event):
    """A turn of a Claude Code session ended (jobs/claude-session). `result` is Claude's final
    answer or the failure, cut to a few thousand characters; continue with `resume = session_id`."""

    type: ClassVar[str] = "claude_session.finished"
    session_id: str
    ok: bool
    result: str
    notify: str
    cost_usd: float | None = None
