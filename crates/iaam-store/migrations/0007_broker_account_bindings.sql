-- The binding between one of the owner's accounts and the broker's own
-- account number for it (iaam-xzz5.3.2).
--
-- A broker sync used to send the account's identifier in *this* system — the
-- UUID of the owner's iaam account — to the broker as the account to read.
-- No live broker recognises that string; the sync only ever worked against a
-- double. What the broker asks for is its own account number, and the pair
-- between the two is the owner's fact: he knows which of his iaam accounts
-- is which of the broker's accounts, and this table is where that word is
-- kept. A sync resolves the number here before it requests anything.
--
-- **A current statement, not a history**, the shape `contour_report_defaults`
-- uses: presence is the whole of the statement, and a rebinding replaces the
-- number rather than standing beside the old one. There is no withdrawn flag
-- and no interval — the owner has bound the account or he has not, and two
-- ways of spelling one state is how they disagree.
--
-- **One binding per (account, broker).** The primary key says an iaam account
-- carries at most one broker number per broker. It does not say the same
-- number names one account: that direction is the unique index, and it is the
-- owner's guarantee that a sync reading the broker's number cannot file its
-- rows onto two of his accounts. Binding the same broker number to a second
-- iaam account is refused by the store, never re-pointed silently.
--
-- **The number is opaque.** It is whatever the broker prints — T-Invest UUIDs,
-- Finam's own identifiers — and this schema checks nothing about its shape
-- beyond non-emptiness. An empty number is indistinguishable from "we don't
-- know", and the owner's word is never a placeholder (§4.9).
--
-- **Why the triggers, and not a foreign key.** The binding names an account
-- that must exist and belong to the same owner, and `accounts.id` is a plain
-- TEXT primary key that carries no owner — so a foreign key on
-- `(account)` alone would accept another owner's account, and there is no
-- key on `(owner, account)` to point at instead. The two triggers make the
-- same promise the composite key would have: a binding names an account this
-- owner holds, on both the insert and the restatement path. Unlike a contour,
-- an account is a real row that cannot be deleted behind the journal's own
-- triggers, so the delete half of a foreign key's promise has nothing to
-- guard here either.
CREATE TABLE broker_account_bindings (
    owner          TEXT NOT NULL,
    account        TEXT NOT NULL,
    broker         TEXT NOT NULL,
    broker_account TEXT NOT NULL CHECK (length(broker_account) > 0),
    recorded_at    TEXT NOT NULL,
    PRIMARY KEY (owner, account, broker)
) STRICT;

CREATE UNIQUE INDEX broker_account_bindings_one_iaam_account_per_broker_account
    ON broker_account_bindings (owner, broker, broker_account);

CREATE TRIGGER broker_account_bindings_name_a_held_account
BEFORE INSERT ON broker_account_bindings
WHEN NOT EXISTS (
    SELECT 1 FROM accounts
    WHERE owner = NEW.owner AND id = NEW.account
)
BEGIN
    SELECT RAISE(ABORT, 'a broker account binding must name an account this owner holds');
END;

-- The same statement for the restatement path. A binding is replaced rather
-- than added to, so the second way a row with an account in it can arrive is
-- an `UPDATE`, and a trigger on `INSERT` alone would let the one route that
-- restates the binding write an account that does not exist. SQLite allows
-- one event per trigger, which is why this is two triggers and not one.
CREATE TRIGGER broker_account_bindings_stay_on_a_held_account
BEFORE UPDATE ON broker_account_bindings
WHEN NOT EXISTS (
    SELECT 1 FROM accounts
    WHERE owner = NEW.owner AND id = NEW.account
)
BEGIN
    SELECT RAISE(ABORT, 'a broker account binding must name an account this owner holds');
END;
