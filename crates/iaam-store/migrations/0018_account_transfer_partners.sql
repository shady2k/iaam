-- The owner's statement about which of his accounts money moves between.
--
-- One economic transfer between two banks is printed twice, once by each side,
-- and nothing in either row says the two are one movement. The system may not
-- decide that for him: a relationship it inferred would be a fabricated fact
-- about his money. So it is recorded here, as his statement, before any
-- statement is imported.
--
-- Two tables rather than one nullable column. «Money moves between this account
-- and none of my others» is a real answer and must be storable, and in a STRICT
-- table a nullable column inside the primary key is not an option — the same
-- constraint `raw_rows` met in 0002. So the statement and its content are
-- separated: a row in `account_transfer_statements` means the owner has ruled,
-- and the partners he named are the rows beside it in
-- `account_transfer_partners`. No statement row is the third state — awaiting
-- his decision — and it is the state a newly created account starts in.
CREATE TABLE account_transfer_statements (
    owner       TEXT NOT NULL,
    account     TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (owner, account),
    -- An identifier is not an access right: a statement recorded against
    -- someone else's account is a statement about someone else's money (§14).
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;

CREATE TABLE account_transfer_partners (
    owner   TEXT NOT NULL,
    account TEXT NOT NULL,
    -- Another of the owner's own accounts. A counterparty who is not the owner
    -- is not expressible here on purpose: this table answers «which two of my
    -- accounts are the two sides of one movement», and a third party is the
    -- classification rules' question, not this one.
    partner TEXT NOT NULL,
    PRIMARY KEY (owner, account, partner),
    FOREIGN KEY (owner, account) REFERENCES account_transfer_statements (owner, account)
        ON DELETE CASCADE,
    FOREIGN KEY (owner, partner) REFERENCES accounts (owner, id)
) STRICT;
