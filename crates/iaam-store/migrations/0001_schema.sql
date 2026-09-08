-- The whole schema, collapsed from 31 migrations into one (iaam-05gi).
--
-- The database was greenfield when this collapse happened: dev.db carried
-- user_version = 22, thirty-nine tables, and not one row in any of them. So
-- nothing here migrates existing data — every table is created in its final
-- shape directly, and the comments below explain that shape rather than the
-- sequence of ALTERs that used to build it up.
--
-- Reference-data, contour-version, source-document and market-observation
-- tables and their triggers are carried over unchanged from the migrations
-- being deleted: .internal/specs/2026-09-08-a-relational-journal-design.md
-- §3 puts those subsystems out of scope. Their original comments (Russian
-- where the source migration wrote them in Russian) are kept as they are.
--
-- The journal (`events` onward), the two rule vocabularies
-- (`classification_rules`, `category_rules`) and the category-assignment
-- projection (`event_category_assignments`) are new: they replace the
-- JSON-document journal per the design's §4. Only two triggers die in the
-- collapse — `events_are_immutable` and `events_are_not_deletable` — because
-- append-only becomes a property of the store's API rather than of the
-- database (§D6). Every other trigger below survives.

-- =============================================================================
-- Accounts, instruments, custody places (0001_initial.sql, 0005_instrument_reference.sql,
-- 0020_account_external_identity.sql, 0021_account_negative_balance_expectation.sql)
-- =============================================================================

-- Справочники. Меняются, поэтому обычные таблицы без триггеров.
CREATE TABLE accounts (
    id           TEXT PRIMARY KEY,
    owner        TEXT NOT NULL,
    title        TEXT NOT NULL,
    institution  TEXT,
    created_at   TEXT NOT NULL,
    -- The client's own label for the source. It scopes provider_account_id
    -- below: without it, two sources that both print short sequential
    -- identifiers would collide on values neither of them controls.
    provider     TEXT,
    -- What the source prints for this account. Opaque to iaam: it is not
    -- parsed, not shape-checked, not validated against a register, and never
    -- rendered anywhere a title belongs.
    provider_account_id TEXT,
    -- The owner's statement about what kind of cash this is: deposit,
    -- savings, card_account or wallet. NULL is "not stated" and is never
    -- filled by a guess. No CHECK constraint: the set of codes is the Rust
    -- enum, and a second copy of it in SQL would be a second truth to keep
    -- in step.
    cash_class   TEXT,
    -- What the owner expects a negative balance on this account to mean.
    -- An expectation, not a rule: it produces a warning about a probable
    -- error, and nothing refuses or suppresses anything on the strength of
    -- it. NULL is "he has not said". No CHECK, for `cash_class`'s reason.
    negative_balance_expectation TEXT
) STRICT;

-- Владелец входит в уникальный ключ: без этого счёт нельзя сослать
-- внешним ключом из состава контура так, чтобы чужой счёт в него
-- не попал.
CREATE UNIQUE INDEX accounts_by_owner ON accounts (owner, id);

-- `(owner, provider, provider_account_id)` is unique, and the partial index
-- says out loud what SQLite's treatment of NULL would say silently: the
-- constraint binds only rows that actually carry an identity.
CREATE UNIQUE INDEX accounts_by_external_identity
    ON accounts (owner, provider, provider_account_id)
    WHERE provider IS NOT NULL AND provider_account_id IS NOT NULL;

-- Further identifiers for one account, each valid over an interval. Two
-- cards over one underlying account are one account with two aliases, so its
-- balance is counted once.
CREATE TABLE account_aliases (
    owner      TEXT NOT NULL,
    account    TEXT NOT NULL,
    value      TEXT NOT NULL,
    valid_from TEXT NOT NULL,
    -- NULL is an open-ended interval, not an unknown end.
    valid_to   TEXT,
    PRIMARY KEY (owner, account, value, valid_from),
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

-- Пересоздание, а не ALTER: добавить NOT NULL-колонку в существующую
-- STRICT-таблицу с данными SQLite не умеет. Final shape after 0005's rebuild.
CREATE TABLE instruments (
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
    lineage_parent        TEXT REFERENCES instruments (id),
    lineage_reason        TEXT,
    created_at            TEXT NOT NULL,
    CHECK ((lineage_parent IS NULL) = (lineage_reason IS NULL))
) STRICT;

-- Внешние коды. Каждый со своим интервалом действия: резолвинг идёт
-- на дату документа, потому что ISIN меняется корпоративным
-- действием isin_change (§4.7), а отчёт за прошлый год приходит
-- со старым кодом.
CREATE TABLE instrument_aliases (
    namespace  TEXT NOT NULL,
    value      TEXT NOT NULL,
    instrument TEXT NOT NULL REFERENCES instruments (id),
    valid_from TEXT NOT NULL,
    valid_to   TEXT,
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

-- Место хранения бумаг (§4.5).
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

-- =============================================================================
-- Contours and snapshots (0001_initial.sql)
-- =============================================================================

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

-- The owner's statement that an account sits outside every contour on
-- purpose. Membership *inside* a contour is not stored here: it is already a
-- fact of `contour_accounts`. What no contour can hold is the opposite
-- statement, so it is recorded once per owner and account.
CREATE TABLE account_scope_exclusions (
    owner       TEXT NOT NULL,
    account     TEXT NOT NULL,
    reason      TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (owner, account),
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

-- The owner's statement about which of his accounts money moves between
-- (0018_account_transfer_partners.sql). Two tables rather than one nullable
-- column: "money moves between this account and none of my others" is a real
-- answer and must be storable, and in a STRICT table a nullable column
-- inside the primary key is not an option.
CREATE TABLE account_transfer_statements (
    owner       TEXT NOT NULL,
    account     TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (owner, account),
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

CREATE TABLE account_transfer_partners (
    owner   TEXT NOT NULL,
    account TEXT NOT NULL,
    partner TEXT NOT NULL,
    PRIMARY KEY (owner, account, partner),
    FOREIGN KEY (owner, account) REFERENCES account_transfer_statements (owner, account)
        ON DELETE CASCADE,
    FOREIGN KEY (owner, partner) REFERENCES accounts (owner, id)
) STRICT;

-- The owner's statement that one of his products ceased to exist
-- (0025_account_retirements.sql, iaam-gua5). An APPEND-ONLY HISTORY: `revision`
-- is a per-owner monotone coordinate, and the statements in force at revision
-- R are, per account, the row with the greatest revision not above R.
CREATE TABLE account_retirements (
    owner        TEXT NOT NULL,
    revision     INTEGER NOT NULL,
    account      TEXT NOT NULL,
    -- The date in the owner's own history that the product ceased on, or
    -- NULL where this row withdraws the statement before it.
    effective_on TEXT,
    recorded_at  TEXT NOT NULL,
    PRIMARY KEY (owner, revision),
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

CREATE INDEX account_retirements_by_account
    ON account_retirements (owner, account, revision);

CREATE TRIGGER account_retirements_are_immutable
BEFORE UPDATE ON account_retirements
BEGIN
    SELECT RAISE(ABORT, 'a retirement is a revision: record a further one');
END;

CREATE TRIGGER account_retirements_are_not_deletable
BEFORE DELETE ON account_retirements
BEGIN
    SELECT RAISE(ABORT, 'a retirement is a revision: withdraw it with a further one');
END;

-- =============================================================================
-- Tokens (0001_initial.sql)
-- =============================================================================

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

-- =============================================================================
-- Source documents and raw rows (0002_sources_and_rules.sql)
-- =============================================================================

-- Сырьё источников (§10.1). Сырьё хранится, потому что версия парсера
-- пишется в provenance ради повторного разбора: разбор без сырья повторить
-- нельзя, и исправленный парсер оказался бы бесполезен для уже загруженного.
CREATE TABLE source_documents (
    id             TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    broker         TEXT NOT NULL,
    format         TEXT NOT NULL,
    parser_version TEXT NOT NULL,
    document_hash  TEXT NOT NULL,
    uploaded_at    TEXT NOT NULL,
    body           BLOB NOT NULL
) STRICT;

-- Тот же файл того же владельца — один документ. Разные владельцы
-- могут загрузить одинаковый файл: это разные факты о разных портфелях.
CREATE UNIQUE INDEX source_documents_by_hash ON source_documents (owner, document_hash);

CREATE TABLE raw_rows (
    document TEXT NOT NULL,
    sheet    TEXT,
    row      INTEGER NOT NULL,
    payload  TEXT NOT NULL,
    status   TEXT NOT NULL,
    FOREIGN KEY (document) REFERENCES source_documents (id)
) STRICT;

-- Локатор уникален, но первичным ключом быть не может: в STRICT-таблице
-- колонки первичного ключа неявно NOT NULL, а у CSV листа нет.
CREATE UNIQUE INDEX raw_rows_by_locator
    ON raw_rows (document, ifnull(sheet, ''), row);

-- Сырьё неизменяемо наравне с журналом: «поправить строку в исходнике»
-- означает переписать факт задним числом. Разбор повторяется, сырьё —
-- никогда.
CREATE TRIGGER source_documents_are_immutable
BEFORE UPDATE ON source_documents
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неизменяемо: загрузите новый документ');
END;

CREATE TRIGGER raw_rows_are_immutable
BEFORE UPDATE ON raw_rows
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неизменяемо');
END;

CREATE TRIGGER source_documents_are_not_deletable
BEFORE DELETE ON source_documents
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неудаляемо: provenance перестанет разрешаться');
END;

CREATE TRIGGER raw_rows_are_not_deletable
BEFORE DELETE ON raw_rows
BEGIN
    SELECT RAISE(ABORT, 'сырьё источника неудаляемо');
END;

-- The account names a reading of a document could not place
-- (0026_document_unresolved_accounts.sql, iaam-x9ls).
CREATE TABLE document_unresolved_accounts (
    owner          TEXT NOT NULL,
    document_hash  TEXT NOT NULL,
    printed        TEXT NOT NULL,
    ordinal        INTEGER NOT NULL,
    records        INTEGER NOT NULL,
    import_session TEXT NOT NULL,
    recorded_at    TEXT NOT NULL,
    PRIMARY KEY (owner, document_hash, printed),
    FOREIGN KEY (import_session) REFERENCES import_sessions (id)
) STRICT;

CREATE INDEX document_unresolved_accounts_by_owner
    ON document_unresolved_accounts (owner, document_hash, ordinal);

-- The names a document printed that the owner has said are not his accounts
-- (0027_declined_account_names.sql, iaam-mk1n). NO FOREIGN KEY to a document
-- or a session, deliberately: the statement outlives any particular reading.
CREATE TABLE declined_account_names (
    owner       TEXT NOT NULL,
    printed     TEXT NOT NULL,
    reason      TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (owner, printed)
) STRICT;

-- The content each source profile version names (0028_source_profile_versions.sql,
-- iaam-mr25). AN INSTANCE FACT, deliberately not an owner's: the catalogue is
-- a property of the deployment. Written once and never updated.
CREATE TABLE source_profile_versions (
    id              TEXT NOT NULL,
    version         INTEGER NOT NULL,
    digest          TEXT NOT NULL,
    first_loaded_at TEXT NOT NULL,
    PRIMARY KEY (id, version)
) STRICT;

-- Audit trail for reversible standing decisions (0031_decision_history.sql).
-- NULL is deliberate for rows written before attribution existed: it means
-- "not recorded", never "by nobody".
CREATE TABLE decision_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    owner       TEXT NOT NULL,
    declared_by TEXT,
    operation   TEXT NOT NULL,
    subject     TEXT NOT NULL,
    decision    TEXT NOT NULL,
    undo        TEXT NOT NULL,
    recorded_at TEXT NOT NULL
) STRICT;

CREATE INDEX decision_history_by_owner_time
    ON decision_history (owner, recorded_at, id);

-- =============================================================================
-- Broker access (0003_broker_access.sql, 0004_broker_environment.sql)
-- =============================================================================

-- Доступ к брокерскому каналу (§14). Токен лежит только шифротекстом: ключ
-- живёт вне базы, и утечка файла базы не даёт доступа к брокерскому счёту.
CREATE TABLE broker_access (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    broker     TEXT NOT NULL,
    scope      TEXT NOT NULL,
    nonce      BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    -- У песочницы и боя РАЗНЫЕ токены, поэтому среда — часть уникальности,
    -- а не только область прав. Толкуется в iaam-broker, не здесь.
    environment TEXT NOT NULL DEFAULT 'prod'
) STRICT;

-- Один действующий доступ на тройку владелец+брокер+среда; второй означал
-- бы, что неизвестно, каким из них система ходит в эту среду.
CREATE UNIQUE INDEX broker_access_active
    ON broker_access (owner, broker, environment)
    WHERE revoked_at IS NULL;

-- =============================================================================
-- Market observations (0006_market_observations.sql, 0007_executability_without_stale.sql,
-- 0008_quotation_basis.sql)
-- =============================================================================

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

-- Цена: площадка и сессия входят в идентичность ряда. Final shape after
-- 0007's rebuild (executability drops 'stale') and 0008's rebuild
-- (quotation_basis and basis_evidence added; existing rows would have gotten
-- 'unknown', but there are no existing rows in a greenfield database).
CREATE TABLE price_observations (
    instrument_id   TEXT NOT NULL REFERENCES instruments (id),
    board           TEXT NOT NULL,
    session         INTEGER NOT NULL,
    trade_date      TEXT NOT NULL,
    kind            TEXT NOT NULL,
    source_id       TEXT NOT NULL,
    observed_at     TEXT NOT NULL,
    price           TEXT NOT NULL,
    currency        TEXT NOT NULL,
    quotation_basis TEXT NOT NULL,
    basis_evidence  TEXT NOT NULL,
    executability   TEXT NOT NULL,
    raw_hash        TEXT NOT NULL,
    sync_run_id     TEXT NOT NULL REFERENCES sync_runs (id),
    PRIMARY KEY (
        instrument_id, board, session, trade_date, kind, source_id, observed_at
    ),
    CHECK (executability IN ('executable', 'indicative_previous_close')),
    CHECK (quotation_basis IN ('money_per_unit', 'percent_of_remaining_face', 'unknown'))
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
    sync_run_id TEXT NOT NULL REFERENCES sync_runs (id),
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
    sync_run_id TEXT NOT NULL REFERENCES sync_runs (id),
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
CREATE TABLE series_completeness (
    source_id            TEXT NOT NULL,
    dataset              TEXT NOT NULL,
    series_key           TEXT NOT NULL,
    complete_through     TEXT,
    updated_at           TEXT NOT NULL,
    last_successful_run  TEXT REFERENCES sync_runs (id),
    PRIMARY KEY (source_id, dataset, series_key)
) STRICT;

-- Наблюдения накопленного купонного дохода (0011_accrued_interest.sql).
-- Расчётный НКД сюда не пишется НИКОГДА: он вывод, а не наблюдение (ADR-0002).
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
    sync_run_id    TEXT NOT NULL REFERENCES sync_runs (id)
) STRICT;

CREATE INDEX accrued_interest_observations_lookup
    ON accrued_interest_observations (instrument_id, board, session, trade_date, observed_at);

-- =============================================================================
-- Broker operation kinds (0009_broker_operation_kinds.sql, 0014_securities_transfer_kinds.sql)
-- =============================================================================

-- Словарь видов операций канала (эпик iaam-d8b.2.2). Владельца в ключе НЕТ
-- намеренно: словарь — факт о брокерском API, а не о владельце. Final shape
-- after 0014's rebuild, which added the two securities-transfer kinds.
CREATE TABLE broker_operation_kinds (
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

-- =============================================================================
-- Bond schedules (0010_bond_schedule.sql)
-- =============================================================================

-- График выплат облигаций снимками (спека E3.4 §2.2). Единица наблюдения —
-- снимок графика выпуска ЦЕЛИКОМ, а не строка.
CREATE TABLE schedule_snapshots (
    id            TEXT PRIMARY KEY,
    instrument_id TEXT NOT NULL REFERENCES instruments (id),
    source_id     TEXT NOT NULL,
    observed_at   TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    recorded_at   TEXT NOT NULL,
    UNIQUE (instrument_id, source_id, observed_at)
) STRICT;

CREATE INDEX schedule_snapshots_by_series
    ON schedule_snapshots (instrument_id, source_id, observed_at);

CREATE TRIGGER schedule_snapshots_are_immutable
BEFORE UPDATE ON schedule_snapshots
BEGIN
    SELECT RAISE(ABORT, 'снимок графика append-only: исправление — новый снимок');
END;

CREATE TRIGGER schedule_snapshots_are_not_deletable
BEFORE DELETE ON schedule_snapshots
BEGIN
    SELECT RAISE(ABORT, 'снимок графика append-only: удаление запрещено');
END;

-- Строки графика. Своей оси знания у них НЕТ намеренно: она принадлежит
-- снимку.
CREATE TABLE schedule_coupon_periods (
    snapshot_id     TEXT NOT NULL REFERENCES schedule_snapshots (id),
    period_start    TEXT NOT NULL,
    accrual_end     TEXT NOT NULL,
    payment_date    TEXT NOT NULL,
    record_date     TEXT,
    amount_status   TEXT NOT NULL,
    amount_per_unit TEXT,
    amount_currency TEXT,
    rate_percent    TEXT,
    source_entry_id TEXT,
    PRIMARY KEY (snapshot_id, period_start),
    CHECK (amount_status IN (
        'amount_fixed', 'rate_fixed_amount_undetermined', 'undetermined'
    )),
    CHECK (
        (amount_status = 'amount_fixed'
             AND amount_per_unit IS NOT NULL AND amount_currency IS NOT NULL)
        OR (amount_status = 'rate_fixed_amount_undetermined'
             AND rate_percent IS NOT NULL AND amount_per_unit IS NULL)
        OR (amount_status = 'undetermined'
             AND amount_per_unit IS NULL AND rate_percent IS NULL)
    ),
    CHECK (accrual_end >= period_start),
    CHECK (payment_date >= accrual_end)
) STRICT;

-- Доля первоначального номинала, а не сумма: сумма зависит от остатка,
-- а остаток выводится.
CREATE TABLE schedule_principal_repayments (
    snapshot_id     TEXT NOT NULL REFERENCES schedule_snapshots (id),
    repayment_date  TEXT NOT NULL,
    share_percent   TEXT NOT NULL,
    source_kind     TEXT NOT NULL,
    source_entry_id TEXT,
    PRIMARY KEY (snapshot_id, repayment_date)
) STRICT;

-- Окно оферты. Пустые условия — незнание, а не заявление об их отсутствии.
CREATE TABLE schedule_offer_windows (
    snapshot_id      TEXT NOT NULL REFERENCES schedule_snapshots (id),
    execution_date   TEXT NOT NULL,
    submission_start TEXT,
    submission_end   TEXT,
    price_percent    TEXT,
    agent            TEXT,
    source_kind      TEXT NOT NULL,
    source_entry_id  TEXT,
    PRIMARY KEY (snapshot_id, execution_date)
) STRICT;

-- Условия выпуска: две оси времени.
CREATE TABLE issue_terms (
    instrument_id            TEXT NOT NULL REFERENCES instruments (id),
    source_id                TEXT NOT NULL,
    observed_at              TEXT NOT NULL,
    effective_from           TEXT,
    maturity_date            TEXT,
    initial_face_value       TEXT,
    face_currency_code       TEXT,
    coupon_periods_per_year  INTEGER,
    day_count                TEXT,
    calendar                 TEXT,
    default_declared         INTEGER NOT NULL CHECK (default_declared IN (0, 1)),
    default_technical        INTEGER NOT NULL CHECK (default_technical IN (0, 1)),
    recorded_at              TEXT NOT NULL,
    PRIMARY KEY (instrument_id, source_id, observed_at)
) STRICT;

CREATE TRIGGER issue_terms_are_immutable
BEFORE UPDATE ON issue_terms
BEGIN
    SELECT RAISE(ABORT, 'условия выпуска append-only: исправление — новое наблюдение');
END;

CREATE TRIGGER issue_terms_are_not_deletable
BEFORE DELETE ON issue_terms
BEGIN
    SELECT RAISE(ABORT, 'условия выпуска append-only: удаление запрещено');
END;

-- Словарь кодов источника — тот же механизм, что broker_operation_kinds,
-- и по тем же причинам.
CREATE TABLE market_source_codes (
    source_id   TEXT NOT NULL,
    domain      TEXT NOT NULL,
    source_code TEXT NOT NULL,
    meaning     TEXT NOT NULL,
    origin      TEXT NOT NULL,
    dictionary  TEXT,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (source_id, domain, source_code),
    CHECK (domain IN ('currency', 'principal_repayment_kind', 'offer_kind')),
    CHECK (origin IN ('seed', 'owner'))
) STRICT;

-- Полнота — три независимых утверждения, а не один флаг (§2.10).
CREATE TABLE schedule_completeness (
    snapshot_id            TEXT NOT NULL REFERENCES schedule_snapshots (id),
    fetch_exhausted        INTEGER NOT NULL CHECK (fetch_exhausted IN (0, 1)),
    structurally_validated INTEGER NOT NULL CHECK (structurally_validated IN (0, 1)),
    incomplete_reason      TEXT,
    pages_seen             TEXT NOT NULL DEFAULT '[]',
    updated_at             TEXT NOT NULL,
    PRIMARY KEY (snapshot_id),
    CHECK ((structurally_validated = 1) = (incomplete_reason IS NULL))
) STRICT;

-- =============================================================================
-- Categories (0015_categories.sql, 0016_category_group_is_income.sql)
-- =============================================================================

-- Category groups are reference data owned by a person: titles are unique
-- per owner, while retirement preserves the names used by historical
-- reports.
CREATE TABLE category_groups (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    retired_at TEXT,
    -- Income is not a second mechanism: it is the same two-level category
    -- list with a flag. The flag lives on the group rather than on each
    -- category so the answer cannot disagree with itself.
    is_income  INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE UNIQUE INDEX category_groups_by_title ON category_groups (owner, title);

-- Categories have one required group so every spending label has exactly
-- one place in the owner's two-level reference tree.
CREATE TABLE categories (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    group_id   TEXT NOT NULL REFERENCES category_groups (id),
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    retired_at TEXT
) STRICT;

CREATE UNIQUE INDEX categories_by_title ON categories (owner, group_id, title);
CREATE INDEX categories_by_owner ON categories (owner, retired_at);

-- =============================================================================
-- Import sessions (0019_import_sessions.sql, 0022_import_control_figures.sql,
-- 0024_import_session_account.sql, 0030_operation_history.sql,
-- 0031_decision_history.sql)
-- =============================================================================

-- An import session is PRE-JOURNAL state. Nothing here is an event and
-- nothing here is in `events`: the journal keeps its property that
-- everything in it is a fact somebody asserted, and a session keeps the
-- opposite one — everything in it is still provisional.
CREATE TABLE import_sessions (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    -- open | committed | abandoned.
    state      TEXT NOT NULL,
    source     TEXT,
    import     TEXT,
    opened_at  TEXT NOT NULL,
    closed_at  TEXT,
    -- The account a declared import session is for. NULL means this session
    -- declared no account: a free session opened without a declaration
    -- legitimately holds rows for several accounts.
    account    TEXT
) STRICT;

CREATE INDEX import_sessions_by_owner ON import_sessions (owner, state);

-- One open session per declared import. Two would split one statement's
-- questions across two places, and the owner would answer one of them.
CREATE UNIQUE INDEX import_sessions_by_import
    ON import_sessions (owner, import)
    WHERE import IS NOT NULL AND state = 'open';

-- One row per submitted line, in submission order, conclusive or observed.
CREATE TABLE import_observations (
    session   TEXT NOT NULL REFERENCES import_sessions (id) ON DELETE CASCADE,
    row       INTEGER NOT NULL,
    row_key   TEXT,
    concluded INTEGER NOT NULL,
    payload   TEXT NOT NULL,
    answer    TEXT,
    -- The classification rule an owner's answer minted, recorded on the
    -- observation it answered. Both nullable, and nothing is back-filled.
    answer_rule         TEXT,
    answer_rule_version INTEGER,
    PRIMARY KEY (session, row)
) STRICT;

CREATE UNIQUE INDEX import_observations_by_key
    ON import_observations (session, row_key)
    WHERE row_key IS NOT NULL;

-- A question is a durable resource, not a sentence in a response body.
CREATE TABLE import_questions (
    id           TEXT PRIMARY KEY,
    session      TEXT NOT NULL REFERENCES import_sessions (id) ON DELETE CASCADE,
    row          INTEGER NOT NULL,
    question     TEXT NOT NULL,
    alternatives TEXT NOT NULL,
    prompt       TEXT NOT NULL,
    asked_at     TEXT NOT NULL,
    answered_at  TEXT,
    answer       TEXT,
    rule         TEXT
) STRICT;

CREATE UNIQUE INDEX import_questions_by_row ON import_questions (session, row);
CREATE INDEX import_questions_open ON import_questions (session, answered_at);

-- The control section a statement prints about itself, held in the session
-- that is importing that statement. Still pre-journal: nothing here is a
-- fact.
CREATE TABLE import_control_figures (
    session         TEXT NOT NULL REFERENCES import_sessions (id) ON DELETE CASCADE,
    -- Not a foreign key: a session holds what a source said, and a section
    -- naming an account the directory does not hold is a finding the
    -- assessment must be able to report rather than an insert that must fail.
    account         TEXT NOT NULL,
    currency        TEXT NOT NULL,
    period_from     TEXT NOT NULL,
    period_to       TEXT NOT NULL,
    opening         INTEGER,
    closing         INTEGER,
    debit_turnover  INTEGER,
    credit_turnover INTEGER,
    stated_at       TEXT NOT NULL,
    PRIMARY KEY (session, account, currency)
) STRICT;

-- =============================================================================
-- The two rule vocabularies, normalised (D9, spec §4.6)
-- =============================================================================

-- Owner classification rules (§10.4). `RuleMatcher` is not an enum: seven
-- independent optional conditions joined by AND, plus `asks_nothing`
-- expressed as a constraint. `Classification` is a tagged outcome of six
-- variants: outcome_kind plus conditional to_account, fee_origin, income_kind.
--
-- A rule is not deleted; it is retired as of a date. An edit creates a new
-- row referring to the previous one via `replaces`.
CREATE TABLE classification_rules (
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
    CHECK (income_kind IS NULL OR income_kind IN ('coupon', 'dividend', 'deposit_interest'))
) STRICT;

-- Номер решения уникален внутри владельца: без этого два одновременных
-- запроса получают один номер, и порядок правил перестаёт быть порядком.
CREATE UNIQUE INDEX classification_rules_by_version
    ON classification_rules (owner, version);

CREATE INDEX classification_rules_by_owner ON classification_rules (owner, retired_at);

-- Category rules are versioned owner decisions: retirement preserves the
-- rule under which historical reports were computed, while validity bounds
-- prevent a merchant's changed trade from being applied to every date.
-- `CategoryMatcher` is a genuine enum of four: matcher_kind, value, text,
-- description_mode, with a conditional CHECK per kind.
CREATE TABLE category_rules (
    id          TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    version     INTEGER NOT NULL,

    -- CategoryMatcher, one table for all four variants.
    matcher_kind      TEXT NOT NULL,
    value             TEXT,
    text              TEXT,
    description_mode  TEXT,

    category    TEXT NOT NULL REFERENCES categories (id),
    valid_from  TEXT,
    valid_to    TEXT,
    created_at  TEXT NOT NULL,
    retired_at  TEXT,
    replaces    TEXT REFERENCES category_rules (id),

    CHECK (matcher_kind IN ('row', 'source_category', 'description_contains', 'description')),
    CHECK (description_mode IS NULL OR description_mode IN ('equals', 'starts_with', 'contains')),
    CHECK (
        CASE matcher_kind
            WHEN 'row'                  THEN value IS NOT NULL AND text IS NULL AND description_mode IS NULL
            WHEN 'source_category'      THEN value IS NOT NULL AND text IS NULL AND description_mode IS NULL
            WHEN 'description_contains' THEN text IS NOT NULL AND value IS NULL AND description_mode IS NULL
            WHEN 'description'          THEN text IS NOT NULL AND description_mode IS NOT NULL AND value IS NULL
        END
    )
) STRICT;

-- A version is unique within an owner so concurrent writes cannot make rule
-- order ambiguous.
CREATE UNIQUE INDEX category_rules_by_version ON category_rules (owner, version);
CREATE INDEX category_rules_by_owner ON category_rules (owner, retired_at);

-- =============================================================================
-- The journal (spec §4.1-4.4)
-- =============================================================================

-- The envelope, dates and provenance of one fact. `events.payload` — the
-- JSON document this table used to carry beside its lifted columns — is
-- removed with no copy kept anywhere (D2): every field a fact has is a typed
-- column here, on `event_legs`, or on one of the family detail tables below.
--
-- Append-only is enforced by the store's API, not by a trigger (D6): there
-- is one journal write and it inserts a whole event, and correction is a new
-- event. `SqliteStore::connection()`/`connection_mut()` move behind a
-- test-only feature so production code has no way to reach this table
-- outside that one write path.
CREATE TABLE events (
    id                      TEXT PRIMARY KEY,
    owner                   TEXT NOT NULL,
    account                 TEXT NOT NULL,
    kind                    TEXT NOT NULL,
    effective_date          TEXT NOT NULL,
    -- EffectiveOrder::source_time.
    source_time             TEXT,
    sequence                INTEGER NOT NULL,
    confidence              TEXT NOT NULL,
    relation_kind           TEXT NOT NULL,
    relation_target         TEXT,
    idempotency_key         TEXT,
    recorded_at             TEXT NOT NULL,

    -- EventDates, all optional.
    date_trade              TEXT,
    date_settled            TEXT,
    date_cash_posted        TEXT,
    date_entitlement        TEXT,
    date_paid               TEXT,
    -- TaxPeriod(i32): a calendar year, not a date.
    tax_period_override     INTEGER,

    -- Provenance.
    source                  TEXT NOT NULL,
    raw_hash                TEXT NOT NULL,
    parser_version          TEXT NOT NULL,
    source_operation_id     TEXT,
    source_category         TEXT,
    source_kind             TEXT,
    owner_category          TEXT,
    source_code             TEXT,
    -- `Provenance::description`: holds either a description or a printed
    -- counterparty, hence the wider name (iaam-b4xw).
    source_description      TEXT,
    import                  TEXT,
    import_session          TEXT,
    declared_by             TEXT,
    -- NULL | no_rule | rule | answered_minting_rule.
    rule_settlement         TEXT,
    settled_by_rule         TEXT,
    settled_by_rule_version INTEGER,
    row_document            TEXT,
    row_sheet               TEXT,
    row_number              INTEGER,

    CHECK (kind IN (
        'trade', 'cash_in', 'cash_out', 'refund', 'cash_transfer',
        'own_account_movement', 'unresolved_own_account_movement',
        'income', 'fee', 'tax', 'opening_position', 'opening_cash',
        'valuation', 'control_assertion', 'import_coverage_gap',
        'corporate_action', 'offer_exercise'
    )),
    CHECK (confidence IN ('known', 'estimated', 'unknown')),
    CHECK (relation_kind IN ('none', 'reversal', 'replacement')),
    CHECK ((relation_kind = 'none') = (relation_target IS NULL)),
    -- `rule_settlement` is nullable, so a two-sided equality is not enough:
    -- a CHECK that evaluates to NULL passes in SQLite. The four states are
    -- spelled out.
    CHECK (
        (rule_settlement IS NULL AND settled_by_rule IS NULL AND settled_by_rule_version IS NULL)
        OR (rule_settlement = 'no_rule' AND settled_by_rule IS NULL AND settled_by_rule_version IS NULL)
        OR (rule_settlement IN ('rule', 'answered_minting_rule')
            AND settled_by_rule IS NOT NULL AND settled_by_rule_version IS NOT NULL)
    ),
    CHECK ((row_document IS NULL) = (row_number IS NULL)),
    CHECK (row_sheet IS NULL OR row_document IS NOT NULL),
    -- `RowLocator.row` is `u64` and SQLite `INTEGER` is a signed `i64`: this
    -- schema does not promise to round-trip the whole `u64` range.
    CHECK (row_number IS NULL OR row_number >= 0),

    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id),
    -- Owner-scoped and deferred — not for sealing, which D6 removed, but
    -- because a bundle import inserts a graph and a replacement event can
    -- precede its target. Deferring one key is simpler than topologically
    -- sorting the import.
    FOREIGN KEY (owner, relation_target) REFERENCES events (owner, id)
        DEFERRABLE INITIALLY DEFERRED

    -- Not declared, and deliberately: `import_session` and `settled_by_rule`
    -- look like foreign keys and must not be. `Bundle` carries events,
    -- accounts and contours and nothing else, so restoring an archive into
    -- an empty database would fail on both — they are archival provenance
    -- handles, not references into a registry this database keeps.
) STRICT;

-- A composite foreign key needs a unique index matching exactly its
-- columns; `id` alone is already the primary key, so (owner, id) is unique
-- a fortiori, but SQLite still requires the index to exist by that name.
-- Needed for `events`' own self-referencing `relation_target` key above,
-- the same way `accounts_by_owner` serves `contour_accounts`' key into
-- `accounts`.
CREATE UNIQUE INDEX events_by_owner_id ON events (owner, id);

-- Порядок проекции: дата, затем sequence, затем идентификатор. Уникальность
-- (owner, дата, sequence) обязательна: без неё два одновременных запроса
-- получают один и тот же номер (§4.8).
CREATE UNIQUE INDEX events_by_order ON events (owner, effective_date, sequence);

-- Идемпотентность (§10.6).
CREATE UNIQUE INDEX events_idempotency_key
    ON events (owner, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

-- Account-scoped since migration 0012: reverting it to
-- (owner, source, source_operation_id) would break IdentityScope::Account.
CREATE UNIQUE INDEX events_source_operation
    ON events (owner, source, account, source_operation_id)
    WHERE source_operation_id IS NOT NULL;

-- Legs: the money and the securities. The shape of a leg follows from its
-- kind, and one conditional CHECK carries it (spec §4.2). `ordinal` fixes a
-- stable order for reconstruction and carries no meaning: no reader may find
-- a leg by `ordinal = 0`.
--
-- Owner scoping on children is application-level: a composite (owner,
-- account) foreign key would need an `owner` column on every child table,
-- and duplicating it across sixteen tables for tenant isolation in a
-- single-owner system is not worth it. The write path checks that every
-- account and custody place named by a leg belongs to the event's owner.
CREATE TABLE event_legs (
    event      TEXT NOT NULL REFERENCES events (id),
    ordinal    INTEGER NOT NULL CHECK (ordinal >= 0),
    kind       TEXT NOT NULL,
    account    TEXT NOT NULL REFERENCES accounts (id),
    custody    TEXT REFERENCES custody_places (id),
    instrument TEXT REFERENCES instruments (id),
    amount     INTEGER,
    currency   TEXT,
    quantity   TEXT,
    PRIMARY KEY (event, ordinal),
    CHECK (kind IN ('cash', 'security_quantity', 'principal', 'fee', 'tax')),
    CHECK (
        CASE kind
            WHEN 'cash'              THEN amount IS NOT NULL AND currency IS NOT NULL
                                      AND custody IS NULL AND instrument IS NULL AND quantity IS NULL
            WHEN 'fee'               THEN amount IS NOT NULL AND currency IS NOT NULL
                                      AND custody IS NULL AND instrument IS NULL AND quantity IS NULL
            WHEN 'tax'               THEN amount IS NOT NULL AND currency IS NOT NULL
                                      AND custody IS NULL AND instrument IS NULL AND quantity IS NULL
            WHEN 'principal'         THEN amount IS NOT NULL AND currency IS NOT NULL
                                      AND instrument IS NOT NULL AND custody IS NULL AND quantity IS NULL
            WHEN 'security_quantity' THEN instrument IS NOT NULL AND custody IS NOT NULL
                                      AND quantity IS NOT NULL AND amount IS NULL AND currency IS NULL
        END
    )
) STRICT;

CREATE INDEX event_legs_by_account ON event_legs (account, event);
CREATE INDEX event_legs_by_currency ON event_legs (currency, event) WHERE currency IS NOT NULL;

-- --- Detail tables, one per family (spec §4.3). Only variants carrying
-- something beyond the legs get a table: CashIn, CashOut, Refund,
-- OpeningCash and OwnAccountMovement have none — their whole content is
-- events.kind plus the legs.

-- Trade: gross, fee, accrued_interest, basis_fee, basis_fee_exact are all
-- STORED (spec §4.5) — gross is not recoverable from the settlement leg
-- without unwinding the fee and the accrued interest.
CREATE TABLE event_trade (
    event                      TEXT PRIMARY KEY REFERENCES events (id),
    side                       TEXT NOT NULL CHECK (side IN ('buy', 'sell')),
    instrument                 TEXT NOT NULL REFERENCES instruments (id),
    quantity                   TEXT NOT NULL,
    gross_amount               INTEGER NOT NULL,
    gross_currency             TEXT NOT NULL,
    fee_amount                 INTEGER,
    fee_currency               TEXT,
    -- Posted basis-only fee; unlike fee, it is absent from the cash leg.
    basis_fee_amount           INTEGER,
    basis_fee_currency         TEXT,
    -- Exact source commission retained for audit of basis_fee rounding.
    basis_fee_exact_value      TEXT,
    basis_fee_exact_currency   TEXT,
    accrued_interest_amount    INTEGER,
    accrued_interest_currency  TEXT,
    CHECK ((fee_amount IS NULL) = (fee_currency IS NULL)),
    CHECK ((basis_fee_amount IS NULL) = (basis_fee_currency IS NULL)),
    CHECK ((basis_fee_exact_value IS NULL) = (basis_fee_exact_currency IS NULL)),
    CHECK ((accrued_interest_amount IS NULL) = (accrued_interest_currency IS NULL))
) STRICT;

-- Cash transfer: both endpoints are stored, and the first draft was wrong to
-- drop them (spec §4.3, codex). Both legs of a transfer are LegKind::Cash
-- and carry no role, so which leg is `from` and which is `to` is not
-- recoverable from the legs alone. `amount` is reconstructed (spec §4.5)
-- from the positive `to` leg, once these columns say which leg that is.
CREATE TABLE event_cash_transfer (
    event        TEXT PRIMARY KEY REFERENCES events (id),
    transfer_id  TEXT NOT NULL,
    from_account TEXT NOT NULL REFERENCES accounts (id),
    to_account   TEXT NOT NULL REFERENCES accounts (id)
) STRICT;

CREATE INDEX event_cash_transfer_by_from ON event_cash_transfer (from_account, event);
CREATE INDEX event_cash_transfer_by_to ON event_cash_transfer (to_account, event);

-- Unresolved own-account movement: this variant has no leg by design.
CREATE TABLE event_unresolved_movement (
    event    TEXT PRIMARY KEY REFERENCES events (id),
    amount   INTEGER NOT NULL,
    currency TEXT NOT NULL
) STRICT;

-- Income: `gross` is the cash leg and is reconstructed (spec §4.5).
CREATE TABLE event_income (
    event       TEXT PRIMARY KEY REFERENCES events (id),
    instrument  TEXT REFERENCES instruments (id),
    income_kind TEXT,
    CHECK (income_kind IS NULL OR income_kind IN ('coupon', 'dividend', 'deposit_interest'))
) STRICT;

-- Fee not tied to a trade. `amount` is the cash leg and is reconstructed.
CREATE TABLE event_fee (
    event  TEXT PRIMARY KEY REFERENCES events (id),
    origin TEXT NOT NULL,
    CHECK (origin IN ('brokerage', 'depositary', 'account_maintenance', 'margin_interest', 'other'))
) STRICT;

-- Tax, whether withheld at source or paid by the owner. `amount` is the
-- cash leg and is reconstructed.
CREATE TABLE event_tax (
    event  TEXT PRIMARY KEY REFERENCES events (id),
    origin TEXT NOT NULL,
    CHECK (origin IN ('withheld_at_source', 'self_paid'))
) STRICT;

-- Reconstructed opening position: `OpeningAssertions` is nine scalar fields
-- (spec §4.3) and needs no child table. No constraint is added between them
-- that the domain does not have — `acquisition_date` may be set with
-- certainty `Unknown`, and `basis_currency` without `basis_rate`.
CREATE TABLE event_opening_position (
    event                      TEXT PRIMARY KEY REFERENCES events (id),
    instrument                 TEXT NOT NULL REFERENCES instruments (id),
    quantity                   TEXT NOT NULL,
    cost_basis_amount          INTEGER,
    cost_basis_currency        TEXT,
    -- OpeningAssertions, all nine fields. `quantity_certainty` is the
    -- assertion's own field and is distinct from `quantity` above, which is
    -- the reconstructed position's actual value.
    quantity_certainty         TEXT NOT NULL,
    acquisition_date           TEXT,
    acquisition_date_certainty TEXT NOT NULL,
    tax_basis_certainty        TEXT NOT NULL,
    basis_currency             TEXT,
    basis_rate                 TEXT,
    fees_included              TEXT NOT NULL,
    ldv_eligibility            TEXT NOT NULL,
    prior_corporate_actions    TEXT NOT NULL,
    CHECK ((cost_basis_amount IS NULL) = (cost_basis_currency IS NULL)),
    CHECK (quantity_certainty IN ('known', 'estimated')),
    CHECK (acquisition_date_certainty IN ('known', 'estimated', 'unknown')),
    CHECK (tax_basis_certainty IN ('documented', 'estimated', 'unknown')),
    CHECK (fees_included IN ('yes', 'no', 'unknown')),
    CHECK (ldv_eligibility IN ('known', 'unknown')),
    CHECK (prior_corporate_actions IN ('known', 'unknown'))
) STRICT;

-- Valuation of an instrument at a per-unit price (spec §5.4). Moves no
-- money: the event has no legs.
CREATE TABLE event_valuation (
    event      TEXT PRIMARY KEY REFERENCES events (id),
    instrument TEXT NOT NULL REFERENCES instruments (id),
    price      TEXT NOT NULL,
    currency   TEXT NOT NULL,
    quality    TEXT NOT NULL,
    CHECK (quality IN ('executable', 'previous_close', 'carried_forward', 'stale', 'owner_estimate'))
) STRICT;

-- The source's control assertion about interval completeness (spec §10.3).
-- `ControlClaim` is six variants sharing a family table with a discriminant
-- and conditional CHECKs, not a table per leaf. Moves no money: no legs.
CREATE TABLE event_control_assertion (
    event         TEXT PRIMARY KEY REFERENCES events (id),
    period_from   TEXT NOT NULL,
    period_to     TEXT NOT NULL,
    claim_kind    TEXT NOT NULL,
    -- cash_balance, position_quantity only.
    balance_point TEXT,
    -- cash_balance, cash_turnover, fees_total, income_total, tax_withheld_total.
    currency      TEXT,
    amount        INTEGER,
    -- position_quantity only.
    instrument    TEXT REFERENCES instruments (id),
    custody       TEXT REFERENCES custody_places (id),
    quantity      TEXT,
    -- cash_turnover only.
    debit         INTEGER,
    credit        INTEGER,
    CHECK (period_to >= period_from),
    CHECK (claim_kind IN (
        'cash_balance', 'position_quantity', 'cash_turnover',
        'fees_total', 'income_total', 'tax_withheld_total'
    )),
    CHECK (balance_point IS NULL OR balance_point IN ('opening', 'closing')),
    CHECK (
        CASE claim_kind
            WHEN 'cash_balance' THEN
                currency IS NOT NULL AND amount IS NOT NULL AND balance_point IS NOT NULL
                AND instrument IS NULL AND custody IS NULL AND quantity IS NULL
                AND debit IS NULL AND credit IS NULL
            WHEN 'position_quantity' THEN
                instrument IS NOT NULL AND custody IS NOT NULL AND quantity IS NOT NULL
                AND balance_point IS NOT NULL
                AND currency IS NULL AND amount IS NULL AND debit IS NULL AND credit IS NULL
            WHEN 'cash_turnover' THEN
                currency IS NOT NULL AND debit IS NOT NULL AND credit IS NOT NULL
                AND amount IS NULL AND balance_point IS NULL
                AND instrument IS NULL AND custody IS NULL AND quantity IS NULL
            WHEN 'fees_total' THEN
                currency IS NOT NULL AND amount IS NOT NULL
                AND balance_point IS NULL AND instrument IS NULL AND custody IS NULL
                AND quantity IS NULL AND debit IS NULL AND credit IS NULL
            WHEN 'income_total' THEN
                currency IS NOT NULL AND amount IS NOT NULL
                AND balance_point IS NULL AND instrument IS NULL AND custody IS NULL
                AND quantity IS NULL AND debit IS NULL AND credit IS NULL
            WHEN 'tax_withheld_total' THEN
                currency IS NOT NULL AND amount IS NOT NULL
                AND balance_point IS NULL AND instrument IS NULL AND custody IS NULL
                AND quantity IS NULL AND debit IS NULL AND credit IS NULL
        END
    )
) STRICT;

-- An import attempt refused rows, so it cannot confirm the dimensions those
-- rows would have moved. `refused` and the top-level `dimensions` set are
-- NOT stored (spec §4.4): they are a count and a union over
-- `event_coverage_gap_rows` below, and storing them would be one value in
-- three places with no trigger left to hold either invariant.
CREATE TABLE event_coverage_gap (
    event       TEXT PRIMARY KEY REFERENCES events (id),
    period_from TEXT NOT NULL,
    period_to   TEXT NOT NULL,
    CHECK (period_to >= period_from)
) STRICT;

-- Corporate action on a security: amortisation, redemption, replacement
-- (spec §4.7). One table for the three-variant family with a discriminant
-- and conditional CHECKs. `custody` is common to all three variants and is
-- NOT NULL unconditionally.
CREATE TABLE event_corporate_action (
    event         TEXT PRIMARY KEY REFERENCES events (id),
    action_kind   TEXT NOT NULL,
    -- partial_redemption, redemption.
    instrument    TEXT REFERENCES instruments (id),
    -- conversion.
    predecessor   TEXT REFERENCES instruments (id),
    successor     TEXT REFERENCES instruments (id),
    custody       TEXT NOT NULL REFERENCES custody_places (id),
    -- partial_redemption, redemption.
    quantity      TEXT,
    -- conversion.
    quantity_in   TEXT,
    quantity_out  TEXT,
    ratio         TEXT,
    -- partial_redemption, redemption: PerUnitAmount.
    principal_returned_per_unit_value    TEXT,
    principal_returned_per_unit_currency TEXT,
    -- partial_redemption, redemption: required Money. conversion: optional.
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
    CHECK ((principal_returned_per_unit_value IS NULL) = (principal_returned_per_unit_currency IS NULL)),
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
                AND principal_returned_per_unit_value IS NOT NULL
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

-- Exercising an offer is the holder's right, not an issuer decision, so it
-- is a separate family from corporate actions (spec §4.7).
CREATE TABLE event_offer_exercise (
    event       TEXT PRIMARY KEY REFERENCES events (id),
    action_kind TEXT NOT NULL,
    submission  TEXT NOT NULL,
    -- offer_submitted only.
    window      TEXT,
    -- offer_submitted, offer_settled.
    instrument  TEXT REFERENCES instruments (id),
    -- offer_settled only.
    custody     TEXT REFERENCES custody_places (id),
    quantity    TEXT NOT NULL,
    gross_amount              INTEGER,
    gross_currency            TEXT,
    fee_amount                INTEGER,
    fee_currency              TEXT,
    accrued_interest_amount   INTEGER,
    accrued_interest_currency TEXT,
    CHECK (action_kind IN ('offer_submitted', 'offer_cancelled', 'offer_settled')),
    CHECK ((action_kind = 'offer_submitted') = (window IS NOT NULL)),
    CHECK ((action_kind = 'offer_settled')   = (gross_amount IS NOT NULL)),
    CHECK ((action_kind = 'offer_settled')   = (custody IS NOT NULL)),
    CHECK ((instrument IS NOT NULL) = (action_kind IN ('offer_submitted', 'offer_settled'))),
    CHECK ((gross_amount IS NULL) = (gross_currency IS NULL)),
    CHECK ((fee_amount IS NULL) = (fee_currency IS NULL)),
    CHECK ((accrued_interest_amount IS NULL) = (accrued_interest_currency IS NULL))
) STRICT;

-- --- Collections get child tables (spec §4.4).

-- `row_name_kind` is given | fingerprint: `SourceRowKey` distinguishes a
-- name the source gave from a fingerprint we computed, and the two are
-- different rows even when the text matches.
CREATE TABLE event_coverage_gap_rows (
    event          TEXT NOT NULL REFERENCES events (id),
    ordinal        INTEGER NOT NULL CHECK (ordinal >= 0),
    source         TEXT NOT NULL,
    row_name_kind  TEXT NOT NULL,
    row_name_value TEXT NOT NULL,
    PRIMARY KEY (event, ordinal),
    CHECK (row_name_kind IN ('given', 'fingerprint'))
) STRICT;

CREATE TABLE event_coverage_gap_row_dimensions (
    event     TEXT NOT NULL,
    ordinal   INTEGER NOT NULL,
    dimension TEXT NOT NULL,
    PRIMARY KEY (event, ordinal, dimension),
    FOREIGN KEY (event, ordinal) REFERENCES event_coverage_gap_rows (event, ordinal),
    CHECK (dimension IN ('cash', 'positions', 'tax_basis', 'income'))
) STRICT;

-- =============================================================================
-- Category-assignment projection (spec §4.7)
-- =============================================================================

-- This is a read model and it is stated as one: it is derived from the fact
-- and the owner's active rules by `iaam_core::category::assign`, it is
-- rebuilt rather than repaired, and no reader treats it as evidence. A row
-- absent from this table means `NotDecomposed`, which is the honest answer
-- and never a silent bucket.
--
-- `rules_revision` is the highest active `category_rules.version` the
-- projection was built from; it is checked on read, and a mismatch means
-- the projection is stale and is rebuilt before the query is answered.
CREATE TABLE event_category_assignments (
    owner          TEXT NOT NULL,
    event          TEXT NOT NULL REFERENCES events (id),
    category       TEXT NOT NULL REFERENCES categories (id),
    rule           TEXT NOT NULL REFERENCES category_rules (id),
    basis          TEXT NOT NULL,
    rules_revision INTEGER NOT NULL,
    PRIMARY KEY (owner, event),
    CHECK (basis IN ('row', 'source_category', 'description'))
) STRICT;

CREATE INDEX event_category_assignments_by_category
    ON event_category_assignments (owner, category, event);

-- =============================================================================
-- Journal keyset indexes (spec §6.2)
-- =============================================================================

-- Every keyset filter carries the ordering columns, so one index answers
-- the predicate and the page order together rather than stopping a column
-- short.
CREATE INDEX events_by_account ON events (owner, account, effective_date, sequence);
CREATE INDEX events_by_kind ON events (owner, kind, effective_date, sequence);
CREATE INDEX events_by_source ON events (owner, source, effective_date, sequence);
CREATE INDEX events_by_import
    ON events (owner, import, effective_date, sequence)
    WHERE import IS NOT NULL;
CREATE INDEX events_by_import_session
    ON events (owner, import_session, effective_date, sequence)
    WHERE import_session IS NOT NULL;
CREATE INDEX events_by_settled_by_rule
    ON events (owner, settled_by_rule, effective_date, sequence)
    WHERE settled_by_rule IS NOT NULL;
CREATE INDEX events_by_source_category
    ON events (owner, source_category, effective_date, sequence)
    WHERE source_category IS NOT NULL;
CREATE INDEX events_by_relation_target
    ON events (owner, relation_target)
    WHERE relation_target IS NOT NULL;
