"""The bridge logic: Discord DMs in, hub messages out, replies back into the DM.

Pure and synchronous: it talks to Discord through the `DiscordApi` protocol and to the hub through
plain callables, so tests drive it with fakes (tests/). Cursors, routes and reply maps live in
`botler.state`, so a restart neither repeats nor loses messages.

Inbound messages come from two places that must agree: the gateway (`handle_message`, live) and
the REST history (`catch_up`, on every start and reconnect). Both go through one lock and one
per-channel cursor (the id of the last message looked at), so a message is handled once whichever
path sees it first. Until a catch-up has succeeded on the current connection the live path stands
down (`synced` is False) and leaves the message for the catch-up, so a live message can never move
the cursor past one that was missed.
"""

from __future__ import annotations

import base64
import json
import re
import threading
import time
from collections.abc import Callable
from typing import Protocol, cast

from .errors import DiscordError
from .models import (
    BOT_DM_CONTEXT,
    GUILD_CONTEXT,
    USER_MESSAGE_TYPES,
    Channel,
    InboundResult,
    Interaction,
    Message,
    OutboxBatch,
    OutboxMessage,
    Pending,
    PendingInteraction,
    PendingReaction,
    Question,
)
from .settings import Settings

CURSOR = "cursor."  # cursor.<channel id> = id of the last message looked at
DMS = "dms"  # JSON list of every DM channel id seen, in case Discord cannot list them
DM_OF = "dm."  # dm.<user id> = that user's DM channel id
PENDING = "pending."  # pending.<sender> = JSON Pending: reactions still to clear
REPLY = "reply_ref."  # reply_ref.<discord message id> = the outbox id it carried
SENT = "sent."  # sent.<outbox id> = the discord message id of its first part
PROGRESS = "progress."  # progress.<outbox id> = how many text parts are already posted

MESSAGE_LIMIT = 2000
PAGE = 50
TYPING_INTERVAL_SECONDS = 8.0
# Typing and "handling" are given up on after this long without a reply.
PENDING_WINDOW_MS = 30 * 60 * 1000
MAX_QUOTE_CHARS = 600
MAX_INBOUND_FILE_BYTES = 25 * 1024 * 1024
# The whole interaction `data` goes along; above this many bytes it is sent as a file instead of text.
MAX_INLINE_JSON_CHARS = 1500
FORWARD_JSON_NAME = "forwarded-message.json"
DISCORD_EPOCH_MS = 1420070400000
# Custom emoji markup in a message: <:name:id> or <a:name:id> (animated).
CUSTOM_EMOJI = re.compile(r"<(a?):([A-Za-z0-9_]+):(\d+)>")


class State(Protocol):
    def get(self, key: str) -> str | None: ...
    def set(self, key: str, value: str) -> None: ...


class DiscordApi(Protocol):
    @property
    def bot_id(self) -> str: ...
    def dm_channels(self) -> list[Channel]: ...
    def messages_after(self, channel_id: str, after: str, limit: int) -> list[Message]: ...
    def open_dm(self, user_id: str) -> Channel: ...
    def send(
        self, channel_id: str, text: str, files: list[tuple[str, bytes]], reply_to: str | None
    ) -> Message: ...
    def add_reaction(self, channel_id: str, message_id: str, emoji: str) -> None: ...
    def remove_reaction(self, channel_id: str, message_id: str, emoji: str) -> None: ...
    def typing(self, channel_id: str) -> None: ...
    def get_message(self, channel_id: str, message_id: str) -> Message: ...
    def followup(self, interaction: PendingInteraction, text: str, files: list[tuple[str, bytes]]) -> str: ...
    def download(self, url: str) -> bytes: ...


Hub = Callable[..., object]
Log = Callable[..., None]


def snowflake_at(ms: int) -> str:
    """The smallest snowflake created at `ms` (Discord accepts any snowflake as `after`)."""
    return str(max(ms - DISCORD_EPOCH_MS, 0) << 22)


def snowflake_ms(snowflake: str) -> int:
    return (int(snowflake) >> 22) + DISCORD_EPOCH_MS


def split_text(text: str, limit: int = MESSAGE_LIMIT) -> list[str]:
    """Cut `text` into parts of at most `limit` characters, at a newline or space when one is near."""
    parts: list[str] = []
    rest = text
    while len(rest) > limit:
        cut = rest.rfind("\n", 0, limit)
        if cut < limit // 2:
            cut = rest.rfind(" ", 0, limit)
        if cut < limit // 2:
            cut = limit
        parts.append(rest[:cut])
        rest = rest[cut:].lstrip("\n ") if cut < limit else rest[cut:]
    if rest or not parts:
        parts.append(rest)
    return parts


def choices_from_question(question: object) -> list[str]:
    """The answer options of an outbox `question`, whichever shape the hub uses."""
    if isinstance(question, list):
        options = cast("list[object]", question)
    elif isinstance(question, dict):
        data = Question.model_validate(question)
        options = data.options or data.choices
    else:
        options = []
    return [option if isinstance(option, str) else str(option) for option in options]


class Bridge:
    def __init__(
        self,
        discord: DiscordApi,
        settings: Settings,
        *,
        state: State,
        inbound: Hub,
        outbox: Hub,
        ack: Hub,
        now: Callable[[], int] | None = None,
        log: Log | None = None,
    ) -> None:
        self.discord = discord
        self.settings = settings
        self.state = state
        self.inbound = inbound
        self.outbox = outbox
        self.ack = ack
        self.now = now or (lambda: int(time.time() * 1000))
        self.log = log
        self.synced = False
        self._lock = threading.RLock()
        self._typing: dict[str, tuple[str, int]] = {}  # sender -> (channel id, started ms)
        self._typing_lock = threading.Lock()

    # -- inbound ---------------------------------------------------------------------------
    def connection_lost(self) -> None:
        """The gateway dropped: live events stand down until the next catch-up succeeds."""
        self.synced = False

    def catch_up(self) -> int:
        """Handle every DM message not yet handled, from the stored cursors. Raises if Discord or
        the hub fails, leaving the cursors at the last message handled, so it can simply be retried.
        Sets `synced` once every channel is read."""
        with self._lock:
            self.synced = False
            channels = self._known_channels()
            delivered = 0
            for channel_id in channels:
                delivered += self._poll_channel(channel_id)
            self.synced = True
            return delivered

    def _known_channels(self) -> list[str]:
        known = self._saved_channels()
        try:
            listed = [c.id for c in self.discord.dm_channels() if c.id]
        except DiscordError as e:
            # Discord documents no way for a bot to list its DMs; the channels we have already
            # seen (saved) still get caught up.
            self._log("could not list DM channels; using the ones already seen", error=str(e))
            listed = []
        for channel_id in listed:
            if channel_id not in known:
                known.append(channel_id)
        if known != self._saved_channels():
            self.state.set(DMS, json.dumps(known))
        return known

    def _saved_channels(self) -> list[str]:
        raw = self.state.get(DMS)
        if not raw:
            return []
        try:
            return [str(c) for c in cast("list[object]", json.loads(raw))]
        except (ValueError, TypeError):
            return []

    def _remember_channel(self, user_id: str, channel_id: str) -> None:
        known = self._saved_channels()
        if channel_id not in known:
            known.append(channel_id)
            self.state.set(DMS, json.dumps(known))
        self.state.set(f"{DM_OF}{user_id}", channel_id)

    def _poll_channel(self, channel_id: str) -> int:
        key = f"{CURSOR}{channel_id}"
        cursor = self.state.get(key)
        if cursor is None:
            # First sight: look back a bounded time, so the message that opened the DM (say
            # `pair <code>`) is delivered but old history is not replayed.
            window = self.settings.first_sight_minutes * 60 * 1000
            cursor = snowflake_at(self.now() - window)
        delivered = 0
        while True:
            page = sorted(self.discord.messages_after(channel_id, cursor, PAGE), key=lambda m: int(m.id))
            for message in page:
                if self._handle(message):
                    delivered += 1
                cursor = message.id
                self.state.set(key, cursor)
            if len(page) < PAGE:
                return delivered

    def handle_message(self, message: Message) -> bool:
        """A live gateway MESSAGE_CREATE. True if it was delivered to the hub."""
        if message.guild_id:
            return False
        with self._lock:
            if not self.synced:
                return False  # the catch-up will read it from history
            key = f"{CURSOR}{message.channel_id}"
            cursor = self.state.get(key)
            if cursor is not None and int(message.id) <= int(cursor):
                return False
            delivered = self._handle(message)
            self.state.set(key, message.id)
            return delivered

    def _handle(self, message: Message) -> bool:
        """Forward one message from the user. The caller advances the cursor afterwards; if the hub
        call raises, it does not, and the message is read again."""
        if message.author.id == self.discord.bot_id or message.author.bot:
            return False
        if message.type not in USER_MESSAGE_TYPES or not message.id:
            return False
        sender = message.author.id
        self._remember_channel(sender, message.channel_id)
        files = self._inbound_files(message)
        if not message.content and not files:
            return False
        reply_ref = None
        text = message.content
        if message.message_reference and message.message_reference.message_id:
            reply_ref = self.state.get(f"{REPLY}{message.message_reference.message_id}")
            quote = self._quote(message, sender)
            if quote:
                text = f"{quote}\n\n{text}" if text else quote
        result = self.inbound(sender=sender, text=text, files=files, reply_ref=reply_ref)
        status = InboundResult.model_validate(result).status
        if status != "delivered":
            # E.g. an unpaired sender: the hub ignores it and so do we. Send nothing back.
            self._log("inbound was not delivered", sender=sender, status=status)
            return False
        # A DM is the newest conversation: a reply goes to it.
        self._add_reaction(sender, message.channel_id, message.id)
        answering = self._pending(sender)
        answering.answering = message.id
        self.state.set(f"{PENDING}{sender}", answering.model_dump_json())
        with self._typing_lock:
            self._typing[sender] = (message.channel_id, self.now())
        self._send_typing(message.channel_id)
        return True

    def _quote(self, message: Message, sender: str) -> str:
        """A marked quote of the message this one replies to, so the agent knows what is being
        answered. Uses what Discord sent along, else fetches it; a failure just means no quote."""
        assert message.message_reference is not None
        original = message.referenced_message
        if original is None:
            try:
                original = self.discord.get_message(message.channel_id, message.message_reference.message_id)
            except DiscordError as e:
                self._log(
                    "could not fetch the replied-to message",
                    id=message.message_reference.message_id,
                    error=str(e),
                )
                return ""
        body = original.content.strip()
        if not body and original.attachments:
            body = "(attachment: " + ", ".join(a.filename for a in original.attachments) + ")"
        if not body:
            return ""
        if len(body) > MAX_QUOTE_CHARS:
            body = body[:MAX_QUOTE_CHARS] + "…"
        whose = "your own earlier message" if original.author.id == sender else "Botler's message"
        if original.author.id not in (sender, self.discord.bot_id):
            whose = f"a message by {original.author.username or original.author.id}"
        quoted = "\n".join(f"> {line}" for line in body.splitlines())
        return f"[replying to {whose}]\n{quoted}"

    def _inbound_files(self, message: Message) -> list[dict[str, str]]:
        files: list[dict[str, str]] = []
        for attachment in message.attachments:
            if attachment.size > MAX_INBOUND_FILE_BYTES:
                self._log("inbound file too large, dropped", file=attachment.filename, bytes=attachment.size)
                continue
            try:
                content = self.discord.download(attachment.url)
            except DiscordError as e:
                self._log("could not fetch an inbound file", file=attachment.filename, error=str(e))
                continue
            files.append({"name": attachment.filename, "base64": base64.b64encode(content).decode()})
        return files

    # -- slash commands --------------------------------------------------------------------
    def handle_command(self, interaction: Interaction) -> bool:
        """A `/remind` (the only command) invoked by anyone, anywhere. The client has already
        answered the interaction with a short ephemeral acknowledgement, so here we only forward to
        the hub and start typing in the DM the reply will land in. Trust is the hub's: an `ignored`
        result is answered with a private refusal and nothing else. True if the hub took it."""
        sender = interaction.sender
        if interaction.data.name != "remind":
            self._followup_note(interaction, f"Unknown command /{interaction.data.name}.")
            return False
        when, what = interaction.option("when"), interaction.option("what")
        if not when or not what:
            self._followup_note(interaction, "Usage: /remind when:<e.g. in 2 hours> what:<reminder text>")
            return False
        try:
            result = self.inbound(sender=sender, text=f"Remind me to {what} at {when}")
            status = InboundResult.model_validate(result).status
        except Exception as e:
            self._log("hub rejected a command", sender=sender, error=f"{type(e).__name__}: {e}")
            self._followup_note(interaction, "Botler could not be reached; try again in a minute.")
            return False
        if status != "delivered":
            self._log("command was not delivered", sender=sender, status=status)
            self._followup_note(
                interaction, "Not authorized: this Discord account is not paired with Botler."
            )
            return False
        self._start_typing_in_dm(sender)
        return True

    # -- message context menu --------------------------------------------------------------
    def handle_message_command(self, interaction: Interaction) -> bool:
        """A "Send to Botler" MESSAGE command whose comment modal was just submitted. The client
        has already answered the interaction with a short ephemeral acknowledgement, so here we only
        forward to the hub and start typing in the DM the reply will land in. Trust is the hub's: an
        unpaired invoker is answered privately and nothing reaches the conversation. True if the hub
        took it."""
        target = interaction.target
        sender = interaction.sender
        if target is None:
            self._followup_note(interaction, "Discord did not send the message; nothing was forwarded.")
            return False
        try:
            files = self._inbound_files(target)
            payload_note, payload_file = self._forward_payload(interaction)
            if payload_file is not None:
                files.append(payload_file)
            text = self._forward_text(interaction, target, payload_note)
            self._log(
                "forwarding a message context command",
                sender=sender,
                context=interaction.context,
                target=target.id,
                text_chars=len(text),
                files=len(files),
            )
            result = self.inbound(sender=sender, text=text, files=files)
            status = InboundResult.model_validate(result).status
        except Exception as e:
            self._log("hub rejected a forwarded message", sender=sender, error=f"{type(e).__name__}: {e}")
            self._followup_note(interaction, "Botler could not be reached; try again in a minute.")
            return False
        if status != "delivered":
            self._log("forwarded message was not delivered", sender=sender, status=status)
            self._followup_note(
                interaction, "Not authorized: this Discord account is not paired with Botler."
            )
            return False
        self._start_typing_in_dm(sender)
        return True

    def _forward_text(self, interaction: Interaction, target: Message, payload_note: str = "") -> str:
        """The text handed to the hub for a forwarded message: source, author, a link, every
        attachment with its filename, content type, size and Discord's signed URL, custom-emoji image
        URLs, the raw content (so <:name:id> / <a:name:id> survives), the user's comment and where
        the full interaction payload can be found. Names are only known when the bot is in the
        server; otherwise the source is marked unknown."""
        author = target.author
        name = author.username or author.id
        if interaction.context == GUILD_CONTEXT:
            channel = (
                f"#{interaction.channel_name}" if interaction.channel_name else f"channel {target.channel_id}"
            )
            where = (
                f"{channel} in {interaction.guild_name}"
                if interaction.guild_name
                else f"{channel} (source server unknown to me)"
            )
        elif interaction.context == BOT_DM_CONTEXT:
            where = "a DM with me"
        else:
            where = "a private channel"
        header = [f"[forwarded from Discord: {where}]", f"author: {name} ({author.id})"]
        if interaction.context == GUILD_CONTEXT and interaction.guild_id:
            header.append(
                f"link: https://discord.com/channels/{interaction.guild_id}/{target.channel_id}/{target.id}"
            )
        if target.attachments:
            header.append("attachments:")
            for attachment in target.attachments:
                mime = attachment.content_type or "unknown type"
                header.append(f"- {attachment.filename} ({mime}, {attachment.size} bytes): {attachment.url}")
        emoji = self._emoji_images(target.content)
        if emoji:
            header.append("custom emoji images:")
            header.extend(f"- {line}" for line in emoji)
        body = target.content if target.content else "(no text)"
        text = "\n".join(header) + f"\n\n{body}"
        if interaction.comment.strip():
            text += f"\n\n[comment]: {interaction.comment.strip()}"
        if payload_note:
            text += f"\n\n{payload_note}"
        return text

    def _forward_payload(self, interaction: Interaction) -> tuple[str, dict[str, str] | None]:
        """The entire interaction `data` (including `resolved` and `message_snapshots`, nothing
        dropped) as raw JSON, with the user's comment added. Small payloads ride inline in the
        message; larger ones become a `forwarded-message.json` file so none of it is truncated.
        Returns (the note for the summary, the file to attach or None)."""
        payload = dict(interaction.raw_data)
        if interaction.comment.strip():
            payload["comment"] = interaction.comment.strip()
        raw = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        size = len(raw.encode("utf-8"))
        if size <= MAX_INLINE_JSON_CHARS:
            return f"payload: {size} bytes (inline below)\n{raw}", None
        encoded = base64.b64encode(raw.encode("utf-8")).decode()
        return f"payload: {size} bytes (full JSON in {FORWARD_JSON_NAME})", {
            "name": FORWARD_JSON_NAME,
            "base64": encoded,
        }

    def _emoji_images(self, content: str) -> list[str]:
        """The CDN image URL of every distinct custom emoji in `content` (`.gif` when animated)."""
        images: list[str] = []
        seen: set[str] = set()
        for animated, emoji_name, emoji_id in CUSTOM_EMOJI.findall(content):
            if emoji_id in seen:
                continue
            seen.add(emoji_id)
            extension = "gif" if animated else "png"
            images.append(
                f"<:{emoji_name}:{emoji_id}> -> https://cdn.discordapp.com/emojis/{emoji_id}.{extension}"
            )
        return images

    def _followup_note(self, interaction: Interaction, text: str) -> None:
        """Answer a deferred interaction ephemerally: a short acknowledgement or a refusal. It is a
        best-effort side channel — the real reply goes to the DM — so a failure is only logged."""
        pending = PendingInteraction(
            id=interaction.id,
            token=interaction.token,
            application_id=interaction.application_id,
            ephemeral=True,
        )
        try:
            self.discord.followup(pending, text, [])
        except DiscordError as e:
            self._log("could not answer an interaction", id=interaction.id, error=str(e))

    def _start_typing_in_dm(self, sender: str) -> None:
        """Show "Botler is typing" in the DM the reply will land in, so an interaction that only
        gets a short ephemeral ack still shows activity where the user is looking. Best effort."""
        try:
            channel_id = self.channel_for(sender)
        except DiscordError as e:
            self._log("could not open the DM to show typing", sender=sender, error=str(e))
            return
        with self._typing_lock:
            self._typing[sender] = (channel_id, self.now())
        self._send_typing(channel_id)

    # -- reactions and typing --------------------------------------------------------------
    def _pending(self, sender: str) -> Pending:
        raw = self.state.get(f"{PENDING}{sender}")
        if not raw:
            return Pending()
        try:
            return Pending.model_validate_json(raw)
        except ValueError:
            return Pending()

    def _add_reaction(self, sender: str, channel_id: str, message_id: str) -> None:
        """React to a delivered message and remember it, so the reply (or a later one, after a
        restart) clears it. A failure is logged and never breaks forwarding."""
        try:
            self.discord.add_reaction(channel_id, message_id, self.settings.reaction_emoji)
        except DiscordError as e:
            self._log("could not add a reaction", msg=message_id, error=str(e))
            return
        pending = self._pending(sender)
        pending.reactions.append(PendingReaction(channel_id=channel_id, message_id=message_id))
        self.state.set(f"{PENDING}{sender}", pending.model_dump_json())

    def _clear_pending(self, sender: str) -> None:
        """Stop typing and remove the reactions. A removal that fails stays pending for next time."""
        with self._typing_lock:
            self._typing.pop(sender, None)
        pending = self._pending(sender)
        if not pending.reactions and not pending.answering:
            return
        remaining: list[PendingReaction] = []
        for item in pending.reactions:
            try:
                self.discord.remove_reaction(item.channel_id, item.message_id, self.settings.reaction_emoji)
            except DiscordError as e:
                if not e.permanent:  # a deleted message or reaction cannot be removed: let it go
                    remaining.append(item)
                self._log("could not remove a reaction", msg=item.message_id, error=str(e))
        self.state.set(f"{PENDING}{sender}", Pending(reactions=remaining).model_dump_json())

    def typing_tick(self) -> int:
        """Refresh typing in every DM with a message still being handled; returns how many."""
        with self._typing_lock:
            active = list(self._typing.items())
        sent = 0
        for sender, (channel_id, started) in active:
            if self.now() - started > PENDING_WINDOW_MS:
                with self._typing_lock:
                    self._typing.pop(sender, None)
                continue
            self._send_typing(channel_id)
            sent += 1
        return sent

    def _send_typing(self, channel_id: str) -> None:
        try:
            self.discord.typing(channel_id)
        except Exception as e:  # typing is best effort
            self._log(
                "could not send a typing indicator", channel=channel_id, error=f"{type(e).__name__}: {e}"
            )

    # -- outbound --------------------------------------------------------------------------
    def drain_outbox(self, wait_seconds: int) -> int:
        """Send what the hub has, acknowledging each once sent; returns how many were sent."""
        batch = OutboxBatch.model_validate(self.outbox(wait_seconds=wait_seconds))
        sent = 0
        for message in batch.messages:
            try:
                self.send(message)
            except DiscordError as e:
                self._log(
                    "could not send an outbox message", id=message.id, error=str(e), permanent=e.permanent
                )
                if not e.permanent:
                    break  # unacknowledged: the hub sends it again, the loop backs off
                # Discord will never take this one (e.g. the user closed their DMs); do not let it
                # block every reply behind it.
            except ValueError as e:
                self._log("dropped an unsendable outbox message", id=message.id, error=str(e))
            self.ack(id=message.id)
            sent += 1
        return sent

    def channel_for(self, user_id: str) -> str:
        """The DM channel with a user: the one we know, else open (or fetch) it."""
        channel_id = self.state.get(f"{DM_OF}{user_id}")
        if channel_id:
            return channel_id
        channel_id = self.discord.open_dm(user_id).id
        self._remember_channel(user_id, channel_id)
        return channel_id

    def send(self, message: OutboxMessage) -> None:
        if not message.sender:
            raise ValueError("the outbox message has no sender to reply to")
        self.post(message.sender, self._with_choices(message), message, message.id)

    def send_dm(self, user_id: str, text: str) -> str:
        """Proactive DM (the MCP tool). Returns the id of the first message posted."""
        if not user_id.isdigit():
            raise ValueError(f"user_id must be a Discord user id (digits only), got {user_id!r}")
        if not text.strip():
            raise ValueError("text is empty")
        return self.post(user_id, text, None, None)

    def _outbound_files(self, message: OutboxMessage | None) -> list[tuple[str, bytes]]:
        """The hub's attachments as Discord files. A malformed or empty one is skipped (and logged),
        never allowed to drop the whole reply with it."""
        files: list[tuple[str, bytes]] = []
        for f in message.files if message else []:
            try:
                content = base64.b64decode(f.base64)
            except ValueError as e:  # binascii.Error is a ValueError
                self._log("skipping an attachment with invalid base64", name=f.name, error=str(e))
                continue
            if not content:
                self._log("skipping an empty attachment", name=f.name)
                continue
            files.append((f.name or "file", content))
        return files

    def post(self, user_id: str, text: str, message: OutboxMessage | None, outbox_id: str | None) -> str:
        """Post a reply into the DM, splitting long text and attaching files to the last part.
        Every reply (including one answering a slash command or a forwarded message) lands in the
        DM: the user's conversation is the DM, not the ephemeral interaction follow-up."""
        files = self._outbound_files(message)
        reply_to = self._reply_target(user_id, message)
        parts = split_text(text)
        channel_id = self.channel_for(user_id)
        done = int(self.state.get(f"{PROGRESS}{outbox_id}") or 0) if outbox_id else 0
        first_id = self.state.get(f"{SENT}{outbox_id}") or "" if outbox_id else ""
        for index, part in enumerate(parts):
            if index < done:
                continue  # posted before a failure; a retry does not repeat it
            last = index == len(parts) - 1
            try:
                posted = self.discord.send(
                    channel_id, part, files if last else [], reply_to if index == 0 else None
                )
            except DiscordError as e:
                if not (e.permanent and last and files):
                    raise
                # The attachments are the problem (too large, unsupported, ...): the text still
                # matters, so send it without them instead of dropping the whole reply.
                self._log(
                    "could not attach the files; sending the text alone",
                    outbox=outbox_id,
                    files=len(files),
                    error=str(e),
                )
                posted = self.discord.send(channel_id, part, [], reply_to if index == 0 else None)
            first_id = first_id or posted.id
            if outbox_id:
                self.state.set(f"{PROGRESS}{outbox_id}", str(index + 1))
                self.state.set(f"{SENT}{outbox_id}", first_id)
                self.state.set(f"{REPLY}{posted.id}", outbox_id)
        self._clear_pending(user_id)
        return first_id

    def _reply_target(self, user_id: str, message: OutboxMessage | None) -> str | None:
        """The Discord message to answer with a reply: the one the outbox names, else the user's
        newest delivered message. A proactive message (no outbox message) answers nothing."""
        if message is None:
            return None
        if message.reply_ref:
            named = self.state.get(f"{SENT}{message.reply_ref}")
            if named:
                return named
        return self._pending(user_id).answering or None

    def _with_choices(self, message: OutboxMessage) -> str:
        text = message.text
        if not message.question:
            return text
        options = choices_from_question(message.question)
        if not [o for o in options if o not in text]:
            return text
        lines = "\n".join(f"- {option}" for option in options)
        return f"{text}\n\n{lines}".strip()

    def _log(self, event: str, /, **fields: object) -> None:
        if self.log is not None:
            self.log(event, **fields)
