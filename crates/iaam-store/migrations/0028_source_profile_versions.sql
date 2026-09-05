-- The content each source profile version names (iaam-mr25).
--
-- Decision 0019 §5: «An instance records the digest of each profile it loads
-- and refuses to load a different content under an `(id, version)` it already
-- recorded. Without that, "the rows version 3 read" is not a set, and
-- constraint 6 — the facts a buggy plugin wrote are findable and retractable —
-- is not true.» This table is that record, and until it existed the binding was
-- a sentence in a document: the catalogue compared two files loaded in one
-- pass, so a profile edited between two starts was accepted under the version
-- it had already used, and the rows of that version stopped being one set with
-- nothing anywhere saying so.
--
-- AN INSTANCE FACT, AND DELIBERATELY NOT AN OWNER'S. Every other declaration
-- here is keyed on the owner; this one is not, and the reason is written on
-- `ProfileCatalogue` itself — the catalogue is a property of the deployment and
-- not of the journal, because two instances of one image must read one
-- institution's export the same way. A per-owner binding would let one owner's
-- history admit a content another's history refuses, which is a reading that
-- depends on who uploaded what and when.
--
-- What each column is, and what it is not:
--
-- `id` and `version` are the profile's own, the pair that becomes
-- `ParserVersion("profile/<id>/<version>")` on every fact the profile reads.
-- They are the key because they are what a retraction is addressed by: «the
-- rows version 3 read» has to be a query, and it is one only while the pair
-- names one content.
--
-- `digest` is SHA-256 of the profile file, in hexadecimal — what
-- `SourceProfile::digest` computed. It is recorded here rather than folded into
-- the parser version string for the reason that doc comment gives: a digest
-- inside the string would demand a new `SettlementLagPolicy::with_profile` band
-- for every byte changed, including changes that touch no date.
--
-- `first_loaded_at` is when this instance first saw the pair. It is not an
-- audit trail of loads — a start that finds the same content writes nothing at
-- all — it is there so an operator reading a refusal can tell how long the
-- content he is being refused has been the one this instance reads.
--
-- WRITTEN ONCE AND NEVER UPDATED. There is no `ON CONFLICT DO UPDATE` anywhere
-- against this table, and that is the whole mechanism: the row that stands is
-- the one that got there first, so a load that disagrees with it is refused
-- rather than quietly winning. Rewriting the digest under a standing pair would
-- be the defect this table exists to catch, performed by the table itself.
CREATE TABLE source_profile_versions (
    id              TEXT NOT NULL,
    version         INTEGER NOT NULL,
    digest          TEXT NOT NULL,
    first_loaded_at TEXT NOT NULL,
    PRIMARY KEY (id, version)
) STRICT;
