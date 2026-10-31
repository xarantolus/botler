-- A proposal's stage history ([{stage, at}], JSON) and the live detail and output tail of the
-- current stage; the commit the gate passed on; the approval (patch id and escalations it
-- covered) that carries over a clean rebase, and how often it did.
ALTER TABLE proposals ADD COLUMN stages TEXT;
ALTER TABLE proposals ADD COLUMN stage_detail TEXT;
ALTER TABLE proposals ADD COLUMN stage_log TEXT;
ALTER TABLE proposals ADD COLUMN gated_commit TEXT;
ALTER TABLE proposals ADD COLUMN approved_patch TEXT;
ALTER TABLE proposals ADD COLUMN approved_escalations TEXT;
ALTER TABLE proposals ADD COLUMN carried_over INTEGER NOT NULL DEFAULT 0;
-- Proposals stuck at gating/merge (a re-run after main moved never ran, also on every boot) had
-- their attempts counted for nothing: give them their re-check.
UPDATE proposals SET attempts = 1 WHERE status = 'gating' AND stage = 'merge';
