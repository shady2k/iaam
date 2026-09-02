# E4: a coverage gap names its rows, and a row is disposed of explicitly

Bead: iaam-szl3 (design). Epic: iaam-evc2. Defect: iaam-dvki.

Supersedes the decision of 2026-08-31 ("attempt identity in provenance"), and
supersedes this document's own first version of 2026-09-01, which inferred
repair from the presence of an event carrying the row's identifier. Both are
recorded below with the reason each was rejected, because both are the obvious
first answer and will be proposed again otherwise.

## The defect

`reconciliation::tainted_dimensions` correlates an `ImportCoverageGap` with an
assertion group by `(account, period, source, parser_version)`. Nothing in that
tuple says *what* is missing, so the gap applies to every assertion group
sharing the channel, for ever:

- a later clean synchronisation through the same access and parser stays
  tainted;
- a refused retry retroactively withholds the evidence of an earlier clean run;
- re-running the import cleanly cannot lift the gap even in principle.

`iaam-en85` recovered the parser-corrected case by putting the parser version
into the assertion idempotency key. What remains is recovery when the parser
version does not change — which is not an edge case. The broker operation-kind
dictionary lives in the database (`broker_operation_kinds`, migration 0009,
epic iaam-d8b.2.2) precisely so that extending it needs no release, and an
unknown code is refused. The system therefore routinely fixes an import
**without** changing `parser_version`, and today it cannot notice.

## Requirements

Fixed by the owner on 2026-09-01. Not renegotiable inside this epic.

- **R1.** A partial import is accepted — the facts we could read are recorded.
  Rejecting the whole response is out of bounds: that was the defect epic
  iaam-40vm / bug iaam-dbvu removed.
- **R2.** Re-running an import that learned nothing new lifts nothing. Pressing
  the button is never recovery. An import failure is our defect, not the
  broker's.
- **R3.** What we could not import is visible to the owner **as that**, and is
  not reported as "no independent source".
- **R4.** A repair that is genuinely ours — extending `broker_operation_kinds`,
  or the owner entering the fact through the documented endpoint — is
  recognised **without** a parser version change.

## The idea

A coverage gap is the statement **"these source rows are not represented in our
journal."** It stops being true when each named row has been *disposed of* — and
disposal is recorded as a fact, not inferred.

Two dispositions exist, because a refused row has two honest outcomes:

- **Supplied** — the row is now represented by named journal events.
- **NoFact** — the row is now understood, and it legitimately produces nothing.

The second is not a formality. `crates/iaam-app/src/adapters/tinkoff.rs:616`
returns `Ok(Vec::new())` for a trade order with no fills; a cancelled order
refused while its kind was unknown will, once the kind is known, correctly yield
no events at all. Under any rule that infers repair from the presence of an
event, such a row can never be cleared.

## Two rejected designs, and why

### Rejected: attempt identity in provenance (2026-08-31)

Give each import attempt an identity; a gap taints only its own attempt. It
separates retries but cannot express repair: a later attempt is "new" because it
ran, not because anything changed. That violates R2 directly — pressing the
button would grant recovery.

### Rejected: repair inferred from the row's identifier being present

The first version of this spec had the gap name refused rows by the source's
operation identifier, and reconciliation check whether an effective event
carried it. Three findings, all verified against the tree, kill it:

1. **`source_operation_id` is event identity, not row identity.** An accepted
   trade row expands into one event per fill, whose identifier is
   `format!("{}#{}", operation_id, trade.num)`
   (`crates/iaam-app/src/adapters/tinkoff.rs:702`). A gap naming `OP-1` can
   never match `OP-1#fill-1`. Trade rows are exactly the class refused by
   `trade_row_reason`, blamed for `Cash` and `Positions`.
2. **Presence is existential and cannot prove completion.** One fill of three
   present would lift the whole row's dimensions. Writes are not batched —
   `append_events` inserts one event at a time
   (`crates/iaam-app/src/adapters/sqlite.rs:197`) — so that intermediate state
   is reachable, and it is a **false confirmation**.
3. **A zero-event row can never be cleared**, per the cancelled-order case
   above.

A fourth, less fatal but decisive for the shape below: clearing a gap by
reversing it does not reverse in the right direction. If the fact that repaired
the row is itself later reversed, the reversed gap stays reversed and the
confirmation stays lifted. A disposition that names the events it rests on
lapses on its own when they stop being effective.

## The change

### 1. A source row has a key

```rust
/// Identity of a row as the source presented it — deliberately distinct from
/// `Provenance::source_operation_id`, which identifies an EVENT.
struct SourceRowKey {
    source: SourceId,
    row: RowName,
}

enum RowName {
    /// The identifier the source gave the row.
    Given(String),
    /// The source gave none: a SHA-256 of the row's raw payload.
    Fingerprint(String),
}
```

Every refused row has a key. There is no unnamed case: a row the source did not
identify is keyed by the fingerprint of its raw JSON, which a later import of
the same unchanged row reproduces exactly. A row whose raw payload changes gets
a different key and is a different row, which is the honest reading.

`Provenance` is **not** changed. The key lives in the coverage gap and in the
resolution, which are import-control facts; it does not belong on every domain
event.

**Known limitation, filed not solved.** `SourceId` is the broker *access*
record's identity (`crates/iaam-app/src/adapters/sqlite.rs:728`,
`SourceId(access.id)`), so revoking and recreating access changes it while the
broker account and its operation identifiers stay the same. Rows recorded under
the old access will not be matched by imports under the new one. The failure is
a false taint, which the owner can clear explicitly, and moving to a durable
namespace (broker, environment, remote account) is its own change with its own
migration.

### 2. The gap names its rows

```rust
ImportCoverageGap {
    period: AssertionPeriod,
    dimensions: BTreeSet<Dimension>,   // kept: the union, owner-facing
    refused: u32,                      // kept: the count, owner-facing
    rows: Vec<RefusedRow>,             // new, #[serde(default)]
}

struct RefusedRow {
    key: SourceRowKey,
    /// What this row alone cannot confirm.
    dimensions: BTreeSet<Dimension>,
}
```

`dimensions` and `refused` are kept rather than derived so that the wire format
stays readable by a build that predates this change, and so that a legacy record
still means what it meant. Validation makes drift impossible (§6).

The gap's idempotency key becomes **row-aware**: a canonical encoding of the
sorted row set with each row's dimensions. Debug or Display formatting must not
be used for it. Today's key carries only the dimension union and the refused
count (`crates/iaam-app/src/scenarios/sync.rs:365`), so a gap about a *different*
row with the same shape collides with an older one and is silently dropped —
tracked separately as iaam-lg4q, and fixed here because this design cannot ship
on top of it.

### 3. Disposal is a fact

```rust
EventKind::ImportRowResolution {
    key: SourceRowKey,
    disposition: RowDisposition,
}

enum RowDisposition {
    /// The row is represented by these events. Never empty.
    Supplied { events: BTreeSet<EventId> },
    /// The row is understood and yields no journal fact.
    NoFact { classification: InertRow, reason: String },
}

enum InertRow {
    /// An order that was cancelled or never filled.
    OrderWithoutFills,
    /// The owner determined the row needs no fact.
    OwnerDetermined,
}
```

A resolution's own provenance records **who made the determination**. It does
not impersonate the source whose row it disposes of — a manual repair carries
manual provenance, or `SourceChannel::is_independent_of` would be fed a false
claim of independence and deduplication would be corrupted.

`InertRow` is a closed enumeration on purpose: "we understood the row and it is
inert" must be a typed, auditable determination. `Ok(Vec::new())` on its own
must never count as disposal — it is indistinguishable from an importer that
silently produced nothing.

### 4. The rule

A refused row is **resolved** when an effective `ImportRowResolution` names its
key and, for `Supplied`, every event it references is effective. Otherwise the
row is outstanding and contributes its dimensions.

`resolve()` excludes both reversed and replaced events
(`crates/iaam-core/src/event/correction.rs`, step 3), so:

- reversing the resolution makes the row outstanding again;
- reversing a supplied fact lapses the resolution on its own, and the taint
  returns without anyone acting;
- **replacing** a supplied fact also lapses it, because the replaced event stops
  being effective. The replacement therefore needs a fresh resolution. That
  fails toward taint rather than confirmation, which is the right direction, but
  it is a sharp edge and the plan must name it.

A resolution is **global by row key**: it clears that row in every gap naming
it, across overlapping periods. A row is either disposed of or it is not; which
gap happened to record it is an accident of when the import ran.

### 5. Taint is a ledger constraint, not a filter on groups

The correlation on `source` and `parser_version` is **dropped**. A missing fact
is missing from the journal, not from a channel, and keeping the correlation
channel-scoped leaves a false-confirmation path: a gap discovered under a new
parser matches no group written under the old one, while `merge_status` keeps
the older group's accepted status
(`crates/iaam-core/src/reconciliation/mod.rs:719` — the maximum, with
`Discrepant` absorbing from either side).

Concretely:

- An outstanding gap constrains `(account, period, dimension)` for **every**
  assertion group of that account whose period **overlaps** it — not only groups
  whose `AssertionPeriod` is equal. A March gap constrains a quarterly assertion
  covering March.
- Statuses are today created only by iterating assertion groups
  (`crates/iaam-core/src/reconciliation/mod.rs:263`). A gap for a period with no
  group must still produce a constraint, or it applies to nothing.
- `with_external_evidence` raises dimensions without consulting gaps
  (`crates/iaam-core/src/reconciliation/mod.rs:278`) and can create a new
  accepted status. The ledger must retain outstanding taints and apply them to
  evidence added later, or the journal-global claim is bypassed by the one API
  built for evidence the journal cannot generate itself.

Two existing tests encode the behaviour being removed, and both assert a defect
rather than a requirement:

- `a_gap_from_another_source_or_parser_leaves_the_group_intact`
  (`crates/iaam-core/tests/reconciliation_ledger.rs:270`);
- `a_later_group_without_a_gap_can_restore_independent_confirmation`
  (`crates/iaam-core/tests/reconciliation_ledger.rs:305`) — iaam-dvki in another
  form.

These stay valid and must keep passing: a named cash gap suppresses cash
promotion (`:225`), does not suppress an unnamed dimension (`:248`), and does
not change whether individual claims match (`:344`).

### 6. Versions and validation

- `SCHEMA_VERSION` 7 → 8: a new `EventKind` variant and a new gap field.
- No SQL migration. The event is persisted as JSON in `payload`
  (`crates/iaam-store/src/events.rs:151`) and `#[serde(default)]` reads records
  written before the change.
- **Validation is schema-aware, and it runs on read.** The projection re-checks
  `validate_structure` over the effective set because the core does not trust
  its input — storage could have been populated bypassing ingestion
  (`crates/iaam-core/src/projection/invariants.rs:95`). A validator that simply
  refused an empty `rows` would make every report fail on a journal holding a
  legacy gap. The rule:
  - `schema_version < 8`: `rows` empty is legal; `dimensions` non-empty as today.
  - `schema_version >= 8`: `rows` non-empty; the union of its rows' dimensions
    equals `dimensions`; `rows.len() as u32 == refused`; every `Supplied`
    resolution carries a non-empty event set.
- The write gate additionally refuses any event whose `schema_version` is not
  `SCHEMA_VERSION`, so a new write cannot claim the legacy allowance.

### 7. Both collectors read the effective set

Groups, gaps and resolutions must all be collected from the effective set.

**Landed ahead of this epic (iaam-ueo1).** `build_with` computed
`resolve(events)` and then passed the **raw** slice to both `collect_groups` and
`collect_coverage_gaps`; both now take `&[&Event]` and receive the effective
set. That was an independent live defect: a reversed control assertion still
formed a group, and — a reversal carrying the kind of its target — asserted its
claim twice. What remains here is the third collector, resolutions, which must
read the same set when it exists.

### 8. Who writes what, and in which order

**The importer.** Before importing an interval, `sync_broker` reads the
outstanding rows for that account and interval. For each row it now disposes of
it appends a resolution — `Supplied` with the events it just recorded, or
`NoFact` with a typed classification.

Order is **facts first, resolution second**. Writes are not batch-atomic
(`crates/iaam-app/src/adapters/sqlite.rs:197`), so a crash between them leaves
the facts recorded and the row outstanding: a false taint, never a false
confirmation. A retry deduplicates the facts, recovers their identifiers and
appends the resolution.

**The owner.** The public repair path targets an outstanding row key. The
endpoint appends the manual fact under truthful manual provenance and then a
resolution referencing it. This is what makes R4 work for the case the adapter
itself directs the owner to: amortisation and redemption are refused with an
instruction to use `POST /v1/ingest/journal-events`
(`crates/iaam-app/src/adapters/tinkoff.rs:432`), and that route mints
`SourceId::new_random()` per request (`crates/iaam-server/src/routes.rs:1266`),
so no presence rule could ever have matched it.

### 9. Showing it to the owner

At import time the refusals are already visible: `Verdict::Quarantined
{ reason }` reaches the DTO (`crates/iaam-server/src/dto.rs:764`).

The report is where the truth is replaced by a different claim.
`crates/iaam-core/src/returns/mod.rs:2234` turns any provisional dimension into
`MaterialIssue::NoIndependentSource` — "there is no independent confirmation for
the account". When the cause is an outstanding row that is false, and it sends
the owner to find a second data source instead of finishing the import.

- `MaterialIssue` gains `ImportIncomplete { account, dimension, outstanding }`,
  where `outstanding` counts rows still undisposed for that dimension.
- The choice becomes conditional: an outstanding gap yields `ImportIncomplete`,
  everything else keeps `NoIndependentSource`.
- `ReturnsRequest` carries the ledger and no journal events
  (`crates/iaam-core/src/returns/mod.rs:596`), so the ledger must carry the
  outstanding counts. There is no other path.
- That site iterates only `Cash` and `Positions` and skips zero-valued
  measurements (`crates/iaam-core/src/returns/mod.rs:2215`). An `Income` or
  `TaxBasis` gap has no way to reach the report today; the plan must either
  widen that loop or state that those dimensions are out of scope, and why.
- The DTO's `MaterialIssue` mapping is an exhaustive match
  (`crates/iaam-server/src/dto.rs:1745`) and gains an arm. Whether
  `ImportIncomplete` counts as a defect in `MaterialIssue::is_defect` decides
  `Mixed` versus `Incomplete` in the data-quality status, and must be decided
  explicitly rather than by whichever arm is written first.

## Tasks

1. `SourceRowKey`, `RowName`; `ImportCoverageGap` gains `rows`; schema-aware
   `validate_structure`; `SCHEMA_VERSION` 7 → 8; the write gate refuses a
   mismatched `schema_version`; legacy records still read. **Cost to plan for:**
   adding an `EventKind` variant breaks seven exhaustive dispatchers in this
   tree — a recorded lesson, not a guess.
2. `EventKind::ImportRowResolution` with `Supplied` / `NoFact`, its structural
   validation, and its projection-inertness across every dispatcher.
3. The row-aware gap idempotency key with a canonical encoding (closes
   iaam-lg4q).
4. `sync_broker` fills `rows` from **both** refusal layers — the adapter's
   quarantine list and the normalisation rejections added by iaam-bl07 — and
   appends resolutions for rows it disposes of, facts first.
5. Reconciliation: the resolution collector reads the effective set as the
   other two already do (iaam-ueo1); the resolution rule; taint as a first-class ledger constraint over
   overlapping periods; `with_external_evidence` cannot bypass it.
6. The owner's repair path: read outstanding rows, and a repair that names the
   row key it disposes of, under truthful manual provenance.
7. The report: `ImportIncomplete`, the ledger's outstanding counts, the DTO arm,
   the `is_defect` decision.
8. The test that does not exist today: **recovery through the same channel.**
   `crates/iaam-core/tests/reconciliation_ledger.rs` builds
   `TestChannel::new("later-parser/1", "later")`, and `TestChannel::new` mints a
   random source (`crates/iaam-core/tests/support/mod.rs:31`), so it proves
   recovery by a *different* channel. The new test holds source and parser
   version fixed and changes only that the row was disposed of.

## Acceptance

1. An outstanding row taints its dimensions; a `Supplied` resolution naming
   effective events clears them — with source and parser version unchanged.
   This is iaam-dvki, and it must fail if correlation still keys on the channel.
2. A row blamed for `{Cash, Positions}` that stays outstanding keeps both
   tainted, even when another row blamed for `{Cash}` is resolved. (The first
   version of this spec got this arithmetic wrong in prose; the union is over
   *unresolved* rows.)
3. A `Supplied` resolution referencing three events, one of which is later
   reversed, no longer resolves the row: the taint returns with no further
   action.
4. A `NoFact` resolution clears a row that produced no events at all; the same
   row without a resolution stays tainted no matter how many imports run.
5. Re-running an import that refuses the same rows appends no event and lifts
   nothing.
6. A gap discovered under parser v2 taints a group asserted under parser v1 for
   an overlapping period.
7. An outstanding gap for a period with no assertion group still constrains the
   account's status for that period, and `with_external_evidence` cannot raise
   past it.
8. A legacy gap (`schema_version` 7, empty `rows`) taints its whole
   `dimensions`, and a report over a journal containing one is produced without
   an invariant failure.
9. Writing a schema-8 gap whose row union disagrees with `dimensions`, or whose
   row count disagrees with `refused`, is refused.
10. A provisional dimension caused by an outstanding row reports
    `ImportIncomplete`; one with no outstanding row still reports
    `NoIndependentSource`.
11. Two refusals whose dimension sets and counts are identical but whose rows
    differ produce two distinct gaps, not one.

## Out of scope, filed separately

- **A durable source-row namespace** replacing `SourceId`, so that reprovisioning
  broker access does not orphan outstanding rows.
- **Cross-source equivalence.** An economically equivalent fact recorded under
  another source row does not resolve this one. The journal cannot prove two
  rows describe the same occurrence from their legs alone, and "account + date +
  amount" is a natural key this code refuses by design — two identical purchases
  in one day are legitimate. Explicit owner resolution is the path.
- **Scenario atomicity (iaam-ge08).** The ordering rule in §8 makes the window
  fail safe; it does not close it.
- **A waiver** — accepting an outstanding row as tolerated without disposing of
  it. Distinct from `NoFact`, which asserts the row yields nothing; a waiver
  would assert it yields something we accept not having.
- `has_out_of_interval_trade` (iaam-l73j); the coverage-date effect of a
  gap-only sync (iaam-y07x).
