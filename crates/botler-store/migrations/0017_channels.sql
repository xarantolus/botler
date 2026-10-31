-- Channels (services with `channel = true` that carry the user's messages from Telegram, email, ...).
-- A binding says an outside sender on a channel is the user, and which chat their messages go to.
CREATE TABLE channel_bindings (
    id TEXT PRIMARY KEY,
    channel TEXT NOT NULL,
    sender TEXT NOT NULL,
    target TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);
CREATE UNIQUE INDEX channel_bindings_live ON channel_bindings (channel, sender) WHERE revoked_at IS NULL;

-- One-time pairing codes the user creates in the web app; `pair <code>` from a sender binds it.
CREATE TABLE channel_codes (
    code TEXT PRIMARY KEY,
    target TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT
);

-- Senders that are not bound: their messages are dropped and counted.
CREATE TABLE channel_strangers (
    channel TEXT NOT NULL,
    sender TEXT NOT NULL,
    count INTEGER NOT NULL,
    last_at TEXT NOT NULL,
    PRIMARY KEY (channel, sender)
);

-- What a channel service sends out: replies, questions, notices. `files` is a JSON list of uploads.
CREATE TABLE channel_outbox (
    id TEXT PRIMARY KEY,
    channel TEXT NOT NULL,
    sender TEXT NOT NULL,
    text TEXT NOT NULL,
    files TEXT NOT NULL,
    dialog_id TEXT,
    created_at TEXT NOT NULL,
    sent_at TEXT
);
CREATE INDEX channel_outbox_pending ON channel_outbox (channel, created_at) WHERE sent_at IS NULL;
