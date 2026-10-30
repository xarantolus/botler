-- A turn whose model calls failed and that the hub retries (one per instance). `next_at` is when
-- the next attempt is sent; NULL while an attempt is in flight.
CREATE TABLE turn_retries (
    instance TEXT PRIMARY KEY,
    event_id TEXT,
    attempts INTEGER NOT NULL,
    error TEXT NOT NULL,
    next_at TEXT
);

-- `sender` waits for `recipient` to answer (a reply or an end-of-turn notice) until `due_at`.
-- `sender` is an instance name, or `hub` for the hub's own waits (the reviewer).
CREATE TABLE reply_expectations (
    sender TEXT NOT NULL,
    recipient TEXT NOT NULL,
    event_id TEXT,
    due_at TEXT NOT NULL,
    PRIMARY KEY (sender, recipient)
);
CREATE INDEX reply_expectations_recipient ON reply_expectations (recipient);
