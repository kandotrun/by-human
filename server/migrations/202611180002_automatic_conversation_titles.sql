-- Existing and explicitly named conversations keep their titles.
ALTER TABLE conversations ADD COLUMN title_pending BOOLEAN NOT NULL DEFAULT FALSE;
