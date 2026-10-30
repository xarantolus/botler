-- botler runtime state. Only facts that are not definitions live here; definitions are in the
-- monorepo. Times are RFC 3339 UTC strings (sortable), ids are UUIDv7 strings (time-ordered).

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- Append-only event journal. Journaled before anything acts on it (outbox).
CREATE TABLE events (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    source      TEXT NOT NULL,           -- Source::key(), e.g. agent:main
    body        TEXT NOT NULL,           -- full Event JSON
    to_instance TEXT,
    correlation TEXT,
    at          TEXT NOT NULL,
    routed      INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX events_kind_at ON events (kind, at);
CREATE INDEX events_unrouted ON events (routed) WHERE routed = 0;
CREATE INDEX events_correlation ON events (correlation) WHERE correlation IS NOT NULL;

-- Agent instances: persistent ones (one per persistent definition) and spawned ephemeral ones.
CREATE TABLE instances (
    name         TEXT PRIMARY KEY,
    definition   TEXT NOT NULL,
    persistent   INTEGER NOT NULL,
    state        TEXT NOT NULL,           -- dormant | starting | running | ended
    session_file TEXT NOT NULL,
    branch       TEXT,
    container_id TEXT,
    parent       TEXT,                    -- spawning instance
    task         TEXT,                    -- spawn task prompt
    created_at   TEXT NOT NULL,
    last_active  TEXT NOT NULL
) STRICT;

-- Queue of events to put into instances. At-least-once; the pi extension dedups by id.
CREATE TABLE deliveries (
    id           TEXT PRIMARY KEY,
    event_id     TEXT NOT NULL REFERENCES events (id),
    instance     TEXT NOT NULL,
    trigger_id   TEXT,
    prompt       TEXT,
    status       TEXT NOT NULL,           -- pending | delivered | dropped
    attempts     INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    delivered_at TEXT,
    note         TEXT
) STRICT;
CREATE INDEX deliveries_pending ON deliveries (instance, created_at) WHERE status = 'pending';

-- One-off timers set at runtime (set_timer). Schedules are definitions, not rows.
CREATE TABLE timers (
    id         TEXT PRIMARY KEY,
    fire_at    TEXT NOT NULL,
    target     TEXT NOT NULL,             -- Target JSON
    prompt     TEXT,
    payload    TEXT NOT NULL,
    created_by TEXT NOT NULL,
    status     TEXT NOT NULL,             -- pending | fired | cancelled
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX timers_pending ON timers (fire_at) WHERE status = 'pending';

-- Per schedule trigger: the facts needed to recompute the timetable.
CREATE TABLE schedule_state (
    trigger_id TEXT PRIMARY KEY,
    anchor     TEXT NOT NULL,             -- when this trigger was first applied
    last_fired TEXT,
    skipped    INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Container runs: jobs, services, actions, gate runs, replays.
CREATE TABLE runs (
    id           TEXT PRIMARY KEY,
    runnable     TEXT NOT NULL,
    kind         TEXT NOT NULL,           -- oneshot | service | action | gate | replay
    trigger_id   TEXT,
    event_id     TEXT,
    commit_id    TEXT,
    status       TEXT NOT NULL,           -- queued | running | succeeded | failed | timeout | cancelled
    container_id TEXT,
    exit_code    INTEGER,
    reason       TEXT,
    started_at   TEXT,
    finished_at  TEXT,
    created_at   TEXT NOT NULL
) STRICT;
CREATE INDEX runs_runnable ON runs (runnable, created_at);
CREATE INDEX runs_active ON runs (status) WHERE status IN ('queued', 'running');

-- One incident per runnable failure streak; repeated failures bump the count.
CREATE TABLE incidents (
    id         TEXT PRIMARY KEY,
    runnable   TEXT NOT NULL,
    kind       TEXT NOT NULL,
    status     TEXT NOT NULL,             -- open | resolved
    count      INTEGER NOT NULL,
    first_at   TEXT NOT NULL,
    last_at    TEXT NOT NULL,
    last_run   TEXT,
    resolution TEXT
) STRICT;
CREATE UNIQUE INDEX incidents_open ON incidents (runnable) WHERE status = 'open';

-- Questions and approvals for the user.
CREATE TABLE dialogs (
    id           TEXT PRIMARY KEY,
    instance     TEXT NOT NULL,
    kind         TEXT NOT NULL,           -- question | approval
    question     TEXT NOT NULL,
    detail       TEXT,
    ctas         TEXT NOT NULL,           -- JSON array of strings
    about        TEXT NOT NULL,           -- JSON array of invalidation targets
    priority     TEXT NOT NULL,           -- blocking | normal | info
    proposal_id  TEXT,
    status       TEXT NOT NULL,           -- open | answered | aborted | expired
    answer       TEXT,
    abort_reason TEXT,
    expires_at   TEXT,
    created_at   TEXT NOT NULL,
    resolved_at  TEXT
) STRICT;
CREATE INDEX dialogs_open ON dialogs (status) WHERE status = 'open';

-- Monorepo change proposals and approval-gated action invocations.
CREATE TABLE proposals (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,           -- change | action
    instance     TEXT NOT NULL,
    title        TEXT NOT NULL,
    branch       TEXT,
    base_commit  TEXT,
    head_commit  TEXT,
    action       TEXT,
    args         TEXT,
    args_hash    TEXT,
    summary      TEXT,                    -- ChangeSummary JSON
    gate_report  TEXT,
    verdict      TEXT,
    review_notes TEXT,
    status       TEXT NOT NULL,           -- gating | reviewing | awaiting_user | merged | approved | rejected | failed
    merged_commit TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

-- Secret values, encrypted with the key file outside the database.
CREATE TABLE secrets (
    name       TEXT PRIMARY KEY,
    nonce      BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

-- Model usage per call, from pi message_end events.
CREATE TABLE usage (
    id       INTEGER PRIMARY KEY,
    instance TEXT NOT NULL,
    at       TEXT NOT NULL,
    model    TEXT NOT NULL,
    tokens   INTEGER NOT NULL,
    cost_usd REAL NOT NULL
) STRICT;
CREATE INDEX usage_instance_at ON usage (instance, at);

-- Small per-runnable key-value state (poller cursors, dedup keys).
CREATE TABLE kv (
    owner      TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (owner, key)
) STRICT;

-- Tripped circuit breakers; a source stays paused until cleared.
CREATE TABLE breakers (
    subject    TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    reason     TEXT NOT NULL,
    tripped_at TEXT NOT NULL
) STRICT;
