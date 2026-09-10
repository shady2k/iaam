-- The owner's, or an agent's, statement that an account row should never
-- have existed (`iaam-o0oj`).
--
-- Distinct from both `account_retirements` (the product existed and ended)
-- and `account_scope_exclusions` (the product exists and the owner's reports
-- leave it out): this says there was never an account here to report on or
-- ask a statement for. A field session against a live instance found three
-- accounts holding both of the other two declarations and still standing in
-- every report's caveat register, because neither declaration says "this row
-- was a mistake" — see `iaam_core::retraction`'s own doc comment for why the
-- distinction is not a nuance.
--
-- A current statement, not a history, the same shape `account_scope_exclusions`
-- already uses and for the same reason: a retraction has no date the owner
-- chooses and nothing about the account it names is versioned by it — unlike
-- a retirement, which suppresses a row in the asset snapshot from a date the
-- owner states and so must stay reproducible against every report published
-- while it stood. Presence is the whole of the statement, and the row is
-- deleted rather than left standing with a withdrawn flag, exactly as
-- `clear_account_scope_exclusion` deletes rather than writes a third value:
-- the absence of a row is what "not retracted" means everywhere else in this
-- schema, and two ways of spelling one state is how they disagree.
--
-- No `reason` column, unlike `account_scope_exclusions`: that table's reason
-- explains a judgement a year later cannot reconstruct from the fact alone.
-- A retraction has no judgement to record beyond "this was a mistake", and
-- who made the call is already carried on `decision_history`, joined by
-- subject, the way every other standing decision's actor is.
CREATE TABLE account_retractions (
    owner       TEXT NOT NULL,
    account     TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (owner, account),
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;
