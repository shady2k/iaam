# A relational journal

Date: 2026-09-08 · Beads: `iaam-fg0e`, `iaam-l0dy`

Reviewed adversarially by codex twice (2026-09-08). Every defect it named that
could be checked against the code was checked, and the ones that held are folded
in below marked `(codex)`. Two were load-bearing: the transfer endpoints and the
category filter.

## 1. Why

The journal keeps each fact as one JSON document in `events.payload`, and lifts
into columns only the fields somebody wanted to search by. That was a defensible
event-store shape, and the reason is written in the header of
`0001_initial.sql`: the journal is immutable, decomposing it buys nothing for
writing, and it adds a way to lose a field when a variant is added.

What changed is how the store is actually used. SQL is increasingly required to
understand the *content* of a fact, and the schema answered that reactively:
`source_time` was lifted in `0013`, `import_session` in `0023`,
`settled_by_rule` in `0029` — each time somebody needed one more filter. What
remains inside the document is reachable only through `json_extract` and
`json_each`, which is why `journal_sql()` scans a JSON blob per row to answer
"which currency" and "which account does this touch", and why "which category"
and "which counterparty" could not be answered in the store at all
(`iaam-9xku`, `iaam-fg0e`).

The owner's ruling: for **this** schema, a document plus a handful of lifted
search keys is an antipattern, and the storage layer is to be normalised. The
system is greenfield — `dev.db` carries `user_version = 22`, 39 tables and not a
single row in any of them, and no other database exists — so nothing is migrated
and nothing stays for compatibility.

## 2. Decisions taken

| # | Decision |
|---|----------|
| D1 | SQLite stays. Postgres is not adopted. |
| D2 | The journal is normalised into typed tables. `events.payload` is removed, and no redundant copy of it is kept anywhere. |
| D3 | Detail tables are grouped **by family**, not one per leaf variant. |
| D4 | Provenance lives on `events` as columns, not in a 1:1 side table. |
| D5 | A value is stored once. Where the domain *defines* it as equal to a leg it is reconstructed from `event_legs`; where the domain merely relates it to one, it is stored. |
| D6 | Append-only is a property of the store's API. No triggers, no authorizer, no per-event tamper hash. Completeness of a written event is a runtime postcondition, not a schema constraint. |
| D7 | The 31 migration files collapse into a single `0001_schema.sql` with `SCHEMA_VERSION = 1`. The migration machinery itself is kept, unused until the first live database exists. |
| D8 | `Event::schema_version` is removed from the domain type. |
| D9 | The two rule vocabularies (`classification_rules`, `category_rules`) are normalised too. |
| D10 | Category assignment is a rebuildable projection table — not a predicate compiled from rules, and not a field on the fact. |

### D1 — SQLite stays

The workload is one owner, one writer, tens of thousands of facts, shipped as a
single binary with SQLite compiled in. The connection is deliberately unpooled,
WAL admits readers during a write, writes serialise through `BEGIN IMMEDIATE`,
and `foreign_keys` is already on (`iaam-store/src/lib.rs:231`). Projections load
the whole journal into Rust regardless (`projection::project`), so no query
planner sits on the critical path for the folds.

Table count is not an argument for Postgres; SQLite handles hundreds. Postgres
would be indicated by concurrent writers, a remote multi-user deployment, heavy
parallel analytics, or a volume at which loading the journal whole stops being
reasonable. None holds, and adopting it would cost the one property the system
is built around: a self-contained binary with embedded storage.

Three real losses, stated so they are not rediscovered:

- **No decimal type.** `Money` is already `PostedMinor` (an `i64` of minor
  units) plus a currency, and stores as `INTEGER` exactly. `Dec` wraps
  `rust_decimal::Decimal`, whose mantissa can exceed `i64`, so no universal
  scaled integer exists for it. It is stored as canonical `TEXT`. `REAL` is
  forbidden — it would silently round money-adjacent values.
- **No enum type.** Closed vocabularies become `CHECK (x IN (...))`, which
  duplicates the vocabulary between the Rust enum and the SQL text. A Postgres
  enum would have to be kept in step with the Rust enum too, so this is not an
  argument for changing database. §9's vocabulary test answers it.
- **No Unicode case folding.** SQLite's `NOCASE` and `lower()` fold ASCII only.
  The owner's rules match Russian text, and Rust's `to_lowercase` folds it. A
  case-insensitive comparison therefore cannot be pushed into SQL at all. This
  is why D10 exists. (codex)

### D6 — what enforces append-only, and what enforces completeness

Today two triggers forbid `UPDATE` and `DELETE` on `events`, on the stated
grounds that "code discipline does not survive the first data-repair script".
That was sound for a fact held in one row. For a fact spread over sixteen tables
it costs roughly forty-five schema objects, because guarding children against
`UPDATE` and `DELETE` is not enough — a late `INSERT` of one more leg into a
year-old event mutates it just as surely.

The owner's ruling is that defending the database file against manual external
editing is not this system's responsibility. Accepted, and it collapses that
layer: no triggers, no connection authorizer, no per-event tamper hash.

**Note the scope of "no triggers": the journal immutability triggers.** (codex)
Reference-data, contour-version and source-document triggers from the other
migrations survive the collapse untouched — §3 puts those subsystems out of
scope, and deleting their triggers would contradict it.

What holds append-only instead: **the store publishes no way to change a fact.**
There is one journal write and it inserts a whole event. Correction is a new
event, as it already is.

`SqliteStore::connection()` and `connection_mut()` are public unconditionally
(`lib.rs:242`) while no production code uses them — all 36 call sites outside
the store crate are tests. They move behind a test-only feature.

**Completeness is a different problem and needs a different answer.** (codex) A
single transaction handles every way a write can be *interrupted*: an SQL error
rolls back, an unwinding panic drops the `Transaction` whose default behaviour
is rollback, and `panic=abort`, a kill or a power cut leave SQLite to recover an
uncommitted WAL transaction. It does not handle code that returns `Ok` having
simply forgotten a child. A foreign key proves a child has its parent; nothing
in SQLite proves a parent has its required children, and there is no cross-table
`ASSERTION`. So the guarantee is a **runtime postcondition**, in §5.

## 3. Scope

In scope: the journal (`events` and everything the domain `Event` holds), the
two rule vocabularies, the category-assignment projection, and the collapse of
the migration files.

Out of scope, deliberately: market observations, schedules, import-session
tables, documents, tokens and broker access keep their current shape — they are
already relational. `source_documents.body`, `raw_rows.payload` and
`import_observations.payload` stay opaque: they hold source material exactly as
it arrived, and parsing it into columns would be this system deciding what the
source said. `snapshots.body` stays a CBOR blob — a rebuildable projection
cache, not a fact.

## 4. The journal schema

### 4.1 `events` — envelope, dates, provenance

```
id                      TEXT PRIMARY KEY
owner                   TEXT NOT NULL
account                 TEXT NOT NULL
kind                    TEXT NOT NULL      -- CHECK: the 17 discriminants
effective_date          TEXT NOT NULL
source_time             TEXT               -- EffectiveOrder::source_time
sequence                INTEGER NOT NULL
confidence              TEXT NOT NULL      -- known | estimated | unknown
relation_kind           TEXT NOT NULL      -- none | reversal | replacement
relation_target         TEXT
idempotency_key         TEXT
recorded_at             TEXT NOT NULL

-- EventDates, all optional
date_trade              TEXT
date_settled            TEXT
date_cash_posted        TEXT
date_entitlement        TEXT
date_paid               TEXT
tax_period_override     INTEGER            -- TaxPeriod(i32): a calendar year

-- Provenance
source                  TEXT NOT NULL
raw_hash                TEXT NOT NULL
parser_version          TEXT NOT NULL
source_operation_id     TEXT
source_category         TEXT
source_kind             TEXT
owner_category          TEXT
source_code             TEXT
source_description      TEXT
import                  TEXT
import_session          TEXT
declared_by             TEXT
rule_settlement         TEXT               -- NULL | no_rule | rule | answered_minting_rule
settled_by_rule         TEXT
settled_by_rule_version INTEGER
row_document            TEXT
row_sheet               TEXT
row_number              INTEGER
```

`tax_period_override` is `INTEGER`: `TaxPeriod` is `pub struct TaxPeriod(pub
i32)`, a calendar year rather than a date (`dates.rs:50`). (codex)

`source_description` rather than `description`: the column holds
`Provenance::description`, but that field carries either a description or a
printed counterparty, and the published vocabulary already settled on the wider
word (`iaam-b4xw`).

**Uniqueness** — note the account in the second one:

```sql
UNIQUE (owner, effective_date, sequence)
UNIQUE (owner, source, account, source_operation_id) WHERE source_operation_id IS NOT NULL
UNIQUE (owner, idempotency_key)                      WHERE idempotency_key    IS NOT NULL
```

The source-operation index has been account-scoped since migration `0012`.
Reverting it to `(owner, source, source_operation_id)` would break
`IdentityScope::Account`. (codex)

**Checks.** `rule_settlement` is nullable, so a two-sided equality is not
enough: a `CHECK` that evaluates to `NULL` passes in SQLite. (codex) The four
states are spelled out:

```sql
CHECK (
  (rule_settlement IS NULL AND settled_by_rule IS NULL AND settled_by_rule_version IS NULL)
  OR (rule_settlement = 'no_rule' AND settled_by_rule IS NULL AND settled_by_rule_version IS NULL)
  OR (rule_settlement IN ('rule','answered_minting_rule')
      AND settled_by_rule IS NOT NULL AND settled_by_rule_version IS NOT NULL)
)
CHECK ((relation_kind = 'none') = (relation_target IS NULL))
CHECK ((row_document IS NULL) = (row_number IS NULL))
CHECK (row_sheet IS NULL OR row_document IS NOT NULL)
CHECK (row_number IS NULL OR row_number >= 0)
```

`RowLocator.row` is `u64` and SQLite `INTEGER` is a signed `i64`. The existing
`RowNumberOutOfRange` rejection stays; this schema does not promise to
round-trip the whole `u64` range. (codex)

**Foreign keys.** Declared:

```sql
FOREIGN KEY (owner, account)         REFERENCES accounts (owner, id)
FOREIGN KEY (owner, relation_target) REFERENCES events (owner, id) DEFERRABLE INITIALLY DEFERRED
```

The relation key is owner-scoped, and deferred — not for sealing, which D6
removed, but because a bundle import inserts a graph and a replacement event can
precede its target. Deferring one key is simpler than topologically sorting the
import. (codex)

**Not declared, and why:** `import_session` and `settled_by_rule` look like
foreign keys and must not be. `Bundle` carries events, accounts and contours and
nothing else (`bundle.rs:46`), so restoring an archive into an empty database
would fail on both. They are archival provenance handles; a table holding a
similar-looking identifier does not make a reference valid. (codex) There is
likewise no registry for `SourceId`, `ImportId` or `PrincipalId`, and none is
invented to give a column the word `REFERENCES`.

### 4.2 `event_legs`

```
event      TEXT NOT NULL REFERENCES events (id)
ordinal    INTEGER NOT NULL CHECK (ordinal >= 0)
kind       TEXT NOT NULL   -- cash | security_quantity | principal | fee | tax
account    TEXT NOT NULL REFERENCES accounts (id)
custody    TEXT REFERENCES custody_places (id)
instrument TEXT REFERENCES instruments (id)
amount     INTEGER         -- PostedMinor
currency   TEXT
quantity   TEXT            -- Dec, canonical text
PRIMARY KEY (event, ordinal)
```

The shape of a leg follows from its kind, and one conditional check carries it.
`CHECK ((amount IS NULL) = (currency IS NULL))` alone would admit a security leg
carrying money and a cash leg carrying a custody place: (codex)

```sql
CHECK (
  CASE kind
    WHEN 'cash'              THEN amount IS NOT NULL AND currency IS NOT NULL
                              AND custody IS NULL AND instrument IS NULL AND quantity IS NULL
    WHEN 'fee'               THEN amount IS NOT NULL AND currency IS NOT NULL
                              AND custody IS NULL AND instrument IS NULL AND quantity IS NULL
    WHEN 'tax'               THEN amount IS NOT NULL AND currency IS NOT NULL
                              AND custody IS NULL AND instrument IS NULL AND quantity IS NULL
    WHEN 'principal'         THEN amount IS NOT NULL AND currency IS NOT NULL
                              AND instrument IS NOT NULL AND custody IS NULL AND quantity IS NULL
    WHEN 'security_quantity' THEN instrument IS NOT NULL AND custody IS NOT NULL
                              AND quantity IS NOT NULL AND amount IS NULL AND currency IS NULL
  END
)
```

How *many* legs a kind requires stays a runtime invariant: `validate_structure`
already answers it and SQL cannot.

**Owner scoping on children is application-level, and the spec says so rather
than implying a constraint it does not have.** (codex) A composite
`(owner, account)` foreign key would need an `owner` column on every child
table, and duplicating it across sixteen tables for tenant isolation in a
single-owner system is not worth it. The identifiers are globally unique UUIDs;
the write path checks that every account and custody place named by a leg
belongs to the event's owner.

`ordinal` fixes a stable order for reconstruction and **carries no meaning**. No
reader may find a leg by `ordinal = 0`; legs are located by kind and role.

### 4.3 Detail tables, one per family

Only variants carrying something beyond the legs get a table. Five need none —
`CashIn`, `CashOut`, `Refund`, `OpeningCash` and `OwnAccountMovement`: their
whole content is `events.kind` plus the legs. (codex)

| Table | Carries |
|---|---|
| `event_trade` | side, instrument, quantity, gross, fee?, basis_fee?, basis_fee_exact?, accrued_interest? |
| `event_cash_transfer` | transfer_id, **from_account, to_account** |
| `event_unresolved_movement` | amount + currency; this variant has no leg by design |
| `event_income` | instrument?, income_kind? — `gross` is the cash leg |
| `event_fee` | origin |
| `event_tax` | origin |
| `event_opening_position` | instrument, quantity, cost_basis?, and `OpeningAssertions` as nine columns |
| `event_valuation` | instrument, price (TEXT), currency, quality — no legs |
| `event_control_assertion` | period, claim_kind, and the claim's fields as conditional columns |
| `event_coverage_gap` | period only |
| `event_corporate_action` | action_kind and the action's fields as conditional columns |
| `event_offer_exercise` | action_kind and the action's fields as conditional columns |

Each keys on `event TEXT PRIMARY KEY REFERENCES events (id)`.

**`event_cash_transfer` stores both endpoints, and the first draft was wrong to
drop them.** (codex) Both legs of a transfer are `LegKind::Cash`, they carry no
role, and `ordinal` is declared meaningless — so which leg is `from` and which
is `to` is not recoverable. `validate_transfer` (`event/mod.rs:545`) *finds* the
legs by matching `leg.account` against the stored `from` and `to`: the endpoints
are the input to that search, not its output.

**Nested enums become one table per family with a discriminant and conditional
`CHECK`s, not a table per leaf.** (codex) `ControlClaim` has six variants,
`CorporateAction` three, `OfferExerciseAction` three, and variants within a
family share most of their fields. The conditional constraints carry the typing:

```sql
CHECK (action_kind IN ('offer_submitted','offer_cancelled','offer_settled'))
CHECK ((action_kind = 'offer_submitted') = (window IS NOT NULL))
CHECK ((action_kind = 'offer_settled')   = (gross IS NOT NULL))
CHECK ((action_kind = 'offer_settled')   = (custody IS NOT NULL))
```

**`BasisAllocation` inside a partial redemption needs four columns, not a flag.**
(codex) `Known` carries `share` *and* an `AllocationEvidence { inputs_hash,
knowledge_as_of, algorithm_version }` (`allocation.rs:106`); dropping the
evidence loses the audit of how the share was computed:

```
allocation_kind        TEXT     -- unknown | known
allocation_gap         TEXT     -- unknown only
allocation_share       TEXT     -- known only
allocation_inputs_hash TEXT     -- known only
allocation_known_as_of TEXT     -- known only
allocation_algorithm   TEXT     -- known only

CHECK ((allocation_kind = 'unknown') = (allocation_gap IS NOT NULL))
CHECK ((allocation_kind = 'known')   = (allocation_share IS NOT NULL))
CHECK ((allocation_kind = 'known')   = (allocation_inputs_hash IS NOT NULL))
CHECK ((allocation_kind = 'known')   = (allocation_known_as_of IS NOT NULL))
CHECK ((allocation_kind = 'known')   = (allocation_algorithm IS NOT NULL))
```

`OpeningAssertions` is nine scalar fields (`kind.rs:67`) and needs no child
table. **No constraint is added between them that the domain does not have**
(codex) — the Rust type permits `acquisition_date` to be set with certainty
`Unknown`, and `basis_currency` without `basis_rate`. If those combinations are
wrong, the domain and `validate_structure` change first; SQL does not quietly
become stricter than the type it stores. `cost_basis` is an amount/currency pair
with a both-or-neither check.

### 4.4 Collections get child tables

```
event_coverage_gap_rows            (event, ordinal, source,
                                    row_name_kind, row_name_value)   PK (event, ordinal)
event_coverage_gap_row_dimensions  (event, ordinal, dimension)       PK all three
```

`row_name_kind` is `given | fingerprint`: `SourceRowKey` distinguishes a name
the source gave from a fingerprint we computed, and the two are different rows
even when the text matches.

**`refused` and the top-level `dimensions` set are not stored.** (codex)
`validate_import_coverage_gap` requires `rows.len() == refused`
(`event/mod.rs:1002`) and the top-level set to equal the union of the rows' sets
(`:1013`). Storing them would be one value in three places, and with no triggers
the schema could not hold either cross-table invariant. They are a count and a
union over `event_coverage_gap_rows`. The legacy escape for facts written before
schema 8 disappears with D8, so both invariants are unconditional.

That is **sixteen journal tables**: `events`, `event_legs`, the twelve detail
tables of §4.3, and the two collection tables above.

### 4.5 D5 in detail — reconstructed against stored

The criterion: a value is reconstructed when the domain **defines** it as equal
to a leg — `validate_structure` builds the expected leg out of it, or ends in
`require_equal` against it — because storing it twice creates two places that
can disagree. Everything else is stored.

Reconstructed:

- `CashIn.amount`, `CashOut.amount`, `Refund.amount`, `OpeningCash.amount`,
  `OwnAccountMovement.amount`, `Income.gross`, `Fee.amount`, `Tax.amount` —
  each ends in `require_equal(name, money, declared)`;
- `CashTransfer.amount` — the positive `to` leg, once §4.3 stores which leg that
  is;
- **`CorporateAction` compensation, all three variants.** (codex) The first
  draft called these unrecoverable and that was wrong:
  `validate_corporate_action` calls `expect_legs(&[principal_leg(account,
  instrument, *compensation)])` — the expected leg is *built from* the
  compensation and matched exactly (`event/mod.rs:700`).

Stored:

- `Trade.gross`, `fee`, `accrued_interest`, `basis_fee`, `basis_fee_exact` —
  gross is not recoverable from the settlement leg without unwinding the fee and
  the accrued interest;
- `Valuation.price` — the variant posts no leg;
- `UnresolvedOwnAccountMovement.amount` — no leg by design, which is the point
  of the variant;
- `CorporateAction.principal_returned_per_unit` — per-unit, no leg;
- control-claim amounts — assertions about the world, not postings;
- offer-settlement figures and `cost_basis`.

Reconstruction goes through one set of typed helpers —
`single_money_leg(kind, expected_leg_kind)` and siblings — never an open-coded
leg search repeated per variant.

### 4.6 The rule vocabularies

`RuleMatcher` is **not** an enum: seven independent optional conditions joined
by AND (`classification.rs:200`). Seven nullable columns plus `asks_nothing`
expressed as a constraint:

```sql
CHECK (counterparty_account IS NOT NULL OR description_contains IS NOT NULL
    OR source_kind IS NOT NULL OR source_category IS NOT NULL
    OR owner_category IS NOT NULL OR source_code IS NOT NULL
    OR movement IS NOT NULL)
```

`Classification` is a tagged outcome of six variants: `outcome_kind` plus
conditional `to_account`, `fee_origin`, `income_kind`.

`CategoryMatcher` is a genuine enum of four: `matcher_kind`, `value`, `text`,
`description_mode`, with a conditional `CHECK` per kind.

No table hierarchy for rules.

### 4.7 `event_category_assignments` — a projection, not a fact

```
owner          TEXT NOT NULL
event          TEXT NOT NULL REFERENCES events (id)
category       TEXT NOT NULL REFERENCES categories (id)
rule           TEXT NOT NULL REFERENCES category_rules (id)
basis          TEXT NOT NULL   -- row | source_category | description
rules_revision INTEGER NOT NULL
PRIMARY KEY (owner, event)
```

**This is a read model and it is stated as one.** It is derived from the fact
and the owner's active rules by `iaam_core::category::assign`, it is rebuilt
rather than repaired, and no reader treats it as evidence: a category is still
not a field on an event, and renaming, splitting or retiring a category still
touches reference data only (`category.rs:1`). A row absent from this table
means `NotDecomposed`, which is the honest answer and never a silent bucket.

**Why a projection and not a predicate compiled from the rules.** (codex) The
first draft promised to compile the active rule set into SQL, and two things
defeat that:

- `assign` (`category.rs:196`) is a priority ladder — `Row`, then
  `SourceCategory`, then `Description`, and within a class the longest matcher
  text, then version, then id. A row matched by a description rule of category X
  can be outranked by a row rule of category Y, so a predicate for X is not a
  disjunction over X's rules; it needs "and no higher-priority rule matches",
  which is quadratic in the rule set and rebuilt on every query.
- `Description` matching is case-insensitive through Rust's Unicode
  `to_lowercase`. SQLite's `NOCASE` and `lower()` fold ASCII only, so for the
  Russian text these rules actually match, the comparison cannot be expressed in
  SQL at all. A `Contains` matcher would additionally defeat any B-tree index.

`rules_revision` is the highest active `category_rules.version` the projection
was built from. It is checked on read; a mismatch means the projection is stale
and it is rebuilt before the query is answered, so a filter can never quietly
answer from an old rule set. A rebuild is a full recompute over the journal by
the same core function that computes a category anywhere else — there is exactly
one implementation of the ladder, and this table is one of its callers.

Rebuild points: any create, edit or retirement of a category rule, and any
append to the journal (incremental, for the appended events).

## 5. Write path

**One private primitive, and every caller goes through it.** (codex) There are
three write paths today — `append_event` (`events.rs:75`),
`append_event_in_order` (`events.rs:97`) and a direct `insert_event` from
`bundle.rs:294` — and after normalisation a second, subtly different insert
sequence is a way to write half a fact. The primitive takes an **already open**
transaction, so the bundle importer, which holds one transaction across many
events, does not open a nested one; `append_event` becomes transactional and
therefore takes `&mut self`.

Inside one transaction, per event:

```text
validate_structure(event)                -- domain, before anything is written
BEGIN IMMEDIATE                          -- or the caller's open transaction
  deduplication check, sequence assignment
  insert header, legs, family detail, collection rows   -- order is free
  read the event back, inside the transaction
  assert reconstructed == event
COMMIT
```

**The read-back is a postcondition on our own writer**, not a tamper check
(§D6). If a mapper forgot a detail row, a second leg or an optional field, the
reconstruction either fails or compares unequal and the transaction rolls back —
the only way to catch the class of bug no foreign key can, since SQLite proves a
child has its parent and never that a parent has its children.

`append_events` is **already** not atomic as a batch: the adapter loops over
`append_event_in_order`, each with its own transaction (`adapters/sqlite.rs:381`
onwards). Normalisation makes each event atomic and does not change the batch's
semantics. Whether an import commit should be all-or-nothing across its events
is a separate decision, filed rather than smuggled in here.

Fault injection is part of the work, not a nicety: an induced error at each Nth
insert, a panic between header and legs and between legs and detail, and a
foreign-key failure just before commit. After each, no row of that event exists
in any table.

## 6. Read path

**A single join with `LIMIT` is wrong** and would be the first bug: legs
multiply the rows, so a page boundary cuts an event in half. Reads are two-phase
inside one read transaction:

1. Select the page of event ids from `events`, filters as ordinary predicates
   and `EXISTS` sub-selects, ordered by `(effective_date, sequence)` with the
   existing keyset cursor.
2. Bulk-load headers, legs and the needed family details for exactly those ids —
   a fixed number of statements regardless of page size, never `N+1`.
3. Reassemble `Event` values in Rust, in the order phase 1 returned.

### 6.1 The two filters this work exists for

`JournalParams` and the aggregate query both gain two fields with the same
meaning in each — that pairing is the whole design of the two routes:

- **`category`** — joins `event_category_assignments` (§4.7). `uncategorised` is
  the absence of a row, expressed as `NOT EXISTS`.
- **`counterparty`** — an `AccountId`, the far endpoint of a movement whose both
  ends are known:

  ```sql
  EXISTS (SELECT 1 FROM event_cash_transfer t WHERE t.event = e.id AND (
      (e.account = t.from_account AND t.to_account   = ?)
   OR (e.account = t.to_account   AND t.from_account = ?)))
  ```

  It is deliberately narrower than `touching`, which is symmetric and also
  matches rows where the named account is the near side. A far side that is the
  owner's but unnamed, and one outside the perimeter, are separate `kind`s and
  are answered by the existing kind filter.

### 6.2 Indexes

Every keyset filter carries the ordering columns, so one index answers the
predicate and the page order together rather than stopping a column short:
(codex)

```sql
events (owner, effective_date, sequence)                     -- unique, kept
events (owner, account, effective_date, sequence)
events (owner, kind, effective_date, sequence)
events (owner, source, effective_date, sequence)
events (owner, import, effective_date, sequence)           WHERE import          IS NOT NULL
events (owner, import_session, effective_date, sequence)   WHERE import_session  IS NOT NULL
events (owner, settled_by_rule, effective_date, sequence)  WHERE settled_by_rule IS NOT NULL
events (owner, source_category, effective_date, sequence)  WHERE source_category IS NOT NULL
events (owner, relation_target)                            WHERE relation_target IS NOT NULL

event_legs (event, ordinal)                                  -- the primary key
event_legs (account, event)
event_legs (currency, event)                               WHERE currency IS NOT NULL

event_cash_transfer (from_account, event)
event_cash_transfer (to_account, event)

event_category_assignments (owner, category, event)
```

`event_legs(account, event)` answers `touching` and does **not** answer "far
side" — that is what the two `event_cash_transfer` indexes are for. (codex)

A `Description` `contains` search is not answerable by a B-tree and this spec
does not pretend otherwise: it stays a scan, reached only when a caller filters
on `source_description` directly. Rule-driven description matching never reaches
SQL at all (§4.7).

Query plans are pinned by `EXPLAIN QUERY PLAN` tests, as the correction chain
already does.

## 7. What is removed

- `events.payload`, with no copy kept anywhere. A relational table *and* a
  canonical document is the worst of both: four ways for the two to disagree and
  no rule for which wins.
- The two journal immutability triggers — and only those (§D6).
- 30 of the 31 migration files. The runner in `schema.rs` is untouched;
  `MIGRATIONS` becomes one entry and `SCHEMA_VERSION` becomes 1.
- `Event::schema_version` and `iaam_core::event::SCHEMA_VERSION` (currently 16).

### 7.1 The reach of D8

Removing the version field is not a rename, and it touches more than
constructors: (codex)

- the legacy branch in `validate_import_coverage_gap` that returns `Ok(())` for
  facts written before schema 8 (`event/mod.rs:993`);
- `SOURCE_CATEGORY_IS_A_CATEGORY_FROM` and the version gates in classification;
- the `unvouched_word` version gate in the import-session scenario;
- the ingest check on a submitted event's `schema_version`;
- `schema_version` in the server DTO and the OpenAPI document
  (`iaam-server/src/dto.rs:6362`), and the route that stamps the current version;
- tests that construct old event shapes, and the serde defaults and comments
  that exist for them.

**Three version numbers are independent and only one of them dies.** (codex)
`Bundle.schema_version` is the *store* schema version and becomes 1 after the
collapse; `BUNDLE_VERSION` is the bundle format's own; `Event::schema_version`
is the one removed.

The snapshot digest domain tag moves from `iaam/journal-prefix/v2` to `v3`: the
serialisable shape of `Event` changes, and two different protocols should not
share a namespace even in a greenfield. (codex)

`dev.db` reports `user_version = 22` and will be rejected as newer than this
build on first open. It holds no rows; it is deleted and recreated.

## 8. A domain defect found on the way

`validate_transfer` (`event/mod.rs:545`) checks `from != to`, that there are
exactly two cash legs, that the residual is zero, and that the declared amount
equals the `to` leg. **It does not check signs.** (codex) An event with
`from = A`, `to = B`, legs `A: +100` and `B: −100` and `amount = −100` passes
validation today: the residual is zero and the declared amount does equal the
`to` leg.

Since §4.3 now stores the endpoints as semantic roles, the validator is
tightened alongside: the `from` leg is negative, the `to` leg is positive, the
declared amount is positive and equals the `to` leg, the two currencies match,
and the event's own account is one of the two. This is a domain change with its
own tests, and it is called out here rather than folded silently into a storage
change.

## 9. Testing

- **Round-trip per variant.** Not one example each: every nested subvariant,
  every `Option` combination, `i64` extrema, decimal scale, and empty against
  non-empty collections.
- **No wildcard destructuring in the persistence mappers.** An exhaustive
  `match` on `EventKind` makes a new *variant* break the build, but `..` in the
  pattern lets a new *field* pass silently — the failure mode `0001` warned
  about. Mappers destructure every field by name; no default-on-read for a
  required field.
- **Fault injection on the write path** (§5).
- **Digest stability.** `projection::state` hashes CBOR of the whole `Event`
  into the snapshot prefix digest (`state.rs:240`). A decimal that changes
  representation on its way through `TEXT` — `1.0` becoming `1` — would change
  the digest. Test `encode(load(insert(event))) == encode(event)`.
- **Vocabulary agreement.** A test compares every SQL `CHECK (x IN (...))` list
  against the Rust enum it mirrors, so the two cannot drift.
- **Projection agreement.** For a generated journal and rule set, every row of
  `event_category_assignments` equals `category::assign` computed directly, and
  a rule edit followed by a rebuild leaves no stale row.
- **Query plans.** `EXPLAIN QUERY PLAN` assertions for the journal listing and
  each filter.

## 10. Follow-ups, filed rather than folded in

- Six of nineteen scenarios import types from `iaam_store` directly
  (`PriceRow`, `SeriesKey`, `MarketWindow`, `IssueTermsRow`) — market and
  schedule reads bypass the ports. Not SQL leaking, but DTOs in the wrong crate.
- Whether an import commit should be all-or-nothing across its events (§5).
- `validate_structure` is not uniformly strict about foreign legs:
  `expect_single_cash` counts only cash legs, and the transfer test admits an
  extra `Principal` leg. Whether that is deliberate extensibility or a gap in
  the validator must be decided before any SQL is written that is stricter than
  the domain. (codex)
- A counterparty identity for parties that are *not* the owner's own accounts
  stays out of scope and unbuilt. `iaam-fg0e`'s counterparty filter answers "the
  far side is this account of mine", which is the only counterparty this system
  can honestly claim to know.
