-- Widen income kinds without changing existing facts or classification decisions.
CREATE TABLE event_income_new (
    event       TEXT PRIMARY KEY REFERENCES events (id),
    instrument  TEXT REFERENCES instruments (id),
    income_kind TEXT,
    CHECK (income_kind IS NULL OR income_kind IN ('coupon', 'dividend', 'deposit_interest', 'securities_lending_income'))
) STRICT;

INSERT INTO event_income_new SELECT * FROM event_income;
DROP TABLE event_income;
ALTER TABLE event_income_new RENAME TO event_income;

CREATE TABLE classification_rules_new (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    -- The owner's decision number in sequence.
    version    INTEGER NOT NULL,

    -- RuleMatcher: seven independent optional conditions.
    counterparty_account TEXT,
    description_contains TEXT,
    source_kind          TEXT,
    source_category      TEXT,
    owner_category       TEXT,
    source_code          TEXT,
    movement             TEXT,

    -- Classification: the tagged outcome.
    outcome_kind TEXT NOT NULL,
    to_account   TEXT REFERENCES accounts (id),
    fee_origin   TEXT,
    income_kind  TEXT,

    created_at TEXT NOT NULL,
    retired_at TEXT,
    replaces   TEXT REFERENCES classification_rules (id),

    -- A condition that asks about nothing matches nothing: an "everything"
    -- rule can only be created by mistake.
    CHECK (counterparty_account IS NOT NULL OR description_contains IS NOT NULL
        OR source_kind IS NOT NULL OR source_category IS NOT NULL
        OR owner_category IS NOT NULL OR source_code IS NOT NULL
        OR movement IS NOT NULL),
    CHECK (movement IS NULL OR movement IN ('in', 'out')),
    CHECK (outcome_kind IN (
        'internal_transfer', 'external_flow', 'fee', 'refund', 'income',
        'own_account_movement'
    )),
    CHECK ((outcome_kind = 'internal_transfer') = (to_account IS NOT NULL)),
    CHECK ((outcome_kind = 'fee') = (fee_origin IS NOT NULL)),
    -- `Income.kind` is itself an `Option<IncomeKind>`: a rule may assert
    -- income with no kind stated, so income_kind stays NULL even when
    -- outcome_kind = 'income'.
    CHECK (income_kind IS NULL OR outcome_kind = 'income'),
    CHECK (fee_origin IS NULL OR fee_origin IN (
        'brokerage', 'depositary', 'account_maintenance', 'margin_interest', 'other'
    )),
    CHECK (income_kind IS NULL OR income_kind IN ('coupon', 'dividend', 'deposit_interest', 'securities_lending_income'))
) STRICT;

INSERT INTO classification_rules_new SELECT * FROM classification_rules;
DROP TABLE classification_rules;
ALTER TABLE classification_rules_new RENAME TO classification_rules;

CREATE UNIQUE INDEX classification_rules_by_version
    ON classification_rules (owner, version);

CREATE INDEX classification_rules_by_owner ON classification_rules (owner, retired_at);
