-- Full redemption may assert posted principal without inventing a per-unit amount.
CREATE TABLE event_corporate_action_new (
    event         TEXT PRIMARY KEY REFERENCES events (id),
    action_kind   TEXT NOT NULL,
    -- partial_redemption, redemption.
    instrument    TEXT REFERENCES instruments (id),
    -- conversion.
    predecessor   TEXT REFERENCES instruments (id),
    successor     TEXT REFERENCES instruments (id),
    custody       TEXT REFERENCES custody_places (id),
    -- partial_redemption, redemption.
    quantity      TEXT,
    -- conversion.
    quantity_in   TEXT,
    quantity_out  TEXT,
    ratio         TEXT,
    -- partial_redemption, redemption: PerUnitAmount.
    principal_returned_per_unit_value    TEXT,
    principal_returned_per_unit_currency TEXT,
    -- redemption only: exact source-posted principal.
    posted_principal_minor INTEGER,
    posted_principal_currency TEXT,
    -- conversion only.
    fractional      TEXT,
    basis_transfer  TEXT,
    effective_date  TEXT NOT NULL,
    record_date     TEXT,
    grounds         TEXT,
    -- BasisAllocation, partial_redemption only (spec §4.3): four columns of
    -- meaning, six of storage, because `Known` carries `share` and an
    -- `AllocationEvidence { inputs_hash, knowledge_as_of, algorithm_version }`
    -- — dropping the evidence loses the audit of how the share was computed.
    allocation_kind        TEXT,
    allocation_gap         TEXT,
    allocation_share       TEXT,
    allocation_inputs_hash TEXT,
    allocation_known_as_of TEXT,
    allocation_algorithm   INTEGER,

    CHECK (action_kind IN ('partial_redemption', 'redemption', 'conversion')),
    CHECK (action_kind = 'redemption' OR custody IS NOT NULL),
    CHECK ((principal_returned_per_unit_value IS NULL) = (principal_returned_per_unit_currency IS NULL)),
    CHECK ((posted_principal_minor IS NULL) = (posted_principal_currency IS NULL)),
    CHECK (action_kind = 'redemption' OR posted_principal_minor IS NULL),
    CHECK (fractional IS NULL OR fractional IN ('cash_compensated', 'rounded_down', 'not_applicable')),
    CHECK (basis_transfer IS NULL OR basis_transfer IN ('carry_over', 'restart')),
    CHECK (allocation_kind IS NULL OR allocation_kind IN ('unknown', 'known')),
    CHECK (allocation_gap IS NULL OR allocation_gap IN (
        'not_computed', 'schedule_missing', 'schedule_not_validated', 'no_repayment_on_date',
        'amount_mismatch', 'currency_mismatch', 'ambiguous_same_date_repayments', 'invalid_prefix'
    )),
    CHECK ((allocation_kind = 'unknown') = (allocation_gap IS NOT NULL)),
    CHECK ((allocation_kind = 'known')   = (allocation_share IS NOT NULL)),
    CHECK ((allocation_kind = 'known')   = (allocation_inputs_hash IS NOT NULL)),
    CHECK ((allocation_kind = 'known')   = (allocation_known_as_of IS NOT NULL)),
    CHECK ((allocation_kind = 'known')   = (allocation_algorithm IS NOT NULL)),
    CHECK (
        CASE action_kind
            WHEN 'partial_redemption' THEN
                instrument IS NOT NULL AND quantity IS NOT NULL
                AND principal_returned_per_unit_value IS NOT NULL
                AND allocation_kind IS NOT NULL
                AND predecessor IS NULL AND successor IS NULL
                AND quantity_in IS NULL AND quantity_out IS NULL AND ratio IS NULL
                AND fractional IS NULL AND basis_transfer IS NULL
            WHEN 'redemption' THEN
                instrument IS NOT NULL AND quantity IS NOT NULL
                AND ((principal_returned_per_unit_value IS NOT NULL) != (posted_principal_minor IS NOT NULL))
                AND allocation_kind IS NULL
                AND predecessor IS NULL AND successor IS NULL
                AND quantity_in IS NULL AND quantity_out IS NULL AND ratio IS NULL
                AND fractional IS NULL AND basis_transfer IS NULL
            WHEN 'conversion' THEN
                predecessor IS NOT NULL AND successor IS NOT NULL
                AND ratio IS NOT NULL AND quantity_in IS NOT NULL AND quantity_out IS NOT NULL
                AND fractional IS NOT NULL AND basis_transfer IS NOT NULL
                AND allocation_kind IS NULL
                AND instrument IS NULL AND quantity IS NULL
                AND principal_returned_per_unit_value IS NULL
        END
    )
) STRICT;

INSERT INTO event_corporate_action_new (
    event, action_kind, instrument, predecessor, successor, custody, quantity, quantity_in, quantity_out, ratio, principal_returned_per_unit_value, principal_returned_per_unit_currency, fractional, basis_transfer, effective_date, record_date, grounds, allocation_kind, allocation_gap, allocation_share, allocation_inputs_hash, allocation_known_as_of, allocation_algorithm
) SELECT event, action_kind, instrument, predecessor, successor, custody, quantity, quantity_in, quantity_out, ratio, principal_returned_per_unit_value, principal_returned_per_unit_currency, fractional, basis_transfer, effective_date, record_date, grounds, allocation_kind, allocation_gap, allocation_share, allocation_inputs_hash, allocation_known_as_of, allocation_algorithm FROM event_corporate_action;
DROP TABLE event_corporate_action;
ALTER TABLE event_corporate_action_new RENAME TO event_corporate_action;
