"""The Discord side: discord.py's gateway client and REST, behind a synchronous facade.

discord.py is asyncio; the bridge is plain synchronous code (so tests need no event loop and the
hub's blocking calls never stall the gateway heartbeat). `DiscordClient` runs the discord.py client
on its own event-loop thread and exposes blocking methods (`DiscordApi` in bridge.py) that hop onto
that loop. Gateway events go the other way: each is handed to `Callbacks`, which `__main__` runs on
a single worker thread, so events are handled in the order they arrived.

Intents are `Intents.none()` plus `direct_messages`. DM text needs no privileged intent: Discord's
gateway docs list "content in DMs with the app" as an exception to the Message Content intent, and
DIRECT_MESSAGES is what delivers MESSAGE_CREATE in DMs. Guild messages are never subscribed to.
Slash commands arrive as INTERACTION_CREATE on the same websocket, which needs no intent.

If the gateway or login fails at start (Discord unreachable), `run` retries with backoff instead of
exiting; a rejected token is the one fatal case (`on_fatal`).
"""

from __future__ import annotations

import asyncio
import io
import threading
import urllib.parse
import urllib.request
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any, TypeVar

import aiohttp
import discord
from discord import app_commands
from discord.http import Route, handle_message_parameters

from .errors import DiscordError
from .models import DM_CHANNEL_TYPE, Channel, Interaction, Message, PendingInteraction

T = TypeVar("T")
CALL_TIMEOUT_SECONDS = 60.0
DOWNLOAD_TIMEOUT_SECONDS = 30.0
MAX_BACKOFF_SECONDS = 60.0
# Attachments are only ever fetched from Discord's own CDN.
CDN_HOSTS = frozenset({"cdn.discordapp.com", "media.discordapp.net"})
# Discord answers these for a request that will never succeed as sent.
PERMANENT_STATUSES = frozenset({400, 403, 404})
Log = Callable[..., None]


@dataclass
class Callbacks:
    """What the gateway tells the bridge. Each is called from the event loop and must not block."""

    on_ready: Callable[[], None]  # connected (fresh or resumed): catch up
    on_disconnect: Callable[[], None]
    on_message: Callable[[Message], None]
    on_command: Callable[[Interaction], None]
    # A "Send to Botler" message command, once its comment modal was submitted.
    on_message_command: Callable[[Interaction], None]
    on_fatal: Callable[[str], None]


def message_model(message: discord.Message) -> Message:
    """A gateway message as the bridge's `Message` model."""
    return Message.model_validate(
        {
            "id": str(message.id),
            "channel_id": str(message.channel.id),
            "content": message.content,
            "type": message.type.value,
            "author": {
                "id": str(message.author.id),
                "username": getattr(message.author, "name", "") or "",
                "bot": message.author.bot,
            },
            "attachments": [
                {
                    "id": str(a.id),
                    "filename": a.filename,
                    "url": a.url,
                    "size": a.size,
                    "content_type": a.content_type or "",
                }
                for a in message.attachments
            ],
            "message_reference": (
                {"message_id": str(message.reference.message_id)}
                if message.reference and message.reference.message_id
                else None
            ),
            "guild_id": str(message.guild.id) if message.guild else None,
            "referenced_message": _referenced(message),
        }
    )


def _referenced(message: discord.Message) -> dict[str, Any] | None:
    """The replied-to message if discord.py has it resolved (it is only a cache lookup)."""
    reference = message.reference
    original = reference.resolved if reference else None
    if not isinstance(original, discord.Message):
        return None
    return {
        "id": str(original.id),
        "channel_id": str(original.channel.id),
        "content": original.content,
        "author": {
            "id": str(original.author.id),
            "username": original.author.name,
            "bot": original.author.bot,
        },
        "attachments": [{"id": str(a.id), "filename": a.filename} for a in original.attachments],
    }


def interaction_model(interaction: discord.Interaction) -> Interaction:
    """A gateway interaction as the bridge's `Interaction` model."""
    context = 0 if interaction.context.guild else 1 if interaction.context.dm_channel else 2
    return Interaction.model_validate(
        {
            "id": str(interaction.id),
            "token": interaction.token,
            "application_id": str(interaction.application_id),
            "context": context,
            "guild_id": str(interaction.guild_id) if interaction.guild_id else None,
            "user": {"id": str(interaction.user.id)},
            "data": interaction.data or {},
        }
    )


def message_command_model(
    invocation: discord.Interaction, target: discord.Message, comment: str
) -> Interaction:
    """The bridge's `Interaction` for a "Send to Botler" command: the invoking interaction's user,
    context and guild, plus the target message Discord resolved for us. `comment` is what the user
    typed into the modal.

    The model is built from the *original* command invocation (which carries the resolved target);
    the caller overwrites id/token/application_id with the modal-submit interaction, whose follow-up
    webhook the hub's reply uses."""
    model = interaction_model(invocation)
    target_model = message_model(target)
    model.comment = comment
    model.raw_data = dict(invocation.data or {})
    model.data.target_id = target_model.id
    model.data.resolved.messages = {target_model.id: target_model}
    model.channel_name = str(getattr(target.channel, "name", "") or "")
    model.guild_name = str(getattr(invocation.guild, "name", "") or "")
    return model


class CommentModal(discord.ui.Modal, title="Send to Botler"):
    """A MESSAGE command cannot take text, so this modal collects an optional comment. The command
    callback sends it at once (the 3 s acknowledgement); `on_submit` is the second interaction."""

    comment: discord.ui.TextInput[Any] = discord.ui.TextInput(
        label="Add a comment (optional)",
        required=False,
        style=discord.TextStyle.paragraph,
        max_length=1000,
    )

    def __init__(
        self, callbacks: Callbacks, invocation: discord.Interaction, target: discord.Message
    ) -> None:
        super().__init__()
        self._callbacks = callbacks
        self._invocation = invocation
        self._target = target

    async def on_submit(self, interaction: discord.Interaction) -> None:
        # Answer the modal submit ourselves, at once and unconditionally: the forward goes to the
        # hub and its reply goes to the DM, never through this interaction. The interaction is
        # never left open for an async reply, so "nothing happened" cannot happen again.
        model = message_command_model(self._invocation, self._target, str(self.comment.value or ""))
        model.id = str(interaction.id)
        model.token = interaction.token
        model.application_id = str(interaction.application_id)
        await interaction.response.send_message(
            "Sent to Botler \u2014 I'll answer in our DM.", ephemeral=True
        )
        self._callbacks.on_message_command(model)


def _error(e: BaseException) -> DiscordError:
    if isinstance(e, DiscordError):
        return e
    if isinstance(e, discord.HTTPException):
        return DiscordError(
            f"Discord answered {e.status} (code {e.code}): {e.text}", permanent=e.status in PERMANENT_STATUSES
        )
    if isinstance(e, (TimeoutError, asyncio.TimeoutError)):
        return DiscordError("Discord did not answer in time")
    return DiscordError(f"Discord is unreachable ({type(e).__name__}: {e})")


class Gateway(discord.Client):
    """discord.py's client, wired to `Callbacks`."""

    def __init__(self, callbacks: Callbacks, log: Log) -> None:
        intents = discord.Intents.none()
        intents.dm_messages = True  # Gateway intent DIRECT_MESSAGES
        super().__init__(intents=intents, max_messages=None, chunk_guilds_at_startup=False)
        self.callbacks = callbacks
        self.log = log
        self.tree = app_commands.CommandTree(self)
        self._synced_commands = False
        self._add_commands()

    def _add_commands(self) -> None:
        @self.tree.command(name="remind", description="Ask Botler to remind you of something.")
        @app_commands.describe(
            when="When, in your own words: in 2 hours, tomorrow 9am, mon 8:00",
            what="What to remind you of",
        )
        @app_commands.allowed_installs(guilds=True, users=True)
        @app_commands.allowed_contexts(guilds=True, dms=True, private_channels=True)
        async def remind(interaction: discord.Interaction, when: str, what: str) -> None:  # pyright: ignore[reportUnusedFunction]
            model = interaction_model(interaction)
            # Answer the interaction ourselves, at once and unconditionally: the hub's reply lands in
            # the DM, never here. Do not leave it open for an async reply.
            await interaction.response.send_message(
                "Reminder received \u2014 I'll answer in our DM.", ephemeral=model.ephemeral
            )
            self.callbacks.on_command(model)

        @self.tree.context_menu(name="Send to Botler")
        @app_commands.allowed_installs(guilds=True, users=True)
        @app_commands.allowed_contexts(guilds=True, dms=True, private_channels=True)
        async def send_to_botler(  # pyright: ignore[reportUnusedFunction]
            interaction: discord.Interaction, message: discord.Message
        ) -> None:
            # The modal is the acknowledgement; Discord hands us the resolved target message, so no
            # REST access to its channel is needed (works in servers the bot is not in).
            await interaction.response.send_modal(CommentModal(self.callbacks, interaction, message))

    async def on_ready(self) -> None:
        self.log("discord gateway ready")
        if not self._synced_commands:
            try:
                await self.tree.sync()
                self._synced_commands = True
            except Exception as e:  # commands are optional; DMs keep working
                self.log("could not register slash commands", error=f"{type(e).__name__}: {e}")
        self.callbacks.on_ready()

    async def on_resumed(self) -> None:
        self.log("discord gateway resumed")
        self.callbacks.on_ready()

    async def on_disconnect(self) -> None:
        self.callbacks.on_disconnect()

    async def on_message(self, message: discord.Message) -> None:
        if message.guild is not None:
            return
        self.callbacks.on_message(message_model(message))


class DiscordClient:
    """The synchronous `DiscordApi`, plus `run` to keep the gateway connected."""

    def __init__(self, token: str, callbacks: Callbacks, log: Log) -> None:
        self._token = token
        self._callbacks = callbacks
        self._log = log
        self._loop = asyncio.new_event_loop()
        self._client: Gateway | None = None
        self._stopping = False

    # -- lifecycle -------------------------------------------------------------------------
    def run(self) -> None:
        """Run the event loop until `stop`. Blocks; call it in a thread."""
        asyncio.set_event_loop(self._loop)
        self._loop.run_until_complete(self._main())

    def stop(self) -> None:
        self._stopping = True
        client = self._client
        if client is not None and not self._loop.is_closed():
            asyncio.run_coroutine_threadsafe(client.close(), self._loop)

    async def _main(self) -> None:
        backoff = 1.0
        while not self._stopping:
            client = Gateway(self._callbacks, self._log)
            self._client = client
            try:
                await client.start(self._token, reconnect=True)
            except discord.LoginFailure:
                self._callbacks.on_fatal(
                    "Discord rejected DISCORD_BOT_TOKEN; enter a valid one under Secrets"
                )
                return
            except Exception as e:
                self._log("discord connection failed", error=f"{type(e).__name__}: {e}")
            finally:
                self._callbacks.on_disconnect()
                await client.close()
            if self._stopping:
                return
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, MAX_BACKOFF_SECONDS)
            if client.is_ready():
                backoff = 1.0

    # -- blocking calls from worker threads --------------------------------------------------
    def _http(self) -> Any:  # noqa: ANN401
        client = self._client
        if client is None or not client.is_ready():
            raise DiscordError("not connected to Discord yet")
        return client.http

    def _call(self, make: Callable[[Any], Awaitable[T]]) -> T:
        http = self._http()
        future = asyncio.run_coroutine_threadsafe(_await(make(http)), self._loop)
        try:
            return future.result(CALL_TIMEOUT_SECONDS)
        except Exception as e:
            future.cancel()
            raise _error(e) from e

    @property
    def bot_id(self) -> str:
        client = self._client
        if client is None or client.user is None:
            raise DiscordError("not connected to Discord yet")
        return str(client.user.id)

    def dm_channels(self) -> list[Channel]:
        raw: list[dict[str, Any]] = self._call(lambda http: http.request(Route("GET", "/users/@me/channels")))
        return [c for c in (Channel.model_validate(item) for item in raw) if c.type == DM_CHANNEL_TYPE]

    def messages_after(self, channel_id: str, after: str, limit: int) -> list[Message]:
        raw: list[dict[str, Any]] = self._call(lambda http: http.logs_from(channel_id, limit, after=after))
        return [Message.model_validate(item) for item in raw]

    def get_message(self, channel_id: str, message_id: str) -> Message:
        return Message.model_validate(self._call(lambda http: http.get_message(channel_id, message_id)))

    def open_dm(self, user_id: str) -> Channel:
        return Channel.model_validate(self._call(lambda http: http.start_private_message(user_id)))

    def send(
        self, channel_id: str, text: str, files: list[tuple[str, bytes]], reply_to: str | None
    ) -> Message:
        def make(http: Any) -> Awaitable[Any]:  # noqa: ANN401
            params = handle_message_parameters(
                content=text or None,
                files=[discord.File(io.BytesIO(data), filename=name) for name, data in files],
                allowed_mentions=discord.AllowedMentions.none(),
                message_reference=(
                    {"message_id": reply_to, "channel_id": channel_id, "fail_if_not_exists": False}  # type: ignore[arg-type]
                    if reply_to
                    else discord.utils.MISSING
                ),
            )
            return http.send_message(channel_id, params=params)

        return Message.model_validate(self._call(make))

    def add_reaction(self, channel_id: str, message_id: str, emoji: str) -> None:
        self._call(lambda http: http.add_reaction(channel_id, message_id, emoji))

    def remove_reaction(self, channel_id: str, message_id: str, emoji: str) -> None:
        self._call(lambda http: http.remove_own_reaction(channel_id, message_id, emoji))

    def typing(self, channel_id: str) -> None:
        self._call(lambda http: http.send_typing(channel_id))

    def followup(self, interaction: PendingInteraction, text: str, files: list[tuple[str, bytes]]) -> str:
        """Answer a deferred interaction through its webhook (valid for 15 minutes)."""
        client = self._client
        if client is None:
            raise DiscordError("not connected to Discord yet")
        self._http()

        async def go() -> str:
            webhook = discord.Webhook.partial(
                int(interaction.application_id), interaction.token, client=client
            )
            sent = await webhook.send(
                content=text or discord.utils.MISSING,
                files=[discord.File(io.BytesIO(data), filename=name) for name, data in files],
                ephemeral=interaction.ephemeral,
                allowed_mentions=discord.AllowedMentions.none(),
                wait=True,
            )
            return str(sent.id)

        future = asyncio.run_coroutine_threadsafe(go(), self._loop)
        try:
            return future.result(CALL_TIMEOUT_SECONDS)
        except Exception as e:
            future.cancel()
            raise _error(e) from e

    def download(self, url: str) -> bytes:
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "https" or parsed.hostname not in CDN_HOSTS:
            raise DiscordError(
                f"refusing to download from {parsed.hostname}: not Discord's CDN", permanent=True
            )
        try:
            with urllib.request.urlopen(url, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:  # noqa: S310
                return response.read()
        except (OSError, aiohttp.ClientError) as e:
            raise _error(e) from e


async def _await(awaitable: Awaitable[T]) -> T:
    return await awaitable


def start_thread(client: DiscordClient) -> threading.Thread:
    thread = threading.Thread(target=client.run, name="discord-gateway", daemon=True)
    thread.start()
    return thread
