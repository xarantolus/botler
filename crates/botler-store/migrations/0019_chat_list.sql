-- The sidebar: chats the user pinned come first, the rest by when someone last wrote in them
-- (`last_active` also moves on every wake and event-driven turn).
ALTER TABLE instances ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE instances ADD COLUMN last_message_at TEXT;
UPDATE instances SET last_message_at = last_active;
-- The user named the chat: agents no longer retitle it.
ALTER TABLE instances ADD COLUMN titled_by_user INTEGER NOT NULL DEFAULT 0;
