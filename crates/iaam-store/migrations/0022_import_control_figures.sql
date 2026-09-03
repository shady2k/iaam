-- The control section a statement prints about itself, held in the session that
-- is importing that statement.
--
-- Still pre-journal, like the three tables of 0019: nothing here is a fact, and
-- nothing here is in `events`. The figures become `ControlAssertion` events at
-- commit, beside the rows they were checked against, and until then they are
-- what the assessment compares the rows with.
--
-- The question this answers is the owner's: «why are we allowed to commit an
-- import that is knowably wrong». Because at commit nothing knew what right
-- looked like — while the source had printed it on the same page as the rows.
CREATE TABLE import_control_figures (
    session         TEXT NOT NULL REFERENCES import_sessions (id) ON DELETE CASCADE,
    -- The account the section is about. Not a foreign key, for the reason the
    -- observations' payload is not one either: a session holds what a source
    -- said, and a section naming an account the directory does not hold is a
    -- finding the assessment must be able to report rather than an insert that
    -- must fail.
    account         TEXT NOT NULL,
    currency        TEXT NOT NULL,
    -- The interval the statement covers, inclusive at both ends, as
    -- `AssertionPeriod` is. Stored per row rather than per session because a
    -- session may hold two statements, and dating one period from the other's
    -- assertions is how a reconciliation comes to cover a month nobody reported.
    period_from     TEXT NOT NULL,
    period_to       TEXT NOT NULL,
    -- Minor units, each independently nullable: a source prints what it prints,
    -- and NULL means «not stated», never zero (§4.9). A zero written in for a
    -- figure nobody stated would manufacture a mismatch out of silence.
    opening         INTEGER,
    closing         INTEGER,
    debit_turnover  INTEGER,
    credit_turnover INTEGER,
    stated_at       TEXT NOT NULL,
    -- One section per account and currency in a session. Restating it replaces
    -- it: a transcription corrected is a correction, and two sections for one
    -- account would let the assessment compare against whichever it read first.
    PRIMARY KEY (session, account, currency)
) STRICT;
