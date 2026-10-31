---
name: channels
description: Build a channel, so the user can talk to Botler from Telegram, Matrix, email, Signal or similar instead of the web app. Use when the user wants to reach Botler from their phone's messenger or by mail.
---

# Channels

A channel is a service with `channel = true` in its `job.toml`. It speaks the outside protocol and
hands messages to the hub; the **hub** decides who counts as the user and where messages go. Start
from `jobs/example-channel` (a spool directory stands in for the protocol) and replace `Spool`.

## What the hub does, so you don't

- **Trust.** A sender is nobody until the user pairs it. The user creates a one-time code under
  Settings → Channels and sends `pair <code>` from the outside account. Never trust a sender
  yourself, keep no allow-list, and send nothing back to strangers (`channel_inbound` returns
  `ignored`; just drop it).
- **Routing.** A paired sender's messages are the user's, marked `via` the channel. They go to
  Botler, or to another chat after `/chat <name>`.
- **Replies.** The reply that ends a turn the channel started, files the agent attaches, and
  questions it asks all come out of `channel_outbox`. Replying with one of a question's choices
  (or replying to the question's message, `reply_ref`) answers it.
- **Approvals, secrets and logins** never go over a channel: the outbox says "waiting for you in
  the web app".

## The three ops (Python: `botler.ops.channel_*`)

- `channel_inbound(sender, text, files=[{name, base64}], reply_ref=None)`. `sender` is the outside
  account's stable id (the Telegram chat id, the address). `reply_ref` is the outbox id the user
  replied to, when the protocol tells you (map it with a small table in `botler.state`).
- `channel_outbox(wait_seconds=30)` is a long poll (at most 50 s), so the service needs no open
  port and no public URL. Each message has `id`, `sender`, `text` (may be empty), `files`
  (`name`, `mime`, `base64`) and `question`.
- `channel_ack(id)` after sending. Unacknowledged messages come again, so a crash between send and
  ack repeats one message. Use the id to make that harmless where the protocol allows.

## Building one

1. Get the protocol's credentials with `request_secrets` (e.g. a `telegram` group with
   `TELEGRAM_BOT_TOKEN`), then grant them in the job's `secrets = [...]`.
2. Run two loops, as the example does: one blocking on the protocol (Telegram `getUpdates` with a
   timeout, IMAP IDLE) and one on `channel_outbox`. Keep protocol cursors (the update offset, the
   last UID) in `botler.state`, so a restart neither repeats nor loses messages.
3. Split long texts the way the protocol needs (Telegram: 4096 characters). Send plain text, or
   convert the markdown agents write.
4. Test with fakes for both sides, like `jobs/example-channel/tests`: a repeat after a crash,
   files both ways, and a stranger.
5. `trial_run` it. In a trial the hub records inbound messages and gives an empty outbox.
6. Propose it. **Every change to a channel asks the user**, not only the first, because it speaks
   as them. Say so in the summary, and tell the user to pair under Settings → Channels once it
   runs.

Telegram, for example, needs no public URL: long-poll `getUpdates`, send with `sendMessage`,
`sendDocument` and `sendPhoto`, and use `message.chat.id` as the sender.
