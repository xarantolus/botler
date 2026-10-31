import json
from pathlib import Path
from typing import Any

from example_channel.__main__ import Spool, deliver, send


def test_inbound_messages_reach_the_hub_once(tmp_path: Path) -> None:
    spool, got = Spool(tmp_path), list[dict[str, Any]]()
    (spool.inbox / "1.json").write_text(json.dumps({"sender": 42, "text": "pair 123456"}))
    (spool.inbox / "2.json").write_text(json.dumps({"sender": 42, "text": "yes", "reply_to": "o1"}))

    def inbound(**kw: Any) -> dict[str, Any]:  # noqa: ANN401
        got.append(kw)
        return {"status": "delivered"}

    assert deliver(spool, inbound) == 2
    assert [g["sender"] for g in got] == ["42", "42"]
    assert got[1]["reply_ref"] == "o1"
    assert deliver(spool, inbound) == 0, "each message is handed over once"


def test_outbox_messages_are_sent_then_acknowledged(tmp_path: Path) -> None:
    spool, acked = Spool(tmp_path), list[str]()
    pending: list[dict[str, Any]] = [
        {"id": "o1", "sender": "42", "text": "Paired.", "files": [], "question": False}
    ]

    def outbox(**_: Any) -> dict[str, Any]:  # noqa: ANN401
        return {"messages": pending}

    def ack(**kw: Any) -> dict[str, Any]:  # noqa: ANN401
        acked.append(kw["id"])
        return {"acked": True}

    assert send(spool, outbox, ack, wait_seconds=0) == 1
    assert json.loads((spool.outbox / "o1.json").read_text())["text"] == "Paired."
    assert acked == ["o1"]
    # A repeat after a crash before the ack writes the same file again, not a second message.
    send(spool, outbox, ack, wait_seconds=0)
    assert len(list(spool.outbox.iterdir())) == 1
