-- Справочник инструментов, псевдонимы и места хранения (E3.1).
--
-- Спецификация: .internal/specs/2026-08-25-e3-1-instrument-reference-design.md

-- Пересоздание, а не ALTER: добавить NOT NULL-колонку в существующую
-- STRICT-таблицу с данными SQLite не умеет.
CREATE TABLE instruments_new (
    id                    TEXT PRIMARY KEY,
    -- NULL = род не установлен. Варианта `unknown` нет намеренно:
    -- §4.9 запрещает unknown как нулевое значение, а Option<T>
    -- заставляет обработать отсутствие.
    kind                  TEXT,
    -- Отображаемый символ, а НЕ идентичность: идентичность живёт
    -- в instrument_aliases, потому что ISIN меняется (§4.7).
    symbol                TEXT NOT NULL,
    title                 TEXT NOT NULL,
    denomination_currency TEXT NOT NULL,
    settlement_currency   TEXT NOT NULL,
    quote_currency        TEXT NOT NULL,
    lineage_parent        TEXT REFERENCES instruments_new(id),
    lineage_reason        TEXT,
    created_at            TEXT NOT NULL,
    -- Происхождение без причины и причина без происхождения одинаково
    -- бессмысленны, поэтому пара обязана быть заполнена целиком.
    CHECK ((lineage_parent IS NULL) = (lineage_reason IS NULL))
) STRICT;

-- Перенос: три роли валюты у уже заведённой бумаги совпадают,
-- род не известен и не выдумывается.
INSERT INTO instruments_new
    (id, kind, symbol, title,
     denomination_currency, settlement_currency, quote_currency,
     lineage_parent, lineage_reason, created_at)
SELECT id, NULL, symbol, title,
       currency, currency, currency,
       NULL, NULL, '1970-01-01T00:00:00Z'
FROM instruments;

DROP TABLE instruments;
ALTER TABLE instruments_new RENAME TO instruments;

-- Внешние коды. Каждый со своим интервалом действия: резолвинг идёт
-- на дату документа, потому что ISIN меняется корпоративным
-- действием isin_change (§4.7), а отчёт за прошлый год приходит
-- со старым кодом.
CREATE TABLE instrument_aliases (
    namespace  TEXT NOT NULL,
    value      TEXT NOT NULL,
    instrument TEXT NOT NULL REFERENCES instruments(id),
    valid_from TEXT NOT NULL,
    -- NULL = открытый интервал.
    valid_to   TEXT,
    -- Псевдоним — утверждение о мире, как и цена, и приходит
    -- с provenance (§4.4). Строка «откуда-то узнали» не позволила бы
    -- отозвать псевдонимы испорченного документа.
    source     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (namespace, value, valid_from),
    CHECK (valid_to IS NULL OR valid_to > valid_from)
) STRICT;

CREATE INDEX instrument_aliases_by_instrument
    ON instrument_aliases (instrument);

-- Непересечение интервалов держится базой, а не дисциплиной кода:
-- дисциплина не переживает первый же скрипт починки данных, а
-- пересечение делает резолвинг неоднозначным (§15.2).
-- Полуинтервал [valid_from, valid_to): смежные записи стыкуются,
-- пересекающиеся — нет.
CREATE TRIGGER instrument_aliases_do_not_overlap
BEFORE INSERT ON instrument_aliases
BEGIN
    SELECT RAISE(ABORT, 'интервалы псевдонима пересекаются: резолвинг стал бы неоднозначным')
    WHERE EXISTS (
        SELECT 1 FROM instrument_aliases existing
        WHERE existing.namespace = NEW.namespace
          AND existing.value = NEW.value
          AND (NEW.valid_to IS NULL OR existing.valid_from < NEW.valid_to)
          AND (existing.valid_to IS NULL OR NEW.valid_from < existing.valid_to)
    );
END;

CREATE TRIGGER instrument_aliases_do_not_overlap_on_update
BEFORE UPDATE ON instrument_aliases
BEGIN
    SELECT RAISE(ABORT, 'интервалы псевдонима пересекаются: резолвинг стал бы неоднозначным')
    WHERE EXISTS (
        SELECT 1 FROM instrument_aliases existing
        WHERE existing.namespace = NEW.namespace
          AND existing.value = NEW.value
          AND existing.valid_from <> OLD.valid_from
          AND (NEW.valid_to IS NULL OR existing.valid_from < NEW.valid_to)
          AND (existing.valid_to IS NULL OR NEW.valid_from < existing.valid_to)
    );
END;

-- Место хранения бумаг (§4.5). CustodyId объявлен в
-- crates/iaam-core/src/ids.rs и требуется Leg::security, но таблицы
-- под него до сих пор не было.
CREATE TABLE custody_places (
    id          TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    title       TEXT NOT NULL,
    institution TEXT,
    created_at  TEXT NOT NULL
) STRICT;

-- Владелец в уникальном ключе — как у accounts: иначе чужое место
-- хранения подставится в ногу сделки (§14).
CREATE UNIQUE INDEX custody_places_by_owner ON custody_places (owner, id);
