-- Which reviewer instance a proposal is waiting on, and how often the pipeline re-ran because
-- main moved underneath it.
ALTER TABLE proposals ADD COLUMN reviewer TEXT;
ALTER TABLE proposals ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
