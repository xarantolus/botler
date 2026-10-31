"""Example channel: the shape of every channel service, with a spool directory as the protocol.

Two loops, each blocking on its own side:
- inbound: wait for what the outside sends (here, `*.json` files in `spool/in`), hand each to the
  hub with `channel_inbound`. The hub decides who it is (paired or not) and where it goes; this
  service never answers strangers itself.
- outbound: long-poll `channel_outbox`, send each message (here, a file in `spool/out`), then
  `channel_ack` it. A crash before the ack sends it again, so sending must tolerate a repeat.

A real channel replaces `Spool` with the protocol (Telegram `getUpdates` / `sendMessage`, IMAP and
SMTP, ...) and keeps the rest.
"""

import base64
import json
import threading
import time
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any, Protocol

import botler

SPOOL = Path("/data/spool")
Hub = Callable[..., dict[str, Any]]


class Outside(Protocol):
    def receive(self) -> Iterable[dict[str, Any]]: ...
    def send(self, message: dict[str, Any]) -> None: ...


class Spool:
    """The stand-in protocol: one JSON file per message in each direction."""

    def __init__(self, root: Path) -> None:
        self.inbox, self.outbox = root / "in", root / "out"
        self.inbox.mkdir(parents=True, exist_ok=True)
        self.outbox.mkdir(parents=True, exist_ok=True)

    def receive(self) -> Iterable[dict[str, Any]]:
        for f in sorted(self.inbox.glob("*.json")):
            message = json.loads(f.read_text())
            yield message
            f.unlink()

    def send(self, message: dict[str, Any]) -> None:
        # Named by the outbox id: sending the same message twice writes the same file.
        (self.outbox / f"{message['id']}.json").write_text(json.dumps(message))


def deliver(outside: Outside, inbound: Hub) -> int:
    """Hand everything that arrived to the hub; returns how many."""
    n = 0
    for m in outside.receive():
        files = [{"name": f["name"], "base64": f["base64"]} for f in m.get("files", [])]
        inbound(sender=str(m["sender"]), text=m.get("text", ""), files=files, reply_ref=m.get("reply_to"))
        n += 1
    return n


def send(outside: Outside, outbox: Hub, ack: Hub, wait_seconds: int) -> int:
    """Send what the hub has for the outside, acknowledging each once sent; returns how many."""
    messages = outbox(wait_seconds=wait_seconds)["messages"]
    for m in messages:
        outside.send(m)
        ack(id=m["id"])
    return len(messages)


def decode(file: dict[str, str]) -> bytes:
    return base64.b64decode(file["base64"])


def main() -> None:
    outside = Spool(SPOOL)

    def outbound() -> None:
        while True:
            send(outside, botler.ops.channel_outbox, botler.ops.channel_ack, wait_seconds=30)

    threading.Thread(target=outbound, daemon=True).start()
    while True:
        deliver(outside, botler.ops.channel_inbound)
        botler.health.ok()
        time.sleep(1)


if __name__ == "__main__":
    main()
