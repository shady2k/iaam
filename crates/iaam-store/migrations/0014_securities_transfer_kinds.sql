-- Replace the cash interpretation of securities transfers with directional meanings.
-- The table has a closed CHECK constraint, so rebuild it before carrying the rows over.
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
        'buy', 'sell', 'dividend', 'coupon', 'commission',
        'deposit', 'withdrawal', 'transfer',
        'bond_amortisation', 'bond_redemption',
        'securities_transfer_in', 'securities_transfer_out'
    ))
) STRICT;

INSERT INTO broker_operation_kinds_new (
    broker, source_kind, kind, origin, dictionary, recorded_at
)
SELECT
    broker,
    source_kind,
    CASE source_kind
        WHEN 'OPERATION_TYPE_INPUT_SECURITIES' THEN 'securities_transfer_in'
        WHEN 'OPERATION_TYPE_OUTPUT_SECURITIES' THEN 'securities_transfer_out'
        ELSE kind
    END,
    origin,
    dictionary,
    recorded_at
FROM broker_operation_kinds;

DROP TABLE broker_operation_kinds;
ALTER TABLE broker_operation_kinds_new RENAME TO broker_operation_kinds;
