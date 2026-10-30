-- Where a proposal's pipeline is (and since when), and who decided it, why, and the user's reason.
ALTER TABLE proposals ADD COLUMN stage TEXT;
ALTER TABLE proposals ADD COLUMN stage_at TEXT;
ALTER TABLE proposals ADD COLUMN decision TEXT;
ALTER TABLE proposals ADD COLUMN decided_by TEXT;
ALTER TABLE proposals ADD COLUMN decided_at TEXT;
ALTER TABLE proposals ADD COLUMN decision_reason TEXT;
