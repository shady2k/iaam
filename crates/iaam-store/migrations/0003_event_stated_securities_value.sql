-- The owner's stated securities value on an account, with no instrument
-- named (`iaam-k3gh.11`).
--
-- `event_stated_securities_value` is a family table of its own, modelled on
-- `event_valuation` rather than on any cash table: the fact moves no money
-- (spec §4.5's reconstruction list is for a value a leg already carries, and
-- this event posts no leg at all), so `amount` and `currency` are stored
-- here in full rather than left for a leg to reconstruct.
--
-- A second migration rather than a line added to `0001_schema.sql`: that
-- file is the collapse of the migrations that preceded it, applied only to a
-- database still at version 0. A database already at `user_version = 2`
-- skips `0001` and `0002` entirely, so a table added there would exist in a
-- database created after this change and in none created before it.
--
-- **`events.kind` carries a `CHECK (kind IN (...))` naming every discriminant
-- by value, and SQLite has no `ALTER TABLE ... ADD CONSTRAINT`.** The only
-- way to widen a `CHECK` is to rebuild the table: create a replacement with
-- the wider list, copy every row across unchanged, drop the original, and
-- rename the replacement into its place. Every column, every other `CHECK`
-- and the `FOREIGN KEY` below are copied verbatim from `0001_schema.sql`
-- (spec §4.1) — nothing about the row shape changes, only the one list that
-- must now also accept `'stated_securities_value'`. `crates/iaam-store/src/schema.rs`
-- runs this migration with `PRAGMA foreign_keys` off for its duration: the
-- fifteen detail and leg tables the rebuilt `events` is a parent of would
-- otherwise refuse the `DROP TABLE` below outright, on rows that are not
-- moving and not becoming invalid — see that module's own comment for why
-- toggling the pragma there, rather than in this file, is what makes the
-- toggle actually take effect.
CREATE TABLE events_v3 (
    id                      TEXT PRIMARY KEY,
    owner                   TEXT NOT NULL,
    account                 TEXT NOT NULL,
    kind                    TEXT NOT NULL,
    effective_date          TEXT NOT NULL,
    -- EffectiveOrder::source_time.
    source_time             TEXT,
    sequence                INTEGER NOT NULL,
    confidence              TEXT NOT NULL,
    relation_kind           TEXT NOT NULL,
    relation_target         TEXT,
    idempotency_key         TEXT,
    recorded_at             TEXT NOT NULL,

    -- EventDates, all optional.
    date_trade              TEXT,
    date_settled            TEXT,
    date_cash_posted        TEXT,
    date_entitlement        TEXT,
    date_paid               TEXT,
    -- TaxPeriod(i32): a calendar year, not a date.
    tax_period_override     INTEGER,

    -- Provenance.
    source                  TEXT NOT NULL,
    raw_hash                TEXT NOT NULL,
    parser_version          TEXT NOT NULL,
    source_operation_id     TEXT,
    -- The broker's own handle for the position, where the channel stated
    -- one. Evidence beside `source_operation_id`, never a custody place
    -- (iaam-xep0, T4): `Provenance::source_position_id` documents why.
    source_position_id      TEXT,
    source_category         TEXT,
    source_kind             TEXT,
    owner_category          TEXT,
    source_code             TEXT,
    -- `Provenance::description`: holds either a description or a printed
    -- counterparty, hence the wider name (iaam-b4xw).
    source_description      TEXT,
    import                  TEXT,
    import_session          TEXT,
    declared_by             TEXT,
    -- NULL | no_rule | rule | answered_minting_rule.
    rule_settlement         TEXT,
    settled_by_rule         TEXT,
    settled_by_rule_version INTEGER,
    row_document            TEXT,
    row_sheet               TEXT,
    row_number              INTEGER,
    -- `source_counterparty` (0002): the counterparty the source printed on a
    -- row, kept apart from `source_description` for the reason that
    -- migration's own comment gives.
    source_counterparty     TEXT,

    CHECK (kind IN (
        'trade', 'cash_in', 'cash_out', 'refund', 'cash_transfer',
        'own_account_movement', 'unresolved_own_account_movement',
        'income', 'fee', 'tax', 'opening_position', 'opening_cash',
        'stated_securities_value',
        'valuation', 'control_assertion', 'import_coverage_gap',
        'corporate_action', 'offer_exercise'
    )),
    CHECK (confidence IN ('known', 'estimated', 'unknown')),
    CHECK (relation_kind IN ('none', 'reversal', 'replacement')),
    CHECK ((relation_kind = 'none') = (relation_target IS NULL)),
    -- `rule_settlement` is nullable, so a two-sided equality is not enough:
    -- a CHECK that evaluates to NULL passes in SQLite. The four states are
    -- spelled out.
    CHECK (
        (rule_settlement IS NULL AND settled_by_rule IS NULL AND settled_by_rule_version IS NULL)
        OR (rule_settlement = 'no_rule' AND settled_by_rule IS NULL AND settled_by_rule_version IS NULL)
        OR (rule_settlement IN ('rule', 'answered_minting_rule')
            AND settled_by_rule IS NOT NULL AND settled_by_rule_version IS NOT NULL)
    ),
    CHECK ((row_document IS NULL) = (row_number IS NULL)),
    CHECK (row_sheet IS NULL OR row_document IS NOT NULL),
    -- `RowLocator.row` is `u64` and SQLite `INTEGER` is a signed `i64`: this
    -- schema does not promise to round-trip the whole `u64` range.
    CHECK (row_number IS NULL OR row_number >= 0),

    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)

    -- Three columns look like foreign keys and are deliberately not:
    -- `import_session`, `settled_by_rule` and `relation_target`. See
    -- `0001_schema.sql`'s own comment on this table for why.
) STRICT;

-- Every column above is in the same order `SELECT *` reads it from the
-- table being replaced, so a positional copy is exact and carries the new
-- column (`source_counterparty`, added by `0002`) along with everything
-- older.
INSERT INTO events_v3 SELECT * FROM events;

DROP TABLE events;

ALTER TABLE events_v3 RENAME TO events;

-- `DROP TABLE` drops every index defined on it, wherever in `0001_schema.sql`
-- it was declared — not only the three that sit beside the table itself, but
-- also the eight keyset indexes §6.2 adds afterwards for the journal's paged
-- reads. All eleven are declared again here, unchanged, or a page filtered
-- by account, kind, source or one of the other keyset columns would fall
-- back to a full scan the day this migration runs.
CREATE UNIQUE INDEX events_by_order ON events (owner, effective_date, sequence);

CREATE UNIQUE INDEX events_idempotency_key
    ON events (owner, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE UNIQUE INDEX events_source_operation
    ON events (owner, source, account, source_operation_id)
    WHERE source_operation_id IS NOT NULL;

CREATE INDEX events_by_account ON events (owner, account, effective_date, sequence);
CREATE INDEX events_by_kind ON events (owner, kind, effective_date, sequence);
CREATE INDEX events_by_source ON events (owner, source, effective_date, sequence);
CREATE INDEX events_by_import
    ON events (owner, import, effective_date, sequence)
    WHERE import IS NOT NULL;
CREATE INDEX events_by_import_session
    ON events (owner, import_session, effective_date, sequence)
    WHERE import_session IS NOT NULL;
CREATE INDEX events_by_settled_by_rule
    ON events (owner, settled_by_rule, effective_date, sequence)
    WHERE settled_by_rule IS NOT NULL;
CREATE INDEX events_by_source_category
    ON events (owner, source_category, effective_date, sequence)
    WHERE source_category IS NOT NULL;
CREATE INDEX events_by_relation_target
    ON events (owner, relation_target)
    WHERE relation_target IS NOT NULL;

CREATE TABLE event_stated_securities_value (
    event    TEXT PRIMARY KEY REFERENCES events (id),
    amount   INTEGER NOT NULL,
    currency TEXT NOT NULL
) STRICT;
