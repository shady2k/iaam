-- Broker trading metadata is absent on instruments recorded before this migration.
ALTER TABLE instruments ADD COLUMN metadata TEXT CHECK (metadata IS NULL OR json_valid(metadata));
