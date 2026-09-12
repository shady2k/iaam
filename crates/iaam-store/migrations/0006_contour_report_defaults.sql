-- The contour the owner's reports are about (`iaam-14is`).
--
-- The statement a field report asked for. `GET /v1/contours` listed two
-- contours with literally the same title, different versions and different
-- compositions, and nothing in either row said which of them the owner's
-- reports are about. The agent reading it took the one with the higher version
-- and the wider composition, and it guessed: the figure it went on to report
-- was computed over a perimeter nobody had told it to use, and no response
-- said so. `ContourDto::same_title_contours` marks the collision and stays as
-- it is — the collision is real — but a mark is not an answer. This table is
-- the owner's answer: the one contour his reports are about, stated once.
--
-- **A statement the caller reads, not a substitution the server performs.** No
-- report route reads this table, and none of them changed: `contour` is still a
-- required parameter on the reports that take one, and a request that omits it
-- is refused exactly as it was before this table existed. What a caller — an
-- agent, usually — does instead is read this declaration from the contour list
-- and then name that contour explicitly in the report request. What the
-- declaration removes is the guess: facing two contours with one title and no
-- other fact to choose by, a caller had nothing in any response to decide with.
-- It is about the reports that take a contour (flow, assets, returns);
-- reconciliation takes an account rather than a contour, so it says nothing
-- about reconciliation.
--
-- **A current statement, not a history**, the shape `account_retractions` uses
-- and for the same reason: presence is the whole of the statement. Withdrawal
-- deletes the row rather than leaving it standing with a withdrawn flag, and
-- the absence of a row is what "undeclared" means, exactly as it means "not
-- retracted" next door — two ways of spelling one state is how they disagree.
-- One row per owner at most, which is what the primary key on `owner` says: he
-- has one reporting perimeter or none.
--
-- **The subject is the contour identity and never a version.** An omitted
-- version already means the latest one, and adding a version replaces the
-- composition from that version onwards, so a designation pinned to a version
-- would keep reporting over the old perimeter after the owner deliberately
-- widened it — the one thing this table exists to prevent. The declaration
-- therefore stores no version, and nothing reads a version out of it.
--
-- **No actor column.** Who declared it is carried on `decision_history`,
-- joined by subject, the way every other standing decision's actor is; the
-- typed decision is written before the index row and the review omits an actor
-- rather than inventing one. See that table's own comment and
-- `iaam_store::decisions`.
--
-- **Why there is no `FOREIGN KEY` clause, and what stands in its place.** This
-- table names a contour that must exist, and it cannot say so with a foreign
-- key, because a contour is not a table here. `contour_versions` holds one row
-- per version and its only key is `(owner, contour, version)`, so a child naming
-- `(owner, contour)` has no parent key to point at — a declaration written
-- through that relationship is not left unchecked, it fails: SQLite reports
-- `foreign key mismatch` on every statement that uses it. The version-carrying
-- key it *could* point at is the one thing the paragraph above forbids. The two
-- tables that name a contour with no key at all, `snapshots` and
-- `applied_rule_versions`, are no precedent either: both name a *version* of
-- one, which this table must not. So the two triggers below make the same
-- promise the foreign key would have made: a declaration names a contour that
-- exists and is this owner's. They are not a weaker guard than the key they
-- replace, because `contour_versions` is immutable and undeletable by this
-- schema's own triggers — a contour's existence can only ever be minted, never
-- revoked, so the delete half of a foreign key's promise has nothing to guard
-- here and only the write half is left to enforce.
CREATE TABLE contour_report_defaults (
    owner       TEXT NOT NULL,
    contour     TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (owner)
) STRICT;

CREATE TRIGGER contour_report_defaults_name_a_held_contour
BEFORE INSERT ON contour_report_defaults
WHEN NOT EXISTS (
    SELECT 1 FROM contour_versions
    WHERE owner = NEW.owner AND contour = NEW.contour
)
BEGIN
    SELECT RAISE(ABORT, 'a report default must name a contour this owner holds');
END;

-- The same statement for the restatement path. A declaration is replaced rather
-- than added to, so the second way a row with a contour in it can arrive is an
-- `UPDATE`, and a trigger on `INSERT` alone would let the one route that
-- restates the declaration write a contour that does not exist. SQLite allows
-- one event per trigger, which is why this is two triggers and not one.
CREATE TRIGGER contour_report_defaults_stay_on_a_held_contour
BEFORE UPDATE ON contour_report_defaults
WHEN NOT EXISTS (
    SELECT 1 FROM contour_versions
    WHERE owner = NEW.owner AND contour = NEW.contour
)
BEGIN
    SELECT RAISE(ABORT, 'a report default must name a contour this owner holds');
END;
