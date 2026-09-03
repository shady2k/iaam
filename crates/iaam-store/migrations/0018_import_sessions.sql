-- An import session is PRE-JOURNAL state, and these three tables are where it
-- lives. Nothing here is an event and nothing here is in `events`: the journal
-- keeps its property that everything in it is a fact somebody asserted, and a
-- session keeps the opposite one — everything in it is still provisional.
--
-- The owner's concern this answers was "open a transaction, import, check, then
-- commit". It is deliberately not a database transaction: an import needs the
-- owner to answer questions between steps, and that takes hours or days. A
-- connection held open across that blocks every other writer and does not
-- survive a restart. Durable application state does both.
CREATE TABLE import_sessions (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    -- open | committed | abandoned. A session leaves `open` once and never
    -- returns: committing writes facts that cannot be unwritten, and abandoning
    -- is the owner saying the rows were wrong.
    state      TEXT NOT NULL,
    -- The source the batch declared, when it declared one. Nullable because a
    -- caller may submit without declaring, and a session minted for such a batch
    -- is addressable by its own identifier and nothing else.
    source     TEXT,
    -- The import the declaration named, when it named a label. This is what
    -- makes a second submission of the same import reach the same session
    -- instead of opening a parallel one holding half the answers.
    import     TEXT,
    opened_at  TEXT NOT NULL,
    closed_at  TEXT
) STRICT;

CREATE INDEX import_sessions_by_owner ON import_sessions (owner, state);

-- One open session per declared import. Two would split one statement's
-- questions across two places, and the owner would answer one of them.
CREATE UNIQUE INDEX import_sessions_by_import
    ON import_sessions (owner, import)
    WHERE import IS NOT NULL AND state = 'open';

-- One row per submitted line, in submission order, conclusive or observed.
--
-- `payload` is opaque JSON to this crate, exactly as a classification rule's
-- matcher is: the store keeps it and the application reads it. It validates only
-- that it parses, because a payload the application cannot read must not be
-- written silently.
CREATE TABLE import_observations (
    session   TEXT NOT NULL REFERENCES import_sessions (id) ON DELETE CASCADE,
    row       INTEGER NOT NULL,
    -- The stable key the caller's row identity yields, when it yields one.
    -- Without it a re-submitted row opens a second question about the same
    -- money, and the owner answers one of the two.
    row_key   TEXT,
    -- 1 when the caller submitted a conclusion, 0 when it submitted an
    -- observation. Both belong in one session: the seam that relates the two
    -- legs of a cross-bank transfer needs to see them together.
    concluded INTEGER NOT NULL,
    payload   TEXT NOT NULL,
    -- The owner's answer, once given. Kept on the observation as well as on the
    -- question because the observation is what commit reads.
    answer    TEXT,
    PRIMARY KEY (session, row)
) STRICT;

CREATE UNIQUE INDEX import_observations_by_key
    ON import_observations (session, row_key)
    WHERE row_key IS NOT NULL;

-- A question is a durable resource, not a sentence in a response body.
--
-- It has an identifier because it outlives the response that carried it: the
-- answer arrives in a later request, possibly days later, and must name the
-- question it answers. `alternatives` is stored beside it so the answer can be
-- checked against what was actually asked.
CREATE TABLE import_questions (
    id           TEXT PRIMARY KEY,
    session      TEXT NOT NULL REFERENCES import_sessions (id) ON DELETE CASCADE,
    row          INTEGER NOT NULL,
    question     TEXT NOT NULL,
    alternatives TEXT NOT NULL,
    -- The sentence put to the owner, rendered where account titles are known.
    prompt       TEXT NOT NULL,
    asked_at     TEXT NOT NULL,
    answered_at  TEXT,
    answer       TEXT,
    -- The classification rule the answer created, so the same counterparty is
    -- not asked about twice.
    rule         TEXT
) STRICT;

CREATE UNIQUE INDEX import_questions_by_row ON import_questions (session, row);
CREATE INDEX import_questions_open ON import_questions (session, answered_at);
