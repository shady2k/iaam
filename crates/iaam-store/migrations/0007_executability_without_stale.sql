-- 0007: устаревание — вывод политики оценки (E3.3), а не атрибут наблюдения.
-- Решение: docs/decisions/0002-polnota-ocenki-i-ispolnimost-ceny-dve-osi.md
DROP TRIGGER price_observations_are_immutable;
DROP TRIGGER price_observations_are_not_deletable;
DROP INDEX price_observations_by_series;

CREATE TABLE price_observations_new (
    instrument_id TEXT NOT NULL REFERENCES instruments(id),
    board         TEXT NOT NULL,
    session       INTEGER NOT NULL,
    trade_date    TEXT NOT NULL,
    kind          TEXT NOT NULL,
    source_id     TEXT NOT NULL,
    observed_at   TEXT NOT NULL,
    price         TEXT NOT NULL,
    currency      TEXT NOT NULL,
    executability TEXT NOT NULL,
    raw_hash      TEXT NOT NULL,
    sync_run_id   TEXT NOT NULL REFERENCES sync_runs(id),
    PRIMARY KEY (
        instrument_id, board, session, trade_date, kind, source_id, observed_at
    ),
    CHECK (executability IN ('executable', 'indicative_previous_close'))
) STRICT;

INSERT INTO price_observations_new SELECT * FROM price_observations;
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
