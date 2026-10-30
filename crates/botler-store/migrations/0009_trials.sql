-- Trial runs (`trial_run`): a job or service run from an agent's branch for a few minutes. What
-- it emits, sends or schedules is recorded in `effects` (a JSON array) instead of taking effect,
-- and reported to `instance` when it ends.
CREATE TABLE trials (
    run_id   TEXT PRIMARY KEY REFERENCES runs (id),
    instance TEXT NOT NULL,
    branch   TEXT NOT NULL,
    minutes  INTEGER NOT NULL,
    withheld TEXT NOT NULL DEFAULT '[]',  -- secret groups the job declares that it did not get
    effects  TEXT NOT NULL DEFAULT '[]'
) STRICT;
