"""Pydantic v2 models for the Discord objects, hub shapes and persisted state the bridge uses.

Every external payload is parsed once here (`model_validate`) and typed fields are used everywhere
else. Discord adds fields freely, so each model ignores what it does not read. Field names are the
wire (snake_case) names.
"""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict, Field

# Message types the user can send by typing: DEFAULT (0) and REPLY (19).
USER_MESSAGE_TYPES = frozenset({0, 19})
# Channel type of a one-to-one DM.
DM_CHANNEL_TYPE = 1
# Application command types: CHAT_INPUT (1), USER (2) and MESSAGE (3).
CHAT_INPUT_COMMAND_TYPE = 1
MESSAGE_COMMAND_TYPE = 3
# Interaction contexts: a guild channel (0), the bot's own DM (1), any other DM or group DM (2).
GUILD_CONTEXT = 0
BOT_DM_CONTEXT = 1
PRIVATE_CHANNEL_CONTEXT = 2


class Model(BaseModel):
    model_config = ConfigDict(extra="ignore")


# -- Discord ----------------------------------------------------------------------------------
class User(Model):
    id: str = ""
    username: str = ""
    bot: bool = False


class Channel(Model):
    id: str = ""
    type: int = DM_CHANNEL_TYPE


class Attachment(Model):
    id: str = ""
    filename: str = "file"
    url: str = ""
    size: int = 0
    content_type: str = ""


class MessageReference(Model):
    message_id: str = ""


class Message(Model):
    id: str = ""
    channel_id: str = ""
    content: str = ""
    type: int = 0
    author: User = Field(default_factory=User)
    attachments: list[Attachment] = []
    message_reference: MessageReference | None = None
    # Present on guild messages only; the gateway listener drops those.
    guild_id: str | None = None
    # The message this one replies to, when Discord (or the gateway cache) supplies it.
    referenced_message: Message | None = None


class CommandOption(Model):
    name: str = ""
    value: object = None


class ResolvedData(Model):
    """The `resolved` part of a command interaction: objects Discord already sent us. For a MESSAGE
    command the target message is here (`target_id` names it), so nothing has to be fetched."""

    messages: dict[str, Message] = {}


class CommandData(Model):
    name: str = ""
    type: int = CHAT_INPUT_COMMAND_TYPE
    options: list[CommandOption] = []
    target_id: str = ""
    resolved: ResolvedData = Field(default_factory=ResolvedData)


class Member(Model):
    user: User = Field(default_factory=User)


class Interaction(Model):
    """An application-command interaction (gateway INTERACTION_CREATE). `context` is Discord's
    interaction context: 0 guild channel, 1 the bot's own DM, 2 another DM or a group DM."""

    id: str = ""
    token: str = ""
    application_id: str = ""
    context: int = 0
    guild_id: str | None = None
    user: User | None = None  # set in DMs
    member: Member | None = None  # set in guilds
    data: CommandData = Field(default_factory=CommandData)
    # Filled in by the client for a MESSAGE command when Discord (or the cache) tells us the names.
    channel_name: str = ""
    guild_name: str = ""
    # The text the user typed in the comment modal ("" when they left it empty).
    comment: str = ""
    # The raw `data` dict exactly as Discord sent it, so the whole payload (resolved, snapshots,
    # fields we do not model) can be forwarded without losing anything.
    raw_data: dict[str, Any] = {}

    @property
    def sender(self) -> str:
        """The invoking user's id."""
        if self.member and self.member.user.id:
            return self.member.user.id
        return self.user.id if self.user else ""

    @property
    def ephemeral(self) -> bool:
        """Only the bot's own DM shows replies to everyone in it; anywhere else they are private."""
        return self.context != BOT_DM_CONTEXT

    @property
    def target(self) -> Message | None:
        """The message a MESSAGE context-menu command was invoked on, from `resolved` (no fetch)."""
        if not self.data.target_id:
            return None
        return self.data.resolved.messages.get(self.data.target_id)

    def option(self, name: str) -> str:
        for option in self.data.options:
            if option.name == name and isinstance(option.value, str):
                return option.value.strip()
        return ""


class PendingInteraction(Model):
    """A deferred interaction waiting for the hub's reply (persisted in state)."""

    id: str = ""
    token: str = ""
    application_id: str = ""
    ephemeral: bool = True
    at: int = 0


# -- hub shapes --------------------------------------------------------------------------------
class InboundResult(Model):
    status: str = ""


class OutboundFile(Model):
    name: str = ""
    base64: str = ""
    mime: str = ""


class OutboxMessage(Model):
    id: str = ""
    sender: str = ""
    text: str = ""
    question: object = None
    files: list[OutboundFile] = []
    # Not in the documented outbox shape; honoured if the hub sends the outbox id being answered.
    reply_ref: str | None = None


class OutboxBatch(Model):
    messages: list[OutboxMessage] = []


class Question(Model):
    options: list[object] = []
    choices: list[object] = []


# -- persisted state ---------------------------------------------------------------------------
class PendingReaction(Model):
    channel_id: str = ""
    message_id: str = ""


class Pending(Model):
    """What is being handled for one sender: reactions to clear, and where to show typing."""

    reactions: list[PendingReaction] = []
    # The newest delivered message from the user: the one the next reply answers (a Discord reply).
    answering: str = ""


Message.model_rebuild()
