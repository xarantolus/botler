-- Human-friendly names for chats. `name` stays the immutable id (session files, branches,
-- tokens); `slug` is a unique, renamable alias usable wherever an instance is addressed.
ALTER TABLE instances ADD COLUMN title TEXT;
ALTER TABLE instances ADD COLUMN slug TEXT;
CREATE UNIQUE INDEX instances_slug ON instances (slug) WHERE slug IS NOT NULL;
CREATE INDEX deliveries_event ON deliveries (event_id);
CREATE INDEX runs_event ON runs (event_id) WHERE event_id IS NOT NULL;
