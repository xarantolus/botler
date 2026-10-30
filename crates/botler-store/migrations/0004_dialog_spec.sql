-- Typed dialogs carry their structured request (e.g. which secret fields to enter).
ALTER TABLE dialogs ADD COLUMN spec TEXT;
