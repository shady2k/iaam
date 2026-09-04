-- The account names a reading of a document could not place (iaam-x9ls).
--
-- An INSTANCE fact, and deliberately not a journal one. Nothing was recorded
-- when these names were read: every record that printed one was refused, so no
-- event exists to carry the name in its provenance, and inventing one would
-- record a movement nobody read. What happened is that this instance was handed
-- a document, read it against the owner's directory, and could not say which
-- account seven of its strings meant. That is a fact about a reading, it is
-- true, and it is the only place the names survive — the refused records never
-- reached the session, and re-reading every kept document to recover them is a
-- fold this system refuses to put on the queue's path.
--
-- What each column is, and what it is not:
--
-- `printed` is the cell as the document printed it, trimmed and otherwise
-- verbatim. It is not a title, not a proposal for one, and not an identifier
-- this instance minted; it is the string the source used, which is the only
-- thing anybody here knows about that account.
--
-- `records` is arithmetic over one reading: how many of that document's records
-- printed that string in the column the profile names as the account. It is not
-- a count of movements — those records were refused, so nothing was read out of
-- them — and it must not be summed with a second reading of the same document,
-- which is why a re-reading replaces this document's rows rather than adding to
-- them.
--
-- `ordinal` is the position the name was first printed at among the names of
-- this reading, so the listing can be given back in the order the document
-- printed rather than in whatever order a key sorts in.
--
-- `import_session` is the session the document was read into. Kept because the
-- session's assessment publishes these names — the section that answers «which
-- accounts did these rows name» could not otherwise answer it for the one case
-- where the answer matters — and because it says which reading this row came
-- out of.
--
-- NOTHING HERE IS A CONCLUSION ABOUT THE OWNER'S DIRECTORY. Whether a name is
-- still unresolved is decided when the row is read back, against the directory
-- as it then stands, by the one implementation of that tiering. A row that says
-- «this document printed this string» stays true after he creates the account;
-- a row that said «this account is missing» would quietly become a lie, and the
-- queue built on it would publish work already done.
CREATE TABLE document_unresolved_accounts (
    owner          TEXT NOT NULL,
    -- SHA-256 of the document, in hexadecimal: the same name `source_documents`
    -- and every derived row key carry it under. The document rather than its
    -- upload identifier, because that is what a re-reading is addressed by.
    document_hash  TEXT NOT NULL,
    printed        TEXT NOT NULL,
    ordinal        INTEGER NOT NULL,
    records        INTEGER NOT NULL,
    import_session TEXT NOT NULL,
    recorded_at    TEXT NOT NULL,
    PRIMARY KEY (owner, document_hash, printed),
    -- The same foreign key every other per-owner declaration carries: a row
    -- recorded against another owner's session is a statement about another
    -- owner's document.
    FOREIGN KEY (import_session) REFERENCES import_sessions (id)
) STRICT;

-- The queue folds this per owner on every reading of the frontier, and the
-- assessment reads one session's share of it. Without the index both are a scan
-- of every name every document of every owner ever printed.
CREATE INDEX document_unresolved_accounts_by_owner
    ON document_unresolved_accounts (owner, document_hash, ordinal);
