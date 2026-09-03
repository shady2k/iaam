-- The identity a source prints for an account, the further identifiers that
-- reach the same account, and the class of cash the owner says it holds
-- (decision 0004).
--
-- Every account that exists today has none of the three. The migration adds
-- columns and a table and invents nothing: an account written before this
-- migration keeps a NULL provider, and NULL is «he has not said», never a
-- placeholder (§4.9).

-- The client's own label for the source. It scopes the identifier below:
-- without it, two sources that both print short sequential identifiers would
-- collide on values neither of them controls.
ALTER TABLE accounts ADD COLUMN provider TEXT;

-- What the source prints for this account. Opaque to iaam: it is not parsed,
-- not shape-checked, not validated against a register, and never rendered
-- anywhere a title belongs. The only operations on it are equality and the
-- uniqueness below.
ALTER TABLE accounts ADD COLUMN provider_account_id TEXT;

-- The owner's statement about what kind of cash this is: deposit, savings,
-- card_account or wallet. NULL is «not stated» and is never filled by a guess.
--
-- No CHECK constraint, following `instruments.kind`: the set of codes is the
-- Rust enum, and a second copy of it in SQL would be a second truth to keep in
-- step. A code the enum does not know is rejected on the way out, rather than
-- becoming a deposit by default.
ALTER TABLE accounts ADD COLUMN cash_class TEXT;

-- `(owner, provider, provider_account_id)` is unique, and the partial index
-- says out loud what SQLite's treatment of NULL would say silently: the
-- constraint binds only rows that actually carry an identity. Two accounts that
-- carry none are never the same account, and the upsert must never merge them.
CREATE UNIQUE INDEX accounts_by_external_identity
    ON accounts (owner, provider, provider_account_id)
    WHERE provider IS NOT NULL AND provider_account_id IS NOT NULL;

-- Further identifiers for one account, each valid over an interval.
--
-- Two cards over one underlying account are one account with two aliases, so
-- its balance is counted once. A card that stopped working is an alias whose
-- interval closed — there is no binding lifecycle and no card entity, and
-- «expired», «reissued», «blocked» and «closed» are deliberately the same two
-- facts here.
CREATE TABLE account_aliases (
    owner      TEXT NOT NULL,
    account    TEXT NOT NULL,
    -- Opaque for the same reason `provider_account_id` is opaque.
    value      TEXT NOT NULL,
    -- The interval is half-open, as it is for instrument aliases: with an
    -- inclusive end, the day a card was replaced would belong to two records.
    valid_from TEXT NOT NULL,
    -- NULL is an open-ended interval, not an unknown end.
    valid_to   TEXT,
    -- `valid_from` is in the key: one value may reach the account, stop, and
    -- reach it again later, and that is two rows rather than an overwrite.
    PRIMARY KEY (owner, account, value, valid_from),
    -- An identifier is not an access right: an alias recorded against someone
    -- else's account is a statement about someone else's money (§14).
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;
