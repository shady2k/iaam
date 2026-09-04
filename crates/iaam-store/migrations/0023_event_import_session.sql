-- The import session a fact was committed out of, lifted out of the payload so
-- the journal can be narrowed by it.
--
-- The value already travels inside `payload` as part of the event's provenance;
-- this column is the same value in the one place a page of the journal can be
-- selected by. Filtering in Rust after the fact was the alternative and is not
-- one: the listing is paginated in SQL, so a filter applied afterwards would
-- return short pages and a cursor that skips rows.
--
-- Nullable, and NULL is the honest answer for every row already recorded as
-- well as for every route that writes without a session — a direct operation
-- submission, a broker synchronisation, a correction. The two cases are not
-- separable and nothing here pretends otherwise: the journal is append-only, so
-- a fact committed by a session before this column existed cannot be told from
-- one that passed through none.
ALTER TABLE events ADD COLUMN import_session TEXT;

-- Partial, on the rows that name a session. The unnamed rows are the majority
-- and are never what this index is asked about.
CREATE INDEX events_by_import_session
    ON events (owner, import_session)
    WHERE import_session IS NOT NULL;
