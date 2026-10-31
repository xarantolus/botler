-- Action requests: the action's code version (its image tag) when it was invoked. Approval runs
-- that code or nothing.
ALTER TABLE proposals ADD COLUMN code_version TEXT;
