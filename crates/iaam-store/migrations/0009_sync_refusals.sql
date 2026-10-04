-- =============================================================================
-- Sync refusals: the reading/question state a broker sync leaves behind.
-- The journal's coverage-gap event keeps the attempt statement — dimensions
-- and the count — while this table keeps what the owner can act on: each
-- refused row's stable key, its original payload, the reason and the remedy
-- (iaam-vg8te.1.2). It is deliberately NOT a journal event: it is the sync's
-- own reading, pre-journal, and a re-sync of the same row updates it rather
-- than minting a second fact.
CREATE TABLE sync_refusals (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    account    TEXT NOT NULL,
    source     TEXT NOT NULL,
    row_key    TEXT NOT NULL,
    range_from TEXT NOT NULL,
    range_to   TEXT NOT NULL,
    dimensions TEXT NOT NULL,
    reason     TEXT NOT NULL,
    payload    TEXT NOT NULL,
    settled    INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
) STRICT;

-- One refusal per owned row: a re-sync of the same unchanged row updates the
-- record and never duplicates it, so the owner's question does not pile up.
CREATE UNIQUE INDEX sync_refusals_by_row
    ON sync_refusals (owner, account, source, row_key);

CREATE INDEX sync_refusals_open_by_account
    ON sync_refusals (owner, account, source) WHERE settled = 0;
