-- Наблюдения накопленного купонного дохода.
--
-- Отдельная таблица от price_observations: величины разной размерности
-- (процент номинала против денег), и общая таблица заставила бы каждую
-- выборку цен фильтровать по виду строки.
--
-- Расчётный НКД сюда не пишется НИКОГДА: он вывод, а не наблюдение
-- (ADR-0002).
CREATE TABLE accrued_interest_observations (
    id             INTEGER PRIMARY KEY,
    instrument_id  TEXT NOT NULL,
    board          TEXT NOT NULL,
    session        INTEGER NOT NULL,
    trade_date     TEXT NOT NULL,
    source_id      TEXT NOT NULL,
    observed_at    TEXT NOT NULL,
    per_unit       TEXT NOT NULL,
    currency       TEXT NOT NULL,
    raw_hash       TEXT NOT NULL,
    sync_run_id    TEXT NOT NULL REFERENCES sync_runs(id)
) STRICT;

CREATE INDEX accrued_interest_observations_lookup
    ON accrued_interest_observations (instrument_id, board, session, trade_date, observed_at);
