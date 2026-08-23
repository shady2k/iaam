-- Доступ к брокерскому каналу (§14).
--
-- Токен лежит только шифротекстом: ключ живёт вне базы, и утечка файла
-- базы не даёт доступа к брокерскому счёту. Хранилище держит nonce
-- и шифротекст непрозрачными байтами — расшифровка живёт в iaam-broker,
-- и адаптеру хранилища знать о ней нечего.
--
-- Область прав хранится строкой и толкуется в iaam-broker: строка,
-- обещающая торговые права, там не разбирается и даёт отказ.

CREATE TABLE broker_access (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    broker     TEXT NOT NULL,
    scope      TEXT NOT NULL,
    nonce      BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT
) STRICT;

-- Один действующий доступ на пару владелец+брокер: второй означает,
-- что неизвестно, каким из них система ходит к брокеру. Отозванные
-- под условие не попадают и остаются историей.
CREATE UNIQUE INDEX broker_access_active
    ON broker_access (owner, broker)
    WHERE revoked_at IS NULL;
