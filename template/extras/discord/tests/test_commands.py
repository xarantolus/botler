"""Slash command and message context-menu tests: parsing, the ephemeral ack, and the rule that
every reply lands in the DM rather than the interaction follow-up."""

from __future__ import annotations

import base64
import json
from typing import Any

from discord_bot.models import Interaction, OutboundFile, OutboxMessage
from fakes import DM, USER, msg
from test_bridge import RECENT, Rig


def message_command(
    *,
    context: int = 0,
    content: str = "look at this",
    comment: str = "",
    attachments: list[dict[str, Any]] | None = None,
    id: str = "m1",
    guild_id: str | None = "g1",
    channel_name: str = "",
    guild_name: str = "",
    author: str = "42",
    username: str = "someone",
) -> Interaction:
    """A wire-shaped MESSAGE (type 3) context-menu interaction, as the client would hand it to the
    bridge after the comment modal was submitted."""
    target: dict[str, Any] = {
        "id": "500",
        "channel_id": "c1",
        "content": content,
        "type": 0,
        "author": {"id": author, "username": username, "bot": False},
        "attachments": attachments or [],
    }
    payload: dict[str, Any] = {
        "id": id,
        "token": "tok",
        "application_id": "app",
        "type": 2,
        "context": context,
        "guild_id": guild_id,
        "channel_name": channel_name,
        "guild_name": guild_name,
        "comment": comment,
        "data": {
            "name": "Send to Botler",
            "type": 3,
            "target_id": "500",
            "resolved": {"messages": {"500": target}},
        },
    }
    if context == 0:
        payload["member"] = {"user": {"id": USER}}
    else:
        payload["user"] = {"id": USER}
    return Interaction.model_validate(payload)


def test_message_command_forwards_the_raw_content_source_author_and_comment() -> None:
    rig = Rig()
    rig.discord.downloads["https://cdn.discordapp.com/attachments/1/2/pic.png?ex=69&is=69&hm=x"] = b"PNG"
    attachments = [
        {
            "id": "1",
            "filename": "pic.png",
            "url": "https://cdn.discordapp.com/attachments/1/2/pic.png?ex=69&is=69&hm=x",
            "size": 42,
            "content_type": "image/png",
        }
    ]
    assert rig.bridge.handle_message_command(
        message_command(
            content="hello <:kek:123> and <a:party:456>", comment="for you", attachments=attachments
        )
    )
    call = rig.inbound.calls[0]
    assert call["sender"] == USER and "reply_ref" not in call  # replies go to the DM, not the interaction
    # The client answered the interaction itself; the bridge only forwards and types in the DM.
    assert rig.discord.followups == []
    assert rig.discord.typed == [DM]  # typing shows where the reply will land
    text = call["text"]
    assert "[forwarded from Discord:" in text
    assert "source server unknown to me" in text  # bot is not in g1
    assert "author: someone (42)" in text
    assert "https://discord.com/channels/g1/c1/500" in text
    assert "hello <:kek:123> and <a:party:456>" in text  # raw markup survives
    assert "[comment]: for you" in text
    # Attachments carry filename, content type, size and Discord's signed URL (unchanged).
    assert (
        "- pic.png (image/png, 42 bytes): https://cdn.discordapp.com/attachments/1/2/pic.png?ex=69&is=69&hm=x"
        in text
    )
    # Custom emoji get their CDN image URL (.png, .gif when animated).
    assert "<:kek:123> -> https://cdn.discordapp.com/emojis/123.png" in text
    assert "<:party:456> -> https://cdn.discordapp.com/emojis/456.gif" in text


def test_message_command_names_the_channel_and_guild_when_known() -> None:
    rig = Rig()
    rig.bridge.handle_message_command(
        message_command(content="hi", channel_name="general", guild_name="test-server")
    )
    assert "#general in test-server" in rig.inbound.calls[0]["text"]


def test_message_command_contexts_are_marked() -> None:
    rig = Rig()
    rig.bridge.handle_message_command(message_command(context=1, guild_id=None))
    rig.bridge.handle_message_command(message_command(context=2, guild_id=None, id="m2"))
    assert "a DM with me" in rig.inbound.calls[0]["text"]
    assert "a private channel" in rig.inbound.calls[1]["text"]


def test_message_command_with_only_an_attachment_is_not_dropped() -> None:
    rig = Rig()
    rig.discord.downloads["https://cdn.discordapp.com/a.png"] = b"PNG"
    attachments = [
        {
            "id": "1",
            "filename": "a.png",
            "url": "https://cdn.discordapp.com/a.png",
            "size": 3,
            "content_type": "image/png",
        }
    ]
    assert rig.bridge.handle_message_command(message_command(content="", attachments=attachments))
    call = rig.inbound.calls[0]
    assert call["files"] == [{"name": "a.png", "base64": base64.b64encode(b"PNG").decode()}]
    assert "(no text)" in call["text"]
    assert "https://cdn.discordapp.com/a.png" in call["text"]  # the URL is passed through too


def test_message_command_from_an_unpaired_user_is_refused_privately() -> None:
    rig = Rig({"status": "ignored"})
    assert not rig.bridge.handle_message_command(message_command())
    ((pending, text, _),) = rig.discord.followups
    assert "Not authorized" in text and pending.ephemeral
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="dm"))  # nothing stale is kept
    assert len(rig.discord.followups) == 1 and rig.discord.sent[0]["channel"] == DM


def test_message_command_without_a_target_is_explained() -> None:
    rig = Rig()
    interaction = message_command()
    interaction.data.resolved.messages = {}
    assert not rig.bridge.handle_message_command(interaction)
    assert "nothing was forwarded" in rig.discord.followups[0][1]


def test_message_command_hub_failure_is_reported_not_raised() -> None:
    rig = Rig()
    rig.inbound.error = RuntimeError("hub restarting")
    assert not rig.bridge.handle_message_command(message_command())
    assert "could not be reached" in rig.discord.followups[0][1]


def test_a_reply_whose_files_discord_refuses_still_arrives_as_text() -> None:
    rig = Rig()
    rig.discord.fail_files = True  # e.g. an attachment over Discord's size limit: a permanent 400
    rig.bridge.send(
        OutboxMessage(
            id="o1",
            sender=USER,
            text="here is the picture",
            files=[OutboundFile(name="big.png", base64=base64.b64encode(b"x").decode())],
        )
    )
    # The text arrives without the file instead of the whole reply being dropped silently.
    assert [(s["text"], s["files"]) for s in rig.discord.sent] == [("here is the picture", [])]


def test_an_attachment_with_invalid_base64_does_not_drop_the_reply() -> None:
    rig = Rig()
    rig.bridge.send(
        OutboxMessage(id="o1", sender=USER, text="still here", files=[OutboundFile(name="bad", base64="abc")])
    )
    assert [(s["text"], s["files"]) for s in rig.discord.sent] == [("still here", [])]


def test_message_command_forwards_the_whole_interaction_payload_inline_when_small() -> None:
    rig = Rig()
    interaction = message_command(comment="note")
    interaction.raw_data = {
        "name": "Send to Botler",
        "type": 3,
        "target_id": "500",
        "resolved": {"messages": {"500": {"id": "500", "content": "raw here"}}},
        "message_snapshots": [{"message": {"content": "a forwarded snapshot"}}],
        "unknown_field": {"kept": True},
    }
    assert rig.bridge.handle_message_command(interaction)
    text = rig.inbound.calls[0]["text"]
    assert "payload:" in text and "inline below" in text
    for fragment in ('"message_snapshots"', '"unknown_field"', '"resolved"', '"note"'):
        assert fragment in text  # nothing dropped
    assert rig.inbound.calls[0]["files"] == []


def test_message_command_sends_a_large_payload_as_a_file_without_truncating() -> None:
    rig = Rig()
    interaction = message_command()
    interaction.raw_data = {"resolved": {"messages": {"500": {"content": "x" * 4000}}}}
    assert rig.bridge.handle_message_command(interaction)
    call = rig.inbound.calls[0]
    assert "full JSON in forwarded-message.json" in call["text"]
    (payload_file,) = call["files"]
    assert payload_file["name"] == "forwarded-message.json"
    forwarded = json.loads(base64.b64decode(payload_file["base64"]))
    assert forwarded["resolved"]["messages"]["500"]["content"] == "x" * 4000  # untruncated


def invocation(
    *, context: int = 0, options: list[dict[str, Any]] | None = None, id: str = "i1", name: str = "remind"
) -> Interaction:
    """A wire-shaped INTERACTION_CREATE payload (guild: `member.user`; DMs: `user`)."""
    payload: dict[str, Any] = {
        "id": id,
        "token": "tok",
        "application_id": "app",
        "type": 2,
        "context": context,
        "data": {
            "name": name,
            "type": 1,
            "options": options
            if options is not None
            else [
                {"name": "when", "type": 3, "value": " in 2 hours "},
                {"name": "what", "type": 3, "value": "call mum"},
            ],
        },
    }
    if context == 0:
        payload["guild_id"] = "g1"
        payload["member"] = {"user": {"id": USER}}
    else:
        payload["user"] = {"id": USER}
    return Interaction.model_validate(payload)


def test_option_parsing_and_sender() -> None:
    i = invocation()
    assert (i.option("when"), i.option("what")) == ("in 2 hours", "call mum")
    assert i.option("missing") == ""
    assert i.sender == USER
    assert invocation(context=1).sender == USER


def test_ephemeral_everywhere_except_the_bots_own_dm() -> None:
    assert invocation(context=0).ephemeral
    assert invocation(context=2).ephemeral
    assert not invocation(context=1).ephemeral


def test_command_is_forwarded_and_replies_go_to_the_dm() -> None:
    rig = Rig()
    assert rig.bridge.handle_command(invocation())
    assert rig.inbound.calls == [{"sender": USER, "text": "Remind me to call mum at in 2 hours"}]
    # The client answered the interaction itself; the bridge only forwards and types in the DM.
    assert rig.discord.followups == []
    assert rig.discord.typed == [DM]  # typing shows where the reply will land
    # The hub's reply is an ordinary DM, never an interaction follow-up.
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="Reminder set for 11:00."))
    assert [(s["channel"], s["text"]) for s in rig.discord.sent] == [(DM, "Reminder set for 11:00.")]
    assert rig.discord.followups == []  # no duplicate of the reply in the server


def test_reply_to_a_command_in_the_bots_dm_also_lands_in_the_dm() -> None:
    rig = Rig()
    rig.bridge.handle_command(invocation(context=1))
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="ok"))
    assert [s["channel"] for s in rig.discord.sent] == [DM]


def test_a_long_reply_is_split_across_dm_messages() -> None:
    rig = Rig()
    rig.bridge.handle_command(invocation())
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="x" * 2500))
    assert [len(s["text"]) for s in rig.discord.sent] == [2000, 500]


def test_a_reply_ref_survives_but_still_goes_to_the_dm() -> None:
    rig = Rig()
    rig.bridge.handle_command(invocation())
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="q"))
    rig.bridge.send(OutboxMessage(id="o2", sender=USER, text="answer", reply_ref="o1"))
    assert rig.discord.sent[-1]["channel"] == DM
    assert rig.discord.sent[-1]["reply_to"] == rig.state.get("sent.o1")  # threaded inside the DM


def test_a_newer_dm_does_not_change_where_the_reply_goes() -> None:
    rig = Rig()
    rig.bridge.handle_command(invocation())
    rig.bridge.synced = True
    rig.bridge.handle_message(msg(RECENT + 1, "actually, a question"))
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="answer"))
    assert rig.discord.sent[0]["channel"] == DM


def test_unpaired_invoker_is_refused_and_nothing_is_kept() -> None:
    rig = Rig({"status": "ignored"})
    assert not rig.bridge.handle_command(invocation())
    ((pending, text, _),) = rig.discord.followups
    assert "Not authorized" in text and pending.ephemeral
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="dm"))  # nothing stale is kept
    assert len(rig.discord.followups) == 1


def test_hub_failure_is_reported_to_the_user_not_raised() -> None:
    rig = Rig()
    rig.inbound.error = RuntimeError("hub restarting")
    assert not rig.bridge.handle_command(invocation())
    assert "could not be reached" in rig.discord.followups[0][1]


def test_bad_options_and_unknown_commands_get_an_explanation() -> None:
    rig = Rig()
    assert not rig.bridge.handle_command(invocation(options=[{"name": "when", "type": 3, "value": "x"}]))
    assert not rig.bridge.handle_command(invocation(name="other"))
    assert rig.inbound.calls == []
    assert "Usage" in rig.discord.followups[0][1] and "Unknown command" in rig.discord.followups[1][1]
