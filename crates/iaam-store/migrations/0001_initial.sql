-- Схема журнала фактов (§4.1, §16.1).
--
-- Событие хранится целиком в JSON, а индексируемые поля вынесены
-- в колонки. Причина: журнал неизменяем, и разложение его по таблицам
-- ничего не даёт для записи, но добавляет способ потерять поле при
-- добавлении варианта события. Round-trip JSON проверен тестом ядра.

CREATE TABLE events (
    id                  TEXT PRIMARY KEY,
    schema_version      INTEGER NOT NULL,
    owner               TEXT NOT NULL,
    account             TEXT NOT NULL,
    kind                TEXT NOT NULL,
    effective_date      TEXT NOT NULL,
    sequence            INTEGER NOT NULL,
    relation_kind       TEXT NOT NULL,
    relation_target     TEXT,
    source              TEXT NOT NULL,
    source_operation_id TEXT,
    idempotency_key     TEXT,
    raw_hash            TEXT NOT NULL,
    payload             TEXT NOT NULL,
    recorded_at         TEXT NOT NULL
) STRICT;

-- Порядок проекции: дата, затем sequence, затем идентификатор.
-- Уникальность (owner, дата, sequence) обязательна: без неё два
-- одновременных запроса получают один и тот же номер, и порядок внутри
-- дня начинает определяться случайным UUID, а не объявленной семантикой
-- effectiveOrder (§4.8).
CREATE UNIQUE INDEX events_by_order ON events (owner, effective_date, sequence);

-- Идемпотентность (§10.6). Ключи разной силы — разные индексы:
-- клиентский ключ и идентификатор операции источника не заменяют друг друга.
CREATE UNIQUE INDEX events_idempotency_key
    ON events (owner, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE UNIQUE INDEX events_source_operation
    ON events (owner, source, source_operation_id)
    WHERE source_operation_id IS NOT NULL;

-- Append-only не как договорённость, а как поведение базы (§4.8).
-- Дисциплина кода не переживает первый же скрипт починки данных.
CREATE TRIGGER events_are_immutable
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'журнал фактов append-only: исправление — новое событие');
END;

CREATE TRIGGER events_are_not_deletable
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'журнал фактов append-only: удаление запрещено');
END;

-- Справочники. Меняются, поэтому обычные таблицы без триггеров.
CREATE TABLE accounts (
    id           TEXT PRIMARY KEY,
    owner        TEXT NOT NULL,
    title        TEXT NOT NULL,
    institution  TEXT,
    created_at   TEXT NOT NULL
) STRICT;

-- Владелец входит в уникальный ключ: без этого счёт нельзя сослать
-- внешним ключом из состава контура так, чтобы чужой счёт в него
-- не попал.
CREATE UNIQUE INDEX accounts_by_owner ON accounts (owner, id);

CREATE TABLE instruments (
    id       TEXT PRIMARY KEY,
    symbol   TEXT NOT NULL,
    title    TEXT NOT NULL,
    currency TEXT NOT NULL
) STRICT;

-- Контур версионирован: состав на версии неизменяем, новая версия —
-- новая строка (§4.10). Иначе изменение состава задним числом молча
-- переписало бы историческую доходность.
CREATE TABLE contour_versions (
    owner    TEXT NOT NULL,
    contour  TEXT NOT NULL,
    version  INTEGER NOT NULL,
    title    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (owner, contour, version)
) STRICT;

CREATE TABLE contour_accounts (
    owner   TEXT NOT NULL,
    contour TEXT NOT NULL,
    version INTEGER NOT NULL,
    account TEXT NOT NULL,
    PRIMARY KEY (owner, contour, version, account),
    FOREIGN KEY (owner, contour, version)
        REFERENCES contour_versions (owner, contour, version),
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

CREATE TRIGGER contour_versions_are_immutable
BEFORE UPDATE ON contour_versions
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: заведите новую версию');
END;

CREATE TRIGGER contour_accounts_are_immutable
BEFORE UPDATE ON contour_accounts
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: заведите новую версию');
END;

-- Удаление запрещено наравне с изменением. Запрет только на UPDATE
-- ловит правку строки, но пропускает DELETE + INSERT, а это тот же
-- результат: исторический состав версии изменён, и все посчитанные
-- по ней цифры молча стали другими (§4.10).
CREATE TRIGGER contour_versions_are_not_deletable
BEFORE DELETE ON contour_versions
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: удаление запрещено');
END;

CREATE TRIGGER contour_accounts_are_not_deletable
BEFORE DELETE ON contour_accounts
BEGIN
    SELECT RAISE(ABORT, 'состав контура версионирован: удаление запрещено');
END;

-- Снимки проекций — кэш. Потеря снимка не является потерей данных:
-- он всегда восстановим полным пересчётом журнала.
CREATE TABLE snapshots (
    owner              TEXT NOT NULL,
    contour            TEXT NOT NULL,
    contour_version    INTEGER NOT NULL,
    lot_rule           INTEGER NOT NULL,
    projection_version INTEGER NOT NULL,
    through_date       TEXT,
    through_sequence   INTEGER,
    fingerprint        TEXT NOT NULL,
    body               BLOB NOT NULL,
    created_at         TEXT NOT NULL,
    PRIMARY KEY (owner, contour, contour_version, lot_rule)
) STRICT;

-- Агентские токены: хранится хеш, не сам токен (§14).
CREATE TABLE api_tokens (
    id          TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    label       TEXT NOT NULL,
    token_hash  TEXT NOT NULL UNIQUE,
    scope       TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    revoked_at  TEXT
) STRICT;

CREATE TABLE token_usage (
    token   TEXT NOT NULL,
    used_at TEXT NOT NULL,
    route   TEXT NOT NULL,
    outcome TEXT NOT NULL
) STRICT;

CREATE INDEX token_usage_by_token ON token_usage (token, used_at);