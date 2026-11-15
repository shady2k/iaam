-- The answer a commit gave, kept with the session it closed (ADR 0042 point 4).
-- A commit is recognised by the session and the revision its caller read, so a
-- repeat after a lost response finds the first answer here instead of being
-- refused as «session not open». `receipt` is opaque JSON: what a commit
-- answered is the application's vocabulary, and the store keeps it the way it
-- keeps an observation's payload, validating only that it parses.
--
-- Both columns are written by the one statement that commits the session
-- (`commit_import_session_within`), so a receipt cannot exist for a session
-- that was not closed by the commit that wrote it. A session closed as
-- abandoned keeps both NULL, which is why the columns are nullable rather than
-- NOT NULL: there is no answer to record.
ALTER TABLE import_sessions ADD COLUMN commit_revision TEXT;
ALTER TABLE import_sessions ADD COLUMN commit_receipt TEXT;
