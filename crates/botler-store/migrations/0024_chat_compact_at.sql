-- The user's context size for one chat: it compacts at this many tokens instead of its agent's.
ALTER TABLE instances ADD COLUMN compact_at INTEGER;
