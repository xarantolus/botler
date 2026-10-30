-- Archiving replaces "ended": an archived chat is hidden but kept whole (session, workspace),
-- carries the agent's handover report, and is restored by messaging it.
UPDATE instances SET state = 'archived' WHERE state = 'ended';
ALTER TABLE instances ADD COLUMN archive_requested_at TEXT;
ALTER TABLE instances ADD COLUMN archive_report TEXT;
