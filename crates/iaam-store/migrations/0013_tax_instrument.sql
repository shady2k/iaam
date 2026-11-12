-- Tax may identify the instrument whose income incurred it. Older tax facts
-- keep their unknown instrument without rewriting journal or classification history.
ALTER TABLE event_tax ADD COLUMN instrument TEXT REFERENCES instruments (id);

-- Keep contract mappings and owner overrides while admitting the tax kind.
CREATE TABLE broker_operation_kinds_new (
    broker      TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    kind        TEXT NOT NULL,
    origin      TEXT NOT NULL,
    dictionary  TEXT,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (broker, source_kind),
    CHECK (origin IN ('contract', 'owner')),
    CHECK (kind IN (
        'buy', 'sell', 'dividend', 'coupon', 'commission', 'tax',
        'deposit', 'withdrawal', 'transfer',
        'bond_amortisation', 'bond_redemption',
        'securities_transfer_in', 'securities_transfer_out'
    ))
) STRICT;

INSERT INTO broker_operation_kinds_new
    (broker, source_kind, kind, origin, dictionary, recorded_at)
SELECT broker, source_kind, kind, origin, dictionary, recorded_at
FROM broker_operation_kinds;
DROP TABLE broker_operation_kinds;
ALTER TABLE broker_operation_kinds_new RENAME TO broker_operation_kinds;
