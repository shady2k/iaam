-- The names a document printed that the owner has said are not his accounts
-- (iaam-mk1n).
--
-- The state this closes is one the queue could not represent. A statement names
-- accounts that are not the owner's at all — another party's account, a person
-- he pays, an account belonging to somebody in his household. From his side
-- those records are already visible from the account whose statement they are
-- on, and the named account is one he will never create. The queue published
-- exactly one way to answer «a document printed a name your directory does not
-- place», and it was `create_account`; so «this name is not an account of mine»
-- was unrepresentable, and the only act that closed the item was one he had
-- decided against. Each such name therefore stood as permanent required work
-- against every report goal, and every report he asked for was flagged short on
-- account of a decision he had already made.
--
-- What each column is, and what it is not:
--
-- `printed` is the cell as a document printed it, matched verbatim against the
-- string `document_unresolved_accounts` recorded. It is not an account
-- identifier, because there is no account: it is the one string anybody here
-- knows about the thing the document named.
--
-- `reason` is his own sentence and it is required, for the reason
-- `account_scope_exclusions.reason` is required one table over: a name ruled
-- out without one is indistinguishable, a year later, from a name nobody ever
-- got round to. It costs more here than there, because the records printed under
-- this name stay refused on the strength of it.
--
-- NOTHING HERE IS A CONCLUSION ABOUT THE OWNER'S DIRECTORY, and this table is
-- beaten by it. A declaration says what he decided; it never says what is true
-- of his accounts now. Whether a printed name still names no account of his is
-- asked where the row is read, against the directory as it then stands, through
-- the one implementation of decision 0004's tiering — so an account created
-- afterwards that answers to the string removes the item outright, and this row
-- is not consulted while it does. That is the same argument the migration next
-- door makes for storing a transcription rather than a verdict, one noun away.
--
-- PER OWNER AND PER STRING, not per document and not per reading. The same
-- counterparty appears in next month's statement, printed the same way, and a
-- declaration keyed on the document it was first seen in would ask him about it
-- again every month. It is not keyed on the institution whose document printed
-- it either: the queue scopes an *identifier* per institution, because that is
-- what keeps two sources' short identifiers apart, while this is a statement
-- about his own directory — «no account of mine answers to this» — and that is
-- not a fact one source can hold a different answer to than another.
--
-- NO FOREIGN KEY TO A DOCUMENT OR A SESSION, deliberately. The statement
-- outlives any particular reading: a document may be retracted and read again,
-- and the name it prints is the same name. A key into the reading would tie a
-- decision about his money to the bookkeeping of one upload.
CREATE TABLE declined_account_names (
    owner       TEXT NOT NULL,
    printed     TEXT NOT NULL,
    reason      TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    -- One statement per name. Declaring a second time replaces the reason
    -- rather than accumulating verdicts: two sentences side by side with
    -- nothing saying which is current is the shape the reading's own record
    -- refuses one table over.
    PRIMARY KEY (owner, printed)
) STRICT;
