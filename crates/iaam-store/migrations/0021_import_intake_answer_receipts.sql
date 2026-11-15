-- The answer one intake request gave for one row, and the answer one answer
-- request gave for one question (ADR 0042 point 4, `iaam-mkbfh.9.1`).
--
-- A request is recognised by its natural key — the session and the row for an
-- intake, the session and the question for an answer — so a repeat after a lost
-- response finds the first answer here instead of writing the request again or
-- being refused as «session not open» / «already answered». Both columns are
-- opaque JSON: what a request answered is the application's vocabulary, and the
-- store keeps it the way it keeps an observation's payload, validating only that
-- it parses.
--
-- Each is written in the same transaction as the writes it describes, so a
-- receipt cannot exist for a row nothing held or for a question nothing
-- answered. Both are nullable rather than NOT NULL: a row whose request has not
-- answered it yet and a question nobody has answered have no answer to record,
-- and nothing is back-filled, so a NULL is silence and never a claim.
ALTER TABLE import_observations ADD COLUMN intake_receipt TEXT;
ALTER TABLE import_questions ADD COLUMN answer_receipt TEXT;
