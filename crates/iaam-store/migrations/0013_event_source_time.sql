-- Source time is nullable: existing journal rows have a day but no invented moment.
-- Their payloads remain valid and continue to sort after timed events.
ALTER TABLE events ADD COLUMN source_time TEXT;
