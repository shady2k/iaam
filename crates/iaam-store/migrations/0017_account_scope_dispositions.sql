-- An account's scope disposition: the owner's statement that it sits outside
-- every contour on purpose.
--
-- Membership *inside* a contour is not stored here. It is already a fact of
-- `contour_accounts`, versioned there, and a second copy of it would be a second
-- truth to keep in step. What no contour can hold is the opposite statement:
-- «outside every contour, deliberately» belongs to no single contour's
-- composition, so it is recorded once per owner and account.
--
-- The absence of a row is the third state — awaiting the owner's decision — and
-- it is the state a newly created account starts in.
CREATE TABLE account_scope_exclusions (
    owner       TEXT NOT NULL,
    account     TEXT NOT NULL,
    -- Free text from the owner. The reason is required by the API, not by the
    -- schema: a rule the database enforced would still be satisfied by a space.
    reason      TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (owner, account),
    -- The same foreign key the contour composition carries, for the same
    -- reason: an identifier is not an access right, and a disposition recorded
    -- against someone else's account is a statement about someone else's money.
    FOREIGN KEY (owner, account) REFERENCES accounts (owner, id)
) STRICT;
