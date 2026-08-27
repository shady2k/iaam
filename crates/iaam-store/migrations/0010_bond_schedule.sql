-- 0010: график выплат облигаций снимками (спека E3.4 §2.2).
--
-- Единица наблюдения — снимок графика выпуска ЦЕЛИКОМ, а не строка.
-- Построчная модель не умеет выразить исчезновение строки: отсутствие
-- новой версии по старой координате неотличимо от «источник не присылал
-- обновлений», и отменённая эмитентом амортизация остаётся рядом с новым
-- графиком, удваивая выплату. Стабильного идентификатора записи, которым
-- эту беду обычно чинят, источник не даёт вовсе.
CREATE TABLE schedule_snapshots (
    id            TEXT PRIMARY KEY,
    instrument_id TEXT NOT NULL REFERENCES instruments(id),
    source_id     TEXT NOT NULL,
    observed_at   TEXT NOT NULL,
    -- Хэш содержимого снимка. Снимок с неизменным содержимым не пишется:
    -- иначе ежедневная синхронизация писала бы неизменный график каждый
    -- день и раздувала ряд в сотни раз.
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
-- снимку. Колонка observed_at здесь вернула бы построчную модель.
CREATE TABLE schedule_coupon_periods (
    snapshot_id     TEXT NOT NULL REFERENCES schedule_snapshots(id),
    period_start    TEXT NOT NULL,
    accrual_end     TEXT NOT NULL,
    -- Дата платежа отдельно от конца начисления: перенос с выходного
    -- двигает первую, но не второй, а НКД считается по второму.
    payment_date    TEXT NOT NULL,
    record_date     TEXT,
    -- Статус определённости выплаты. Список закрыт (§2.3).
    amount_status   TEXT NOT NULL,
    -- Сумма на единицу ПЕРВОНАЧАЛЬНОГО номинала. NULL — неизвестно,
    -- и это не ноль: ноль — присутствующее значение.
    amount_per_unit TEXT,
    amount_currency TEXT,
    rate_percent    TEXT,
    source_entry_id TEXT,
    PRIMARY KEY (snapshot_id, period_start),
    CHECK (amount_status IN (
        'amount_fixed', 'rate_fixed_amount_undetermined', 'undetermined'
    )),
    -- Статус и наличие полей обязаны сходиться: строка со статусом
    -- «сумма известна» и пустой суммой — молчаливый ноль в потоке.
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
-- а остаток выводится. Окончательность возврата здесь не хранится —
-- она свойство проекции, и кода окончательности источник даёт не всегда.
CREATE TABLE schedule_principal_repayments (
    snapshot_id     TEXT NOT NULL REFERENCES schedule_snapshots(id),
    repayment_date  TEXT NOT NULL,
    share_percent   TEXT NOT NULL,
    -- Как вид назвал источник. Множество открыто и источнику принадлежит,
    -- поэтому текст без CHECK: толкование — в словаре market_source_codes.
    source_kind     TEXT NOT NULL,
    source_entry_id TEXT,
    PRIMARY KEY (snapshot_id, repayment_date)
) STRICT;

-- Окно оферты. Пустые условия — незнание, а не заявление об их отсутствии:
-- источник массово отдаёт окна без дат подачи, без цены и без агента.
CREATE TABLE schedule_offer_windows (
    snapshot_id      TEXT NOT NULL REFERENCES schedule_snapshots(id),
    execution_date   TEXT NOT NULL,
    submission_start TEXT,
    submission_end   TEXT,
    price_percent    TEXT,
    agent            TEXT,
    source_kind      TEXT NOT NULL,
    source_entry_id  TEXT,
    PRIMARY KEY (snapshot_id, execution_date)
) STRICT;

-- Условия выпуска: две оси времени. effective_from NULL означает, что
-- источник даты вступления в силу не сообщил, — и это НЕ повод подставить
-- observed_at: догадка, выданная за факт, воспроизводит условия, которых
-- на выбранную дату не существовало.
CREATE TABLE issue_terms (
    instrument_id            TEXT NOT NULL REFERENCES instruments(id),
    source_id                TEXT NOT NULL,
    observed_at              TEXT NOT NULL,
    effective_from           TEXT,
    maturity_date            TEXT,
    initial_face_value       TEXT,
    -- Код валюты как его назвал источник. Перевод — словарём.
    face_currency_code       TEXT,
    coupon_periods_per_year  INTEGER,
    -- База начисления дней и календарь: у MOEX всегда NULL. Значения
    -- по умолчанию здесь запрещены — подставленный day-count даёт
    -- правдоподобно неверный НКД.
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

-- Словарь кодов источника — тот же механизм, что broker_operation_kinds
-- (0009), и по тем же причинам: вид права по оферте у MOEX это свободный
-- русский текст, а один источник даёт два кода на одну валюту (SUR в
-- описании выпуска и RUB в графике одного выпуска). Зашитый в разборщик
-- match ломается от правки формулировки на стороне биржи.
--
-- Члена 'other' здесь нет намеренно: «код, которого нет в словаре»
-- выражается ОТСУТСТВИЕМ строки и даёт явный отказ.
CREATE TABLE market_source_codes (
    source_id   TEXT NOT NULL,
    -- Что именно классифицируется: валюта, вид возврата номинала,
    -- вид права по оферте.
    domain      TEXT NOT NULL,
    source_code TEXT NOT NULL,
    meaning     TEXT NOT NULL,
    -- Откуда строка: наш засев или решение владельца. Без этого решение
    -- владельца неотличимо от засева и молча затирается им.
    origin      TEXT NOT NULL,
    dictionary  TEXT,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (source_id, domain, source_code),
    CHECK (domain IN ('currency', 'principal_repayment_kind', 'offer_kind')),
    CHECK (origin IN ('seed', 'owner'))
) STRICT;

-- Полнота — три независимых утверждения, а не один флаг (§2.10).
-- Полностью вычитанный источник с дырой внутри проходил бы как полный.
CREATE TABLE schedule_completeness (
    snapshot_id            TEXT NOT NULL REFERENCES schedule_snapshots(id),
    -- Источник вычитан до конца по его собственным правилам.
    fetch_exhausted        INTEGER NOT NULL CHECK (fetch_exhausted IN (0, 1)),
    -- Доменные инварианты профиля источника выполнены.
    structurally_validated INTEGER NOT NULL CHECK (structurally_validated IN (0, 1)),
    -- Причина, если инварианты нарушены. Не 'complete_prefix': усечённый
    -- график выглядит замкнутым и правдоподобным.
    incomplete_reason      TEXT,
    -- Просмотренные смещения страниц — след запуска.
    pages_seen             TEXT NOT NULL DEFAULT '[]',
    updated_at             TEXT NOT NULL,
    PRIMARY KEY (snapshot_id),
    CHECK ((structurally_validated = 1) = (incomplete_reason IS NULL))
) STRICT;
