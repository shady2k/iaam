-- Category groups are reference data owned by a person: titles are unique per
-- owner, while retirement preserves the names used by historical reports.
CREATE TABLE category_groups (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    retired_at TEXT
) STRICT;

CREATE UNIQUE INDEX category_groups_by_title ON category_groups (owner, title);

-- Categories have one required group so every spending label has exactly one
-- place in the owner's two-level reference tree.
CREATE TABLE categories (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    group_id   TEXT NOT NULL REFERENCES category_groups (id),
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    -- Retirement, never deletion: rules and printed reports point at this row,
    -- and removing it would turn a past report into a lie about what it said.
    retired_at TEXT
) STRICT;

CREATE UNIQUE INDEX categories_by_title ON categories (owner, group_id, title);
CREATE INDEX categories_by_owner ON categories (owner, retired_at);
