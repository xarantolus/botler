-- The per-chat graph. Grants: resources a chat holds beyond its definition (a secret group, a
-- mount, a service, a restricted event type), applied on every wake until revoked. Subscriptions:
-- event types a chat asked to receive at runtime. Both are suspended while the chat is archived
-- (derived from instances.state, so restoring brings them back unchanged).
CREATE TABLE grants (
    id          TEXT PRIMARY KEY,
    holder      TEXT NOT NULL,           -- instance
    kind        TEXT NOT NULL,           -- secret | mount | service | event
    resource    TEXT NOT NULL,           -- name within the kind
    mode        TEXT,                    -- mounts: ro | rw
    granted_by  TEXT NOT NULL,           -- user | policy | spawn:<parent>
    created_at  TEXT NOT NULL,
    revoked_at  TEXT
) STRICT;
CREATE INDEX grants_live ON grants (holder) WHERE revoked_at IS NULL;

CREATE TABLE subscriptions (
    id          TEXT PRIMARY KEY,
    instance    TEXT NOT NULL,
    pattern     TEXT NOT NULL,           -- event type or glob
    filter      TEXT NOT NULL,           -- JSON object: payload path -> required value
    wake        INTEGER NOT NULL,        -- 0: deliver as a quiet note
    note        TEXT,
    created_at  TEXT NOT NULL
) STRICT;
CREATE INDEX subscriptions_instance ON subscriptions (instance);
