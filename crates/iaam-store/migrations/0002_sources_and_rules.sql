-- Сырьё источников (§10.1) и правила классификации (§10.4).
--
-- Сырьё хранится, потому что версия парсера пишется в provenance
-- ради повторного разбора: разбор без сырья повторить нельзя, и
-- исправленный парсер оказался бы бесполезен для уже загруженного.

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
-- колонки первичного ключа неявно NOT NULL, а у CSV листа нет, и
-- `PRIMARY KEY (document, sheet, row)` запретил бы хранить его строки
-- вовсе. Пустой строкой лист не подменяется: неизвестное — NULL (§4.9).
--
-- `ifnull` в индексе обязателен: в обычном уникальном индексе SQLite
-- считает NULL несовпадающими, и один и тот же кусок сырья без листа
-- лёг бы в базу дважды.
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

-- Правила классификации (§10.4). Меняются владельцем, поэтому обычная
-- таблица; версия нужна, чтобы пересчёт истории знал, каким правилом
-- он вызван.
CREATE TABLE classification_rules (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    version    INTEGER NOT NULL,
    matcher    TEXT NOT NULL,
    outcome    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    retired_at TEXT,
    -- Правка правила — новая строка, ссылающаяся на прежнюю. Без этой
    -- колонки вывод из обращения и заведение остаются двумя
    -- несвязанными строками, и «как правило дошло до нынешнего вида»
    -- ответа не имеет.
    replaces   TEXT REFERENCES classification_rules (id)
) STRICT;

-- Номер решения уникален внутри владельца: без этого два одновременных
-- запроса получают один номер, и порядок правил перестаёт быть порядком.
CREATE UNIQUE INDEX classification_rules_by_version
    ON classification_rules (owner, version);

CREATE INDEX classification_rules_by_owner ON classification_rules (owner, retired_at);
