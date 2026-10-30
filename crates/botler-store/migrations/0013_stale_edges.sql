-- Grants and subscriptions that stopped applying while their chat was archived (the resource or
-- event type is gone, a secret group lost its values): why, set when the chat is restored. A stale
-- edge is kept for the record but no longer applied or routed.
ALTER TABLE grants ADD COLUMN stale TEXT;
ALTER TABLE subscriptions ADD COLUMN stale TEXT;
