-- How many spawns deep an instance is: 0 for chats the user or a trigger started, parent + 1 for
-- agents spawned by agents. Bounds agent trees (`max_spawn_depth`).
ALTER TABLE instances ADD COLUMN depth INTEGER NOT NULL DEFAULT 0;
