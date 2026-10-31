-- A turn in flight: set when a turn-starting delivery is handed to pi (or pi starts a turn),
-- cleared when it settles or its container exits. Still set at boot means the hub stopped mid-turn;
-- the hub then wakes the chat to continue. `turn_interruptions` counts boots in a row that found it
-- mid-turn (reset when a turn settles), so a turn that keeps taking the hub down is not resumed forever.
ALTER TABLE instances ADD COLUMN turn_started_at TEXT;
ALTER TABLE instances ADD COLUMN turn_interruptions INTEGER NOT NULL DEFAULT 0;
