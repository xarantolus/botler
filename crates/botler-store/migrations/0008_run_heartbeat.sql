-- Last health heartbeat of a service run (the `health` op with ok = true). A service whose
-- heartbeat is overdue is unhealthy and restarted.
ALTER TABLE runs ADD COLUMN heartbeat_at TEXT;
