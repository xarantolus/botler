# discord

The botler channel for Discord: **DMs**, the `/remind` slash command, and a **"Send to Botler"
message context-menu command**. A `lifetime = "service"` job with `channel = true` and
`serves_mcp = true`.

## Enabling it

This is an extra (see `extras/README.md`): it runs once it is moved to `jobs/discord` (its
Dockerfile copies from there). Then add `mcp = ["discord"]` to the agents that should be able to DM
you (`agents/botler/agent.toml`), propose the change, and enter the bot token (secret group
`discord`, set-up steps below). Then pair your Discord account: in the web app, Settings → Channels →
**Pair an account**, and DM the bot `pair <code>`; until then the bot ignores you.

## What it does

- **Inbound (gateway, never polling).** discord.py holds the gateway websocket (heartbeats,
  resume and reconnect are discord.py's). Intents are `Intents.none()` + `DIRECT_MESSAGES`; no
  privileged intent (no Message Content, no Guild Messages). Discord's gateway docs list "content
  in DMs with the app" as an exception to the Message Content intent, so DM text arrives. Guild
  messages are never subscribed to and are dropped if they appear. Each DM from a user goes to
  `channel_inbound(sender=<discord user id>, text, files)`. The bot's own messages are never handled.
  Only `delivered` gets a reaction/typing; `ignored` (unpaired) gets nothing back. Trust is the hub's.
- **Offline catch-up.** On every start and reconnect/resume: `GET /users/@me/channels`, then
  `GET /channels/{id}/messages?limit=50&after=<cursor>`, paginated, oldest first. The per-channel
  cursor (`cursor.<channel id>` in `botler.state`) is the id of the last message looked at, advanced
  after each message, so a hub failure mid-way resumes exactly there. A live `MESSAGE_CREATE` is
  checked against the same cursor (a RESUME replay is a no-op) and stands down until the catch-up
  has succeeded on the current connection, so it can never move the cursor past a missed message.
  A channel seen for the first time looks back `first_sight_minutes` (default 60). Channel ids ever
  seen are kept in state as well, because Discord does not document `GET /users/@me/channels` for
  bots: if it fails, those are still caught up.
- **Outbound.** `channel_outbox` is drained; each message is posted to the sender's DM
  (`POST /users/@me/channels` once, then remembered), split at 2000 characters, files uploaded as
  multipart with the last part, mentions disabled, then acked. A retry after a partial send does not
  repeat parts already posted. A transient failure leaves the message unacked (retried); a permanent
  one (403/404, e.g. the user closed their DMs) is logged and acked so it cannot block the queue.
  `reply_ref.<discord message id>` → outbox id maps a user's reply back (`reply_ref` on inbound);
  `sent.<outbox id>` → message id lets an outbox message that carries a `reply_ref` set
  `message_reference`.
- **UX.** A delivered message gets 👀 (`PUT .../reactions/%F0%9F%91%80/@me`), removed when the reply
  posts; pending (channel, message) pairs persist, a failed removal is retried later and never
  dropped. Typing (`POST /channels/{id}/typing`) is refreshed every 8 s until the reply posts or
  30 minutes pass.
- **`/remind when:<text> what:<text>`.** A global application command, installable to servers and
  to users, usable in guilds, the bot's DM and group DMs. It arrives as `INTERACTION_CREATE` on the
  gateway (no HTTP endpoint). The command callback answers the interaction itself, **at once and
  unconditionally**, with a short **ephemeral acknowledgement** ("Reminder received — I'll answer
  in our DM"); the hub's reply, like every reply, is posted to the **DM** (see below), and typing
  shows there. `ignored` is answered "Not authorized" (privately) and nothing is kept. The job sets
  no timers: botler does, and DMs the user via `send_dm` when it fires.
- **`Send to Botler` (MESSAGE context menu head).** A global type-3 command, installable to servers
  and to users (`integration_types: [0, 1]`), offered in guilds, the bot's DM and private channels
  (`contexts: [0, 1, 2]`). A message command cannot take text, so invoking it immediately opens an
  ephemeral modal with one optional field, "Add a comment (optional)"; the modal is the 3-second
  acknowledgement. On submit the modal interaction **answers itself at once** with a short
  **ephemeral acknowledgement** and the message is forwarded through the same inbound path as a DM
  (`channel_inbound(sender=<invoking user id>, ...)`) with the target message's
  **raw content** (custom-emoji markup `<:name:id>` / `<a:name:id>` survives), its attachments
  (downloaded from Discord's CDN; oversized ones are dropped; each is also listed with its filename,
  content type, size and the **signed CDN URL** so the receiver can re-post or hand it on) and the
  comment. Custom emoji are additionally listed with their CDN image URL
  (`https://cdn.discordapp.com/emojis/<id>.png`, `.gif` when animated). The **entire interaction
  `data`** (including `resolved` and `message_snapshots`, nothing dropped) also goes along as raw
  JSON: inline when small, else as a `forwarded-message.json` file (never truncated) with its size in
  the summary. The source is marked
  "forwarded from Discord: …" with the author, and `#channel in server` / a Discord message link
  when the bot knows them, else "(source server unknown to me)". Because Discord hands the app the
  resolved message in the interaction, this works for messages in servers the bot is **not** in. The
  interaction **answers itself** with a short **ephemeral acknowledgement** ("Sent to Botler — I'll
  answer in our DM"); the hub's reply is posted to the **DM** like any other reply, and typing shows
  there while botler works. Trust is the hub's: an unpaired invoker is refused privately and nothing
  reaches the conversation.
- **Replies always land in the DM.** The user's conversation with botler is the DM, so a reply to a
  slash command or a forwarded message is posted to the DM (`channel_for(sender)`), never as a full
  interaction follow-up in the server it was invoked from. The interaction only ever carries the
  handler's instant ephemeral ack (or a private refusal/error). `:eyes:`, typing, threading and
  `send_dm` are unchanged.
- **MCP.** `send_dm(user_id, text)` on `http://discord:8071/mcp` messages a user unprompted.

## Using `Send to Botler`

For the command to appear in a user's Apps menu (not just in servers the bot is in), that user must
install the app to their account once:

```
https://discord.com/oauth2/authorize?client_id=<your application id>&integration_type=1&scope=applications.commands
```

Then right-click (or long-press) any message → Apps → **Send to Botler**, optionally add a comment
and submit. It works in servers the bot is not in, and in DMs.

## Configuration

Secret group `discord`, field `DISCORD_BOT_TOKEN`. Settings: `reaction_emoji`,
`first_sight_minutes`, `health_seconds`. A rejected token is the only fatal error; an unreachable
Discord is retried with backoff and shows as unhealthy after ~2 minutes.

## Limits

Inbound attachments over 25 MiB are dropped (downloads only from Discord's CDN). Discord only lets
a bot DM a user who shares a server with it (or has installed it), and who allows DMs.
