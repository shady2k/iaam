-- Наблюдения рынка (E3.2): append-only и две оси времени.
--
-- `trade_date` — день, к которому относится значение, а `observed_at` —
-- момент, когда система его узнала. Исправление источника добавляется новой
-- строкой: наблюдение нельзя переписать или удалить задним числом.

-- Долговечный запуск синхронизации. Серия входит в lease: отказ одной серии
-- не должен блокировать остальные.
CREATE TABLE sync_runs (
    id              TEXT PRIMARY KEY,
    source_id       TEXT NOT NULL,
    dataset         TEXT NOT NULL,
    series_key      TEXT NOT NULL,
    status          TEXT NOT NULL
                    CHECK (status IN ('running', 'succeeded', 'partial', 'failed')),
    requested_from  TEXT NOT NULL,
    requested_to    TEXT NOT NULL,
    covered_from    TEXT,
    covered_to      TEXT,
    pages           INTEGER NOT NULL DEFAULT 0 CHECK (pages >= 0),
    rows            INTEGER NOT NULL DEFAULT 0 CHECK (rows >= 0),
    page_errors     TEXT NOT NULL DEFAULT '[]',
    rate_limit_hits INTEGER NOT NULL DEFAULT 0 CHECK (rate_limit_hits >= 0),
    raw_hash        TEXT,
    lease_token     TEXT,
    lease_expires_at TEXT,
    started_at      TEXT NOT NULL,
    finished_at     TEXT,
    CHECK (requested_to >= requested_from),
    CHECK ((covered_from IS NULL) = (covered_to IS NULL)),
    CHECK (covered_to IS NULL OR covered_to >= covered_from)
) STRICT;

-- У одной единицы (источник, набор, серия) в каждый момент только один
-- активный запуск. Завершённые запуски остаются историей и не конфликтуют.
CREATE UNIQUE INDEX sync_runs_active_lease
    ON sync_runs (source_id, dataset, series_key)
    WHERE status = 'running';

CREATE INDEX sync_runs_by_series
    ON sync_runs (source_id, dataset, series_key, started_at);

-- Цена: площадка и сессия входят в идентичность ряда. Все поля составного
-- ключа обязательны, поэтому здесь именно PRIMARY KEY, а не nullable-индекс.
CREATE TABLE price_observations (
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
    CHECK (executability IN ('executable', 'indicative_previous_close', 'stale'))
) STRICT;

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

-- Курсы ЦБ: направление пары и номинал — часть самого наблюдения, а не
-- вычисляемое оформление числа.
CREATE TABLE fx_observations (
    from_code   TEXT NOT NULL,
    to_code     TEXT NOT NULL,
    trade_date  TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    nominal     INTEGER NOT NULL CHECK (nominal > 0),
    value       TEXT NOT NULL,
    unit_rate   TEXT NOT NULL,
    raw_hash    TEXT NOT NULL,
    sync_run_id TEXT NOT NULL REFERENCES sync_runs(id),
    PRIMARY KEY (from_code, to_code, trade_date, source_id, observed_at)
) STRICT;

CREATE INDEX fx_observations_by_series
    ON fx_observations (from_code, to_code, trade_date, source_id, observed_at);

CREATE TRIGGER fx_observations_are_immutable
BEFORE UPDATE ON fx_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение курса append-only: исправление — новая строка');
END;

CREATE TRIGGER fx_observations_are_not_deletable
BEFORE DELETE ON fx_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение курса append-only: удаление запрещено');
END;

-- Ключевая ставка ЦБ — дневные наблюдения; интервалы выводятся при чтении.
CREATE TABLE key_rate_observations (
    trade_date  TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    rate        TEXT NOT NULL,
    raw_hash    TEXT NOT NULL,
    sync_run_id TEXT NOT NULL REFERENCES sync_runs(id),
    PRIMARY KEY (trade_date, source_id, observed_at)
) STRICT;

CREATE INDEX key_rate_observations_by_date
    ON key_rate_observations (trade_date, source_id, observed_at);

CREATE TRIGGER key_rate_observations_are_immutable
BEFORE UPDATE ON key_rate_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение ставки append-only: исправление — новая строка');
END;

CREATE TRIGGER key_rate_observations_are_not_deletable
BEFORE DELETE ON key_rate_observations
BEGIN
    SELECT RAISE(ABORT, 'наблюдение ставки append-only: удаление запрещено');
END;

-- Граница полноты хранится отдельно для каждой (источник, набор, серия).
-- NULL означает, что полной границы ещё нет; nullable-колонка намеренно не
-- входит в составной ключ.
CREATE TABLE series_completeness (
    source_id            TEXT NOT NULL,
    dataset              TEXT NOT NULL,
    series_key           TEXT NOT NULL,
    complete_through     TEXT,
    updated_at           TEXT NOT NULL,
    last_successful_run  TEXT REFERENCES sync_runs(id),
    PRIMARY KEY (source_id, dataset, series_key)
) STRICT;
