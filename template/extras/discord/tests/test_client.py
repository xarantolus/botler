"""Gateway wiring: intents, command registration and the discord.py -> model mapping."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from typing import Any

import discord
from discord import app_commands
from discord_bot.client import (
    Callbacks,
    CommentModal,
    Gateway,
    interaction_model,
    message_command_model,
    message_model,
)
from discord_bot.errors import DiscordError
from discord_bot.models import Interaction

USER = "42"


def callbacks() -> Callbacks:
    return Callbacks(
        on_ready=lambda: None,
        on_disconnect=lambda: None,
        on_message=lambda m: None,
        on_command=lambda i: None,
        on_message_command=lambda i: None,
        on_fatal=lambda r: None,
    )


def test_only_direct_message_intents_no_privileged_ones() -> None:
    gateway = Gateway(callbacks(), lambda *a, **k: None)
    intents = gateway.intents
    assert intents.dm_messages and not intents.guild_messages
    assert not intents.message_content and not intents.members and not intents.presences
    assert not intents.guilds


def test_remind_is_a_global_command_for_both_installs_and_all_contexts() -> None:
    gateway = Gateway(callbacks(), lambda *a, **k: None)
    (command,) = gateway.tree.get_commands(type=discord.AppCommandType.chat_input)
    assert isinstance(command, app_commands.Command)
    assert command.name == "remind"
    assert {p.name for p in command.parameters if p.required}  # pyright: ignore[reportUnknownVariableType, reportUnknownMemberType] == {"when", "what"}
    payload: dict[str, Any] = command.to_dict(gateway.tree)
    assert sorted(payload["integration_types"]) == [0, 1]  # guild + user install
    assert sorted(payload["contexts"]) == [0, 1, 2]  # guild, bot DM, private channels
    assert "guild_id" not in payload


def test_send_to_botler_is_a_global_message_command_with_both_installs_and_all_contexts() -> None:
    gateway = Gateway(callbacks(), lambda *a, **k: None)
    (command,) = gateway.tree.get_commands(type=discord.AppCommandType.message)
    assert isinstance(command, app_commands.ContextMenu)
    assert command.name == "Send to Botler"
    assert command.type is discord.AppCommandType.message
    payload: dict[str, Any] = command.to_dict(gateway.tree)
    assert payload["type"] == 3  # MESSAGE
    assert sorted(payload["integration_types"]) == [0, 1]  # guild + user install
    assert sorted(payload["contexts"]) == [0, 1, 2]  # guild, bot DM, private channels
    assert "guild_id" not in payload


def test_message_command_model_pulls_the_resolved_target_out_of_the_interaction() -> None:
    invocation: Any = SimpleNamespace(
        id=7,
        token="t",  # noqa: S106
        application_id=8,
        guild_id=99,
        guild=SimpleNamespace(name="test-server"),
        user=SimpleNamespace(id=USER),
        member=None,
        context=SimpleNamespace(guild=True, dm_channel=False),
        data={"name": "Send to Botler", "type": 3},
    )
    target: Any = SimpleNamespace(
        id=500,
        channel=SimpleNamespace(id=501, name="general"),
        content="hello <:kek:123>",
        type=discord.MessageType.default,
        author=SimpleNamespace(id=42, username="someone", bot=False),
        attachments=[],
        reference=None,
        guild=None,
    )
    model = message_command_model(invocation, target, "look at this")
    assert model.sender == USER and model.context == 0
    assert model.comment == "look at this"
    assert model.channel_name == "general" and model.guild_name == "test-server"
    assert model.target is not None and model.target.content == "hello <:kek:123>"
    assert model.target.channel_id == "501" and model.target.author.id == "42"


def test_comment_modal_answers_then_forwards_the_typed_comment() -> None:
    forwarded: list[Interaction] = []
    cb = Callbacks(
        on_ready=lambda: None,
        on_disconnect=lambda: None,
        on_message=lambda m: None,
        on_command=lambda i: None,
        on_message_command=forwarded.append,
        on_fatal=lambda r: None,
    )
    invocation: Any = SimpleNamespace(
        id=7,
        token="t",  # noqa: S106
        application_id=8,
        guild_id=None,
        guild=None,
        user=SimpleNamespace(id=USER),
        member=None,
        context=SimpleNamespace(guild=False, dm_channel=True),
        data={"name": "Send to Botler", "type": 3},
    )
    target: Any = SimpleNamespace(
        id=500,
        channel=SimpleNamespace(id=501, name=""),
        content="see this",
        type=discord.MessageType.default,
        author=SimpleNamespace(id=42, username="someone", bot=False),
        attachments=[],
        reference=None,
        guild=None,
    )

    class FakeResponse:
        def __init__(self) -> None:
            self.messages: list[tuple[str, bool]] = []

        async def send_message(self, content: str, *, ephemeral: bool = False) -> None:
            self.messages.append((content, ephemeral))

    response = FakeResponse()
    submit: Any = SimpleNamespace(id=123, token="mtok", application_id=8, response=response)  # noqa: S106
    modal = CommentModal(cb, invocation, target)
    modal.comment._value = "a comment"  # pyright: ignore[reportPrivateUsage]
    asyncio.run(modal.on_submit(submit))

    # The handler answers the interaction itself, at once and unconditionally.
    assert response.messages == [("Sent to Botler \u2014 I'll answer in our DM.", True)]
    (model,) = forwarded
    assert model.id == "123" and model.token == "mtok"  # noqa: S105
    assert model.comment == "a comment"
    assert model.target is not None and model.target.content == "see this"
    assert model.context == 1  # the bot's own DM, from the original invocation


def test_message_model_maps_a_discord_py_message() -> None:
    fake: Any = SimpleNamespace(
        id=11,
        channel=SimpleNamespace(id=22),
        content="hi",
        type=discord.MessageType.reply,
        author=SimpleNamespace(id=33, bot=False),
        attachments=[
            SimpleNamespace(
                id=1, filename="a.png", url="https://cdn.discordapp.com/a", size=4, content_type=None
            )
        ],
        reference=SimpleNamespace(message_id=9, resolved=None),
        guild=None,
    )
    m = message_model(fake)
    assert (m.id, m.channel_id, m.type, m.author.id) == ("11", "22", 19, "33")
    assert m.attachments[0].filename == "a.png" and m.message_reference is not None
    assert m.message_reference.message_id == "9" and m.guild_id is None
    assert m.referenced_message is None  # not cached by discord.py: the bridge fetches it


def test_interaction_model_maps_context_and_options() -> None:
    def fake(**context: bool) -> Any:  # noqa: ANN401
        return SimpleNamespace(
            id=1,
            token="t",  # noqa: S106
            application_id=2,
            guild_id=None,
            user=SimpleNamespace(id=3),
            context=SimpleNamespace(guild=context.get("guild", False), dm_channel=context.get("dm", False)),
            data={
                "name": "remind",
                "options": [{"name": "when", "value": "now"}, {"name": "what", "value": "x"}],
            },
        )

    assert interaction_model(fake(guild=True)).context == 0
    bot_dm = interaction_model(fake(dm=True))
    assert bot_dm.context == 1 and not bot_dm.ephemeral and bot_dm.sender == "3"
    assert interaction_model(fake()).context == 2
    assert bot_dm.option("what") == "x"


def test_download_refuses_anything_but_discords_cdn() -> None:
    from discord_bot.client import DiscordClient

    client = DiscordClient("x", callbacks(), lambda *a, **k: None)
    for url in ("http://cdn.discordapp.com/a", "https://evil.example/a", "https://169.254.169.254/x"):
        try:
            client.download(url)
        except DiscordError as e:
            assert e.permanent
        else:
            raise AssertionError(url)


def test_message_model_carries_a_resolved_reply_target() -> None:
    from unittest.mock import MagicMock

    original = MagicMock(spec=discord.Message)
    original.id, original.content, original.attachments = 5, "the original", []
    original.channel = SimpleNamespace(id=22)
    original.author = SimpleNamespace(id=900, name="botler", bot=True)
    fake: Any = SimpleNamespace(
        id=11,
        channel=SimpleNamespace(id=22),
        content="yes",
        type=discord.MessageType.reply,
        author=SimpleNamespace(id=33, bot=False),
        attachments=[],
        reference=SimpleNamespace(message_id=5, resolved=original),
        guild=None,
    )
    m = message_model(fake)
    assert m.referenced_message is not None
    assert (m.referenced_message.content, m.referenced_message.author.id) == ("the original", "900")
