"""Run the Discord bridge: gateway events in, an outbox drain out, and the send_dm MCP tool.

The hub calls this a channel (`channel = true`). Messages the paired user DMs to the bot, and their
`/remind` slash commands, are handed to `channel_inbound`; `channel_outbox` is drained back into the
DM. Inbound is the Discord gateway websocket (discord.py), never
polling; on every start and reconnect `Bridge.catch_up` reads the DM history from the stored
cursors, so messages sent while the bot was down are not lost.

Threads: the gateway (own event loop), one worker for gateway events (keeps them in order), one for
commands, a sync loop (catch-up with backoff), the outbox drain, the typing refresh, MCP. Nothing
here exits because Discord or the hub is unreachable; it retries and reports health.
"""

from __future__ import annotations

import os
import sys
import threading
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from typing import Any

import botler

from .bridge import TYPING_INTERVAL_SECONDS, Bridge
from .client import Callbacks, DiscordClient, start_thread
from .mcp import McpServer, build_tools
from .models import Interaction, Message
from .settings import Settings

MCP_PORT = int(os.environ.get("DISCORD_BOT_PORT", "8071"))
# Consecutive health checks (15 s apart by default) without a working connection before the
# service reports itself unhealthy. It keeps retrying regardless.
FAIL_AFTER = 8
MAX_BACKOFF_SECONDS = 60.0

Log = Callable[..., None]


def report_health(health: Any, *, ok: bool, log: Log, reason: str = "") -> None:  # noqa: ANN401
    """Report health to the hub. A hub restart makes the call raise `BotlerError`; that must not end
    the service, which keeps serving Discord and re-attaches when the hub is back."""
    try:
        if ok:
            health.ok()
        else:
            health.fail(reason)
    except botler.BotlerError as e:
        log("could not report health to the hub", error=str(e))


def outbox_loop(bridge: Bridge, stop: threading.Event) -> None:
    """Send hub messages to Discord forever; a failure is retried, never fatal."""
    while not stop.is_set():
        try:
            bridge.drain_outbox(wait_seconds=30)
        except Exception as e:
            botler.log("outbox loop error", error=f"{type(e).__name__}: {e}")
            stop.wait(5)


def typing_loop(bridge: Bridge, stop: threading.Event) -> None:
    """Typing lasts about 10 s in Discord: refresh it every 8 s while a message is being handled."""
    while not stop.is_set():
        try:
            bridge.typing_tick()
        except Exception as e:
            botler.log("typing loop error", error=f"{type(e).__name__}: {e}")
        stop.wait(TYPING_INTERVAL_SECONDS)


def sync_loop(bridge: Bridge, wanted: threading.Event, stop: threading.Event) -> None:
    """Catch up whenever the gateway (re)connects or a live message could not be handled; retry with
    backoff until it succeeds. Until then the live path stands down, so nothing is skipped."""
    backoff = 1.0
    while not stop.is_set():
        if not wanted.wait(timeout=1):
            continue
        wanted.clear()
        try:
            delivered = bridge.catch_up()
            botler.log("caught up", delivered=delivered)
            backoff = 1.0
        except Exception as e:
            botler.log("catch-up failed; will retry", error=f"{type(e).__name__}: {e}")
            stop.wait(backoff)
            backoff = min(backoff * 2, MAX_BACKOFF_SECONDS)
            wanted.set()


def main() -> int:
    token = os.environ.get("DISCORD_BOT_TOKEN")
    if not token:
        botler.health.fail("DISCORD_BOT_TOKEN is not set; enter it under Secrets")
        return 1

    settings = Settings.load()
    stop = threading.Event()
    wanted = threading.Event()
    events = ThreadPoolExecutor(max_workers=1, thread_name_prefix="events")
    commands = ThreadPoolExecutor(max_workers=2, thread_name_prefix="commands")
    fatal: list[str] = []
    bridge_box: list[Bridge] = []

    def on_message(message: Message) -> None:
        def work() -> None:
            try:
                bridge_box[0].handle_message(message)
            except Exception as e:
                # Not handled (hub or Discord trouble): the catch-up reads it again from history.
                botler.log("live message failed; catching up", error=f"{type(e).__name__}: {e}")
                bridge_box[0].connection_lost()
                wanted.set()

        events.submit(work)

    def on_command(interaction: Interaction) -> None:
        def work() -> None:
            try:
                bridge_box[0].handle_command(interaction)
            except Exception as e:
                botler.log("command failed", error=f"{type(e).__name__}: {e}")

        commands.submit(work)

    def on_message_command(interaction: Interaction) -> None:
        def work() -> None:
            try:
                bridge_box[0].handle_message_command(interaction)
            except Exception as e:
                botler.log("forwarded message command failed", error=f"{type(e).__name__}: {e}")

        commands.submit(work)

    def on_disconnect() -> None:
        if bridge_box:
            bridge_box[0].connection_lost()

    def on_fatal(reason: str) -> None:
        fatal.append(reason)
        stop.set()

    discord = DiscordClient(
        token,
        Callbacks(
            on_ready=wanted.set,
            on_disconnect=on_disconnect,
            on_message=on_message,
            on_command=on_command,
            on_message_command=on_message_command,
            on_fatal=on_fatal,
        ),
        botler.log,
    )
    bridge = Bridge(
        discord,
        settings,
        state=botler.state,
        inbound=botler.ops.channel_inbound,
        outbox=botler.ops.channel_outbox,
        ack=botler.ops.channel_ack,
        log=botler.log,
    )
    bridge_box.append(bridge)

    server = McpServer(build_tools(bridge.send_dm), port=MCP_PORT)
    threading.Thread(target=server.serve_forever, name="mcp", daemon=True).start()
    start_thread(discord)
    for target, args, name in (
        (sync_loop, (bridge, wanted, stop), "sync"),
        (outbox_loop, (bridge, stop), "outbox"),
        (typing_loop, (bridge, stop), "typing"),
    ):
        threading.Thread(target=target, args=args, name=name, daemon=True).start()

    failures = 0
    while not stop.is_set():
        if bridge.synced:
            failures = 0
            report_health(botler.health, ok=True, log=botler.log)
        else:
            failures += 1
            if failures == FAIL_AFTER:
                report_health(
                    botler.health,
                    ok=False,
                    log=botler.log,
                    reason="not connected to Discord (gateway or catch-up failing); retrying",
                )
        stop.wait(settings.health_seconds)

    discord.stop()
    server.shutdown()
    report_health(botler.health, ok=False, log=botler.log, reason=fatal[0] if fatal else "stopped")
    return 1


if __name__ == "__main__":
    sys.exit(main())
