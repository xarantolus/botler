"""Fakes shared by the tests: Discord, state and hub ops. No network, no hub, no token."""

from __future__ import annotations

from typing import Any

from discord_bot.errors import DiscordError
from discord_bot.models import Channel, Message, PendingInteraction

BOT = "900"
USER = "100000000000000001"
DM = "5000"


class FakeState:
    def __init__(self) -> None:
        self.data: dict[str, str] = {}

    def get(self, key: str) -> str | None:
        return self.data.get(key)

    def set(self, key: str, value: str) -> None:
        self.data[key] = value


class Hub:
    """A fake hub op recording its keyword arguments and answering `result`."""

    def __init__(self, result: dict[str, Any] | None = None) -> None:
        self.calls: list[dict[str, Any]] = []
        self.result = {"status": "delivered"} if result is None else result
        self.error: Exception | None = None

    def __call__(self, **kwargs: object) -> dict[str, Any]:
        self.calls.append(dict(kwargs))
        if self.error:
            raise self.error
        return self.result


def msg(
    id: int,
    content: str = "hi",
    *,
    author: str = USER,
    channel: str = DM,
    type: int = 0,
    attachments: list[dict[str, Any]] | None = None,
    reference: str | None = None,
    guild: str | None = None,
    referenced: Message | None = None,
    username: str = "",
) -> Message:
    return Message.model_validate(
        {
            "id": str(id),
            "channel_id": channel,
            "content": content,
            "type": type,
            "author": {"id": author, "bot": author == BOT, "username": username},
            "attachments": attachments or [],
            "message_reference": {"message_id": reference} if reference else None,
            "guild_id": guild,
            "referenced_message": referenced.model_dump() if referenced else None,
        }
    )


class FakeDiscord:
    bot_id = BOT

    def __init__(self) -> None:
        self.history: dict[str, list[Message]] = {}
        self.listed: list[str] | None = []  # None: listing fails
        self.sent: list[dict[str, Any]] = []
        self.followups: list[tuple[PendingInteraction, str, list[tuple[str, bytes]]]] = []
        self.reactions: list[tuple[str, str, str]] = []
        self.removed: list[tuple[str, str, str]] = []
        self.typed: list[str] = []
        self.opened: list[str] = []
        self.fetched: list[str] = []
        self.downloads: dict[str, bytes] = {}
        self.fail_history = False
        self.fail_remove: DiscordError | None = None
        self.fail_send_at: int | None = None
        self.send_error: DiscordError | None = None
        self.fail_files = False
        self.followup_error: DiscordError | None = None
        self._next = 7000

    def dm_channels(self) -> list[Channel]:
        if self.listed is None:
            raise DiscordError("403 not allowed", permanent=True)
        return [Channel(id=c) for c in self.listed]

    def messages_after(self, channel_id: str, after: str, limit: int) -> list[Message]:
        if self.fail_history:
            raise DiscordError("Discord is unreachable")
        found = [m for m in self.history.get(channel_id, []) if int(m.id) > int(after)]
        # Discord returns newest first; the bridge must not depend on it.
        return sorted(found, key=lambda m: -int(m.id))[:limit] if len(found) <= limit else found[:limit]

    def get_message(self, channel_id: str, message_id: str) -> Message:
        self.fetched.append(message_id)
        for m in self.history.get(channel_id, []):
            if m.id == message_id:
                return m
        raise DiscordError("404 unknown message", permanent=True)

    def open_dm(self, user_id: str) -> Channel:
        self.opened.append(user_id)
        return Channel(id=DM)

    def send(
        self, channel_id: str, text: str, files: list[tuple[str, bytes]], reply_to: str | None
    ) -> Message:
        if self.send_error:
            raise self.send_error
        if files and self.fail_files:
            raise DiscordError("400 request entity too large", permanent=True)
        if self.fail_send_at is not None and len(self.sent) == self.fail_send_at:
            self.fail_send_at = None
            raise DiscordError("Discord is unreachable")
        self._next += 1
        self.sent.append({"channel": channel_id, "text": text, "files": files, "reply_to": reply_to})
        return msg(self._next, text, author=BOT, channel=channel_id)

    def add_reaction(self, channel_id: str, message_id: str, emoji: str) -> None:
        self.reactions.append((channel_id, message_id, emoji))

    def remove_reaction(self, channel_id: str, message_id: str, emoji: str) -> None:
        if self.fail_remove:
            raise self.fail_remove
        self.removed.append((channel_id, message_id, emoji))

    def typing(self, channel_id: str) -> None:
        self.typed.append(channel_id)

    def followup(self, interaction: PendingInteraction, text: str, files: list[tuple[str, bytes]]) -> str:
        if self.followup_error:
            raise self.followup_error
        self.followups.append((interaction, text, files))
        return f"f{len(self.followups)}"

    def download(self, url: str) -> bytes:
        return self.downloads[url]
