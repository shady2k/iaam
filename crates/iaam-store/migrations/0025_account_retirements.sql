-- The owner's statement that one of his products ceased to exist (iaam-gua5).
--
-- The second axis. `contour_accounts` says which accounts a calculation folds
-- over; this says which products still exist. They were one axis for as long as
-- the schema had only the first, and the cost showed on a term deposit that was
-- closed and its balance returned to another account of the owner's: keeping it
-- in the contour keeps the interest an earning and the closing movement
-- internal, and leaves a zero-balance row in every asset report for ever;
-- dropping it from a later contour version removes the row and destroys both of
-- the others.
--
-- An APPEND-ONLY HISTORY, and every column here follows from that.
--
-- `revision` is a per-owner monotone coordinate over the whole table, minted by
-- every accepted call. A report states the revision it read, so an asset
-- snapshot stays reproducible after a further retirement — the property
-- `contour_versions` buys with a full copy of the membership per version, bought
-- here with one row per statement instead. The statements in force at revision R
-- are, per account, the row with the greatest revision not above R.
--
-- It is the PRIMARY KEY together with the owner, and the account deliberately is
-- not: one account may be retired, withdrawn and retired again, and each of
-- those is a row. What refuses a second retirement is the application, which
-- reads the standing statement first — a constraint here could only refuse the
-- second row outright, and would then refuse the legitimate re-declaration after
-- a withdrawal as well.
--
-- `effective_on` NULL is the withdrawal: the owner taking the statement back.
-- A row rather than a delete, because deleting one would change what an earlier
-- revision said, which is the whole thing the coordinate exists to prevent.
-- NULL is never «he has not said» here — that state is the absence of any row
-- for the account, exactly as it is in `account_scope_exclusions`.
--
-- No CHECK on the date's relation to today: what may be declared is a rule about
-- the moment of the call, and the same row is perfectly valid a day later. The
-- refusal lives where the clock is.
--
-- Nothing in this table is ever read by contour classification. A retired
-- account stays a contour member; a retirement that moved the perimeter would be
-- the retroactive rewriting of history that the triggers below, and the ones on
-- `contour_versions`, exist to refuse.
CREATE TABLE account_retirements (
    owner        TEXT NOT NULL,
    revision     INTEGER NOT NULL,
    account      TEXT NOT NULL,
    -- The date in the owner's own history that the product ceased on, or NULL
    -- where this row withdraws the statement before it.
    effective_on TEXT,
    recorded_at  TEXT NOT NULL,
    PRIMARY KEY (owner, revision),
    -- The same foreign key the contour composition and the scope dispositions
    -- carry, for the same reason: an identifier is not an access right, and a
    -- declaration recorded against someone else's account is a statement about
    -- someone else's money.
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

-- The statements in force are read per account, and the read walks backwards
-- from the newest revision. Without this index that read is a scan of every
-- retirement the owner ever declared, once per report.
CREATE INDEX account_retirements_by_account
    ON account_retirements (owner, account, revision);

CREATE TRIGGER account_retirements_are_immutable
BEFORE UPDATE ON account_retirements
BEGIN
    SELECT RAISE(ABORT, 'a retirement is a revision: record a further one');
END;

-- Deletion is refused beside modification, for the reason the contour tables
-- give: a ban on UPDATE alone catches an edited row and lets DELETE + INSERT
-- through, and the result is the same — a revision an already-published report
-- named now says something else.
CREATE TRIGGER account_retirements_are_not_deletable
BEFORE DELETE ON account_retirements
BEGIN
    SELECT RAISE(ABORT, 'a retirement is a revision: withdraw it with a further one');
END;
