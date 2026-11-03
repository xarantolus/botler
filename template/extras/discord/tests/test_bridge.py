"""Bridge tests: catch-up, live dedupe, reactions, typing and outbound, all against fakes."""

from __future__ import annotations

import base64
from typing import Any

import pytest
from discord_bot.bridge import PAGE, Bridge, snowflake_at, split_text
from discord_bot.errors import DiscordError
from discord_bot.models import OutboxMessage
from discord_bot.settings import Settings
from fakes import BOT, DM, USER, FakeDiscord, FakeState, Hub, msg

NOW = 1_800_000_000_000  # ms
OLD = int(snowflake_at(NOW - 5 * 3600 * 1000))  # five hours ago: outside the first-sight window
RECENT = int(snowflake_at(NOW - 60 * 1000))


class Rig:
    def __init__(self, hub_result: dict[str, Any] | None = None) -> None:
        self.discord = FakeDiscord()
        self.state = FakeState()
        self.inbound = Hub(hub_result)
        self.outbox = Hub({"messages": []})
        self.ack = Hub({})
        self.logs: list[tuple[str, dict[str, Any]]] = []
        self.clock = NOW
        self.bridge = Bridge(
            self.discord,
            Settings.model_validate({}),
            state=self.state,
            inbound=self.inbound,
            outbox=self.outbox,
            ack=self.ack,
            now=lambda: self.clock,
            log=self._log,
        )

    def _log(self, event: str, /, **fields: object) -> None:
        self.logs.append((event, dict(fields)))

    def texts(self) -> list[str]:
        return [c["text"] for c in self.inbound.calls]


@pytest.fixture
def rig() -> Rig:
    return Rig()


# -- catch-up ----------------------------------------------------------------------------------
def test_catch_up_delivers_missed_messages_in_order_and_sets_cursor(rig: Rig) -> None:
    rig.discord.listed = [DM]
    rig.state.set(f"cursor.{DM}", str(RECENT))
    rig.discord.history[DM] = [
        msg(RECENT + 2, "b"),
        msg(RECENT + 1, "a"),
        msg(RECENT + 3, "mine", author=BOT),
    ]
    assert rig.bridge.catch_up() == 2
    assert rig.texts() == ["a", "b"]
    assert all(c["sender"] == USER for c in rig.inbound.calls)
    assert rig.state.get(f"cursor.{DM}") == str(RECENT + 3)  # advanced past the bot's own message
    assert rig.bridge.synced
    # A second catch-up (a reconnect) handles nothing twice.
    assert rig.bridge.catch_up() == 0
    assert rig.texts() == ["a", "b"]


def test_catch_up_paginates(rig: Rig) -> None:
    rig.discord.listed = [DM]
    rig.state.set(f"cursor.{DM}", str(RECENT))
    count = PAGE * 2 + 7
    rig.discord.history[DM] = [msg(RECENT + i, f"m{i}") for i in range(1, count + 1)]
    assert rig.bridge.catch_up() == count
    assert rig.texts() == [f"m{i}" for i in range(1, count + 1)]


def test_first_sight_looks_back_a_bounded_time(rig: Rig) -> None:
    rig.discord.listed = [DM]
    rig.discord.history[DM] = [msg(OLD, "ancient"), msg(RECENT, "pair abc")]
    rig.bridge.catch_up()
    assert rig.texts() == ["pair abc"]


def test_catch_up_uses_saved_channels_when_discord_cannot_list(rig: Rig) -> None:
    rig.discord.listed = [DM]
    rig.bridge.catch_up()  # learns the channel
    rig.discord.listed = None
    rig.discord.history[DM] = [msg(RECENT + 1, "while down")]
    rig.state.set(f"cursor.{DM}", str(RECENT))
    assert rig.bridge.catch_up() == 1
    assert any("could not list DM channels" in m for m, _ in rig.logs)


def test_catch_up_failure_keeps_cursor_and_stays_unsynced(rig: Rig) -> None:
    rig.discord.listed = [DM]
    rig.state.set(f"cursor.{DM}", str(RECENT))
    rig.discord.history[DM] = [msg(RECENT + 1, "a"), msg(RECENT + 2, "b")]
    rig.inbound.error = RuntimeError("hub restarting")
    with pytest.raises(RuntimeError):
        rig.bridge.catch_up()
    assert not rig.bridge.synced
    assert rig.state.get(f"cursor.{DM}") == str(RECENT)  # nothing lost
    rig.inbound.error = None
    assert rig.bridge.catch_up() == 2


def test_discord_failure_during_catch_up_raises_for_retry(rig: Rig) -> None:
    rig.discord.listed = [DM]
    rig.discord.fail_history = True
    with pytest.raises(DiscordError):
        rig.bridge.catch_up()
    assert not rig.bridge.synced


# -- live events -------------------------------------------------------------------------------
def test_live_message_stands_down_until_synced_then_dedupes(rig: Rig) -> None:
    rig.discord.listed = [DM]
    rig.state.set(f"cursor.{DM}", str(RECENT))
    live = msg(RECENT + 1, "live")
    rig.discord.history[DM] = [live]
    assert not rig.bridge.handle_message(live)  # not synced: left for the catch-up
    assert rig.inbound.calls == []
    rig.bridge.catch_up()
    assert rig.texts() == ["live"]
    assert not rig.bridge.handle_message(live)  # a RESUME replay of the same event
    assert rig.texts() == ["live"]


def test_live_message_is_delivered_and_advances_cursor(rig: Rig) -> None:
    rig.bridge.synced = True
    assert rig.bridge.handle_message(msg(RECENT + 5, "hello"))
    assert rig.state.get(f"cursor.{DM}") == str(RECENT + 5)
    assert not rig.bridge.handle_message(msg(RECENT + 4, "older replay"))
    assert rig.texts() == ["hello"]


def test_live_ignores_guild_bot_own_and_system_messages(rig: Rig) -> None:
    rig.bridge.synced = True
    assert not rig.bridge.handle_message(msg(RECENT + 1, "g", guild="1"))
    assert not rig.bridge.handle_message(msg(RECENT + 2, "own", author=BOT))
    assert not rig.bridge.handle_message(msg(RECENT + 3, "pinned", type=6))
    assert rig.inbound.calls == []


def test_hub_failure_on_live_message_leaves_cursor_for_the_catch_up(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.inbound.error = RuntimeError("hub down")
    with pytest.raises(RuntimeError):
        rig.bridge.handle_message(msg(RECENT + 1, "x"))
    assert rig.state.get(f"cursor.{DM}") is None


def test_unpaired_sender_gets_nothing_back(rig: Rig) -> None:
    rig.inbound.result = {"status": "ignored"}
    rig.bridge.synced = True
    assert not rig.bridge.handle_message(msg(RECENT + 1, "hi"))
    assert rig.discord.reactions == [] and rig.discord.sent == [] and rig.discord.typed == []
    assert rig.state.get(f"cursor.{DM}") == str(RECENT + 1)  # handled, not retried


def test_attachments_are_forwarded_and_oversized_dropped(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.discord.downloads["https://cdn.discordapp.com/a.png"] = b"PNG"
    attachments = [
        {"id": "1", "filename": "a.png", "url": "https://cdn.discordapp.com/a.png", "size": 3},
        {"id": "2", "filename": "huge.bin", "url": "https://cdn.discordapp.com/h", "size": 10**9},
    ]
    rig.bridge.handle_message(msg(RECENT + 1, "", attachments=attachments))
    call = rig.inbound.calls[0]
    assert call["files"] == [{"name": "a.png", "base64": base64.b64encode(b"PNG").decode()}]


def test_reply_to_a_bot_message_carries_the_outbox_id(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.state.set("reply_ref.777", "outbox-1")
    rig.bridge.handle_message(msg(RECENT + 1, "yes", reference="777"))
    assert rig.inbound.calls[0]["reply_ref"] == "outbox-1"


# -- reactions and typing ----------------------------------------------------------------------
def test_eyes_and_typing_on_delivery_cleared_on_reply(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.bridge.handle_message(msg(RECENT + 1, "hi"))
    assert rig.discord.reactions == [(DM, str(RECENT + 1), "👀")]
    assert rig.discord.typed == [DM]
    assert rig.bridge.typing_tick() == 1
    assert rig.discord.typed == [DM, DM]
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="done"))
    assert rig.discord.removed == [(DM, str(RECENT + 1), "👀")]
    assert rig.bridge.typing_tick() == 0  # stopped


def test_typing_gives_up_after_the_pending_window(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.bridge.handle_message(msg(RECENT + 1, "hi"))
    rig.clock += 31 * 60 * 1000
    assert rig.bridge.typing_tick() == 0


def test_failed_reaction_removal_is_retried_not_forgotten(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.bridge.handle_message(msg(RECENT + 1, "one"))
    rig.discord.fail_remove = DiscordError("Discord is unreachable")
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="r1"))
    assert rig.discord.removed == []
    rig.discord.fail_remove = None
    rig.bridge.handle_message(msg(RECENT + 2, "two"))
    rig.bridge.send(OutboxMessage(id="o2", sender=USER, text="r2"))
    assert {m for _, m, _ in rig.discord.removed} == {str(RECENT + 1), str(RECENT + 2)}


def test_reaction_failure_never_breaks_forwarding(rig: Rig) -> None:
    def boom(*_: object) -> None:
        raise DiscordError("403", permanent=True)

    rig.discord.add_reaction = boom  # type: ignore[method-assign]
    rig.discord.typing = boom  # type: ignore[method-assign]
    rig.bridge.synced = True
    assert rig.bridge.handle_message(msg(RECENT + 1, "hi"))


# -- outbound ----------------------------------------------------------------------------------
def test_split_text_respects_the_limit() -> None:
    assert split_text("short") == ["short"]
    text = ("word " * 900).strip()
    parts = split_text(text)
    assert all(len(p) <= 2000 for p in parts) and len(parts) == 3
    assert " ".join(parts).split() == text.split()
    blob = "x" * 4500
    assert [len(p) for p in split_text(blob)] == [2000, 2000, 500]
    assert "".join(split_text(blob)) == blob


def test_outbox_is_posted_to_the_dm_and_acked_with_reply_maps(rig: Rig) -> None:
    rig.outbox.result = {
        "messages": [
            {
                "id": "o1",
                "sender": USER,
                "text": "a" * 2500,
                "files": [{"name": "r.txt", "mime": "text/plain", "base64": base64.b64encode(b"x").decode()}],
            }
        ]
    }
    assert rig.bridge.drain_outbox(30) == 1
    first, second = rig.discord.sent
    assert first["channel"] == DM and len(first["text"]) == 2000 and first["files"] == []
    assert second["files"] == [("r.txt", b"x")]  # files go with the last part
    assert rig.ack.calls == [{"id": "o1"}]
    assert rig.discord.opened == [USER]
    assert rig.state.get("reply_ref.7001") == "o1" and rig.state.get("sent.o1") == "7001"


def test_a_retry_after_a_partial_send_does_not_repeat_parts(rig: Rig) -> None:
    rig.discord.fail_send_at = 1
    message = OutboxMessage(id="o1", sender=USER, text="a" * 2500)
    rig.outbox.result = {"messages": [message.model_dump()]}
    assert rig.bridge.drain_outbox(30) == 0  # second part failed: not acked
    assert rig.ack.calls == []
    assert rig.bridge.drain_outbox(30) == 1
    assert [len(s["text"]) for s in rig.discord.sent] == [2000, 500]


def test_reply_ref_sets_message_reference(rig: Rig) -> None:
    rig.state.set("sent.earlier", "4242")
    rig.bridge.send(OutboxMessage(id="o2", sender=USER, text="re", reply_ref="earlier"))
    assert rig.discord.sent[0]["reply_to"] == "4242"


def test_question_choices_are_appended(rig: Rig) -> None:
    rig.bridge.send(OutboxMessage(id="o3", sender=USER, text="Which?", question={"options": ["red", "blue"]}))
    assert rig.discord.sent[0]["text"] == "Which?\n\n- red\n- blue"


def test_transient_failure_leaves_the_message_unacked(rig: Rig) -> None:
    rig.discord.send_error = DiscordError("Discord is unreachable")
    rig.outbox.result = {"messages": [{"id": "o1", "sender": USER, "text": "x"}]}
    assert rig.bridge.drain_outbox(30) == 0
    assert rig.ack.calls == []


def test_permanent_failure_is_acked_so_it_cannot_block_the_queue(rig: Rig) -> None:
    rig.discord.send_error = DiscordError("403 cannot DM this user", permanent=True)
    rig.outbox.result = {"messages": [{"id": "o1", "sender": USER, "text": "x"}]}
    rig.bridge.drain_outbox(30)
    assert rig.ack.calls == [{"id": "o1"}]
    assert any(k.get("permanent") for _, k in rig.logs)


def test_send_dm_validates_and_posts(rig: Rig) -> None:
    assert rig.bridge.send_dm(USER, "ping") == "7001"
    assert rig.discord.sent[0]["text"] == "ping"
    with pytest.raises(ValueError, match="digits"):
        rig.bridge.send_dm("someone", "x")
    with pytest.raises(ValueError, match="empty"):
        rig.bridge.send_dm(USER, "  ")


# -- replies, both directions --------------------------------------------------------------------
def test_the_bots_answer_is_a_discord_reply_to_the_message_it_answers(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.bridge.handle_message(msg(RECENT + 1, "first"))
    rig.bridge.handle_message(msg(RECENT + 2, "second"))
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="a" * 2500))
    first, second = rig.discord.sent
    assert first["reply_to"] == str(RECENT + 2)  # the newest message; only the first part quotes it
    assert second["reply_to"] is None
    # The reply is used up: a later, unprompted message is a plain DM.
    rig.bridge.send(OutboxMessage(id="o2", sender=USER, text="by the way"))
    assert rig.discord.sent[-1]["reply_to"] is None


def test_an_explicit_reply_ref_wins_and_proactive_dms_reply_to_nothing(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.bridge.handle_message(msg(RECENT + 1, "hi"))
    rig.state.set("sent.earlier", "4242")
    rig.bridge.send(OutboxMessage(id="o1", sender=USER, text="re", reply_ref="earlier"))
    assert rig.discord.sent[0]["reply_to"] == "4242"
    rig.bridge.handle_message(msg(RECENT + 2, "again"))
    rig.bridge.send_dm(USER, "ping")
    assert rig.discord.sent[1]["reply_to"] is None


def test_a_user_reply_carries_a_marked_quote_of_the_original(rig: Rig) -> None:
    rig.bridge.synced = True
    original = msg(RECENT + 1, "Shall I book it?\nFriday works.", author=BOT, username="botler")
    rig.bridge.handle_message(msg(RECENT + 2, "yes please", reference=original.id, referenced=original))
    text = rig.inbound.calls[0]["text"]
    assert text == "[replying to Botler's message]\n> Shall I book it?\n> Friday works.\n\nyes please"
    assert rig.discord.fetched == []  # Discord sent it along: no extra request


def test_the_original_is_fetched_when_only_its_id_is_present(rig: Rig) -> None:
    rig.bridge.synced = True
    rig.discord.history[DM] = [msg(RECENT + 1, "my note", author=USER)]
    rig.bridge.handle_message(msg(RECENT + 2, "see above", reference=str(RECENT + 1)))
    assert rig.discord.fetched == [str(RECENT + 1)]
    assert rig.inbound.calls[0]["text"] == "[replying to your own earlier message]\n> my note\n\nsee above"


def test_an_unfetchable_original_means_no_quote_not_a_lost_message(rig: Rig) -> None:
    rig.bridge.synced = True
    assert rig.bridge.handle_message(msg(RECENT + 2, "hello?", reference="999"))
    assert rig.inbound.calls[0]["text"] == "hello?"


def test_a_long_quote_is_cut(rig: Rig) -> None:
    rig.bridge.synced = True
    original = msg(RECENT + 1, "w" * 5000, author=BOT)
    rig.bridge.handle_message(msg(RECENT + 2, "ok", reference=original.id, referenced=original))
    assert len(rig.inbound.calls[0]["text"]) < 700
