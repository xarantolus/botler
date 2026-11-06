-- How many of a call's tokens were context read back from the provider's cache (billed at a
-- fraction of new input): what tells resent context from new work.
ALTER TABLE usage ADD COLUMN cache_read INTEGER NOT NULL DEFAULT 0;
