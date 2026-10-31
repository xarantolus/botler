-- Signed-in devices (browsers): each holds an HttpOnly cookie whose secret is stored hashed here.
CREATE TABLE devices (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    revoked_at TEXT
);

-- One-time sign-in codes, made on a signed-in device or with `botler login-code`; stored hashed.
CREATE TABLE login_codes (
    code_hash TEXT PRIMARY KEY,
    expires_at TEXT NOT NULL,
    used_at TEXT
);
