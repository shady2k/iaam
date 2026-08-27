-- 0008: основание котировки — атрибут наблюдения от источника (§10.2).
-- Дефект iaam-a75: без основания облигация по 98.5 при номинале 1000 ₽
-- оценивалась в 98.5 ₽ вместо 985 ₽.
--
-- Существующие строки получают 'unknown', а НЕ 'money_per_unit':
-- доказательства, что облигационных строк среди них нет, у миграции
-- не существует, а неинтерпретируемая строка честнее подставленной.
DROP TRIGGER price_observations_are_immutable;
DROP TRIGGER price_observations_are_not_deletable;
DROP INDEX price_observations_by_series;

CREATE TABLE price_observations_new (
    instrument_id   TEXT NOT NULL REFERENCES instruments(id),
    board           TEXT NOT NULL,
    session         INTEGER NOT NULL,
    trade_date      TEXT NOT NULL,
    kind            TEXT NOT NULL,
    source_id       TEXT NOT NULL,
    observed_at     TEXT NOT NULL,
    price           TEXT NOT NULL,
    currency        TEXT NOT NULL,
    -- DEFAULT намеренно НЕ задан ни у одной из двух колонок: INSERT,
    -- забывший колонку, обязан отказать, а не получить правдоподобное
    -- 'unknown'. Существующим строкам значение подставляет перенос ниже,
    -- один раз и явно.
    quotation_basis TEXT NOT NULL,
    basis_evidence  TEXT NOT NULL,
    executability   TEXT NOT NULL,
    raw_hash        TEXT NOT NULL,
    sync_run_id     TEXT NOT NULL REFERENCES sync_runs(id),
    PRIMARY KEY (
        instrument_id, board, session, trade_date, kind, source_id, observed_at
    ),
    CHECK (executability IN ('executable', 'indicative_previous_close')),
    CHECK (quotation_basis IN ('money_per_unit', 'percent_of_remaining_face', 'unknown'))
) STRICT;

INSERT INTO price_observations_new (
    instrument_id, board, session, trade_date, kind, source_id, observed_at,
    price, currency, quotation_basis, basis_evidence, executability,
    raw_hash, sync_run_id
)
SELECT
    instrument_id, board, session, trade_date, kind, source_id, observed_at,
    price, currency, 'unknown', '', executability, raw_hash, sync_run_id
FROM price_observations;

DROP TABLE price_observations;
ALTER TABLE price_observations_new RENAME TO price_observations;

CREATE INDEX price_observations_by_series
    ON price_observations (
        instrument_id, board, session, trade_date, source_id, observed_at
    );

CREATE TRIGGER price_observations_are_immutable
BEFORE UPDATE ON price_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение цены append-only: исправление — новая строка');
END;

CREATE TRIGGER price_observations_are_not_deletable
BEFORE DELETE ON price_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение цены append-only: удаление запрещено');
END;
