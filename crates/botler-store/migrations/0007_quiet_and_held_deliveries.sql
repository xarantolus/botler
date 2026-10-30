-- Quiet deliveries (acknowledgements between agents) are added to the session without starting a
-- turn and never wake a sleeping agent. Held deliveries (status 'held', note = breaker subject)
-- wait for the user to clear a pair breaker.
ALTER TABLE deliveries ADD COLUMN quiet INTEGER NOT NULL DEFAULT 0;
CREATE INDEX deliveries_held ON deliveries (note) WHERE status = 'held';
CREATE INDEX events_to_at ON events (to_instance, at);
