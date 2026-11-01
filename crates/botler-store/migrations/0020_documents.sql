-- Documents agents write for the user to read (Markdown): a report, a running list, a plan.
-- One row per name; writing it again replaces the body, the previous one kept for "what changed".
CREATE TABLE documents (
    name TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    previous_body TEXT,
    author TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
