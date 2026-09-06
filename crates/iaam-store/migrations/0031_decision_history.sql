-- Audit trail for reversible standing decisions.
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
