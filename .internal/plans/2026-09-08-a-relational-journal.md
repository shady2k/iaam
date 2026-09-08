# A Relational Journal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Replace the JSON-document journal with a normalised relational schema, so every field of a fact is a typed, indexable column and the category and counterparty filters become ordinary predicates.

**Architecture:** `events` carries the envelope, the six optional dates and all of provenance as columns; `event_legs` carries the money and the securities; twelve detail tables carry what each `EventKind` family holds beyond its legs; two child tables carry the only real collections. A value the domain *defines* as equal to a leg is reconstructed from the legs rather than stored twice. Append-only is a property of the store's API, and completeness of a written event is a runtime postcondition — a read-back inside the write transaction — because SQLite can prove a child has its parent but never that a parent has its children.

**Tech Stack:** Rust (edition 2024), rusqlite 0.40 with `bundled` SQLite, `serde`, `time`, `rust_decimal`, `cargo nextest`.

**Spec:** `.internal/specs/2026-09-08-a-relational-journal-design.md` — the schema, every `CHECK`, and the reasoning live there. This plan does not restate the DDL; it says which task writes which part of it.

## Global Constraints

- **English only.** Identifiers, test names, doc comments, `#[error(...)]` texts and any new document. Values that come from an external source stay verbatim (`CLAUDE.md` § Language).
- **No owner data anywhere.** Fixtures are invented from scratch — `Main`, `Savings`, `Shop One` — never trimmed from a real export. `make privacy` runs as a pre-commit hook and must stay green.
- **Greenfield.** No migration of existing data, no backwards compatibility, no `#[serde(default)]` added for an older shape. `dev.db` is deleted and recreated.
- **`make check` is the gate**: `fmt lint arch privacy skill-doc fixtures deps test doc-test`. A task is not done until it is green — **with one structural exception, Task 3.** Removing `events.payload` and the JSON `matcher` column breaks every reader of them, and those readers are rewritten by Tasks 4, 5, 6 and 7. So Task 3 lands with `test` red, and the tasks after it are measured by the failure count falling, not by it being zero. Every other gate — `fmt lint arch privacy skill-doc fixtures deps doc-test` — is green for Task 3 like any other task.
- **Two branches.** `wave-ai/relational-journal` stays green and holds only work that leaves the tree green. Tasks 3 through 7 land on `wave-ai/relational-journal-schema`, which is red from Task 3 until Task 7 closes it, and is merged back once `make check` is green again. A worker is told which branch is its base; without this, a worker cannot tell its own breakage from the breakage it inherited.
- **TDD.** The failing test is written and *run* before the implementation.
- **One commit per task**, message ending with the bead id and the `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` trailer.
- **Do not use TodoWrite.** Task tracking is beads only.
- **The persistence mappers never use `..` in a pattern.** Every field is destructured by name, so a new field on a variant breaks the build rather than being silently dropped. No default-on-read for a required field.

## File Structure

`crates/iaam-store/src/events.rs` (1082 lines) is replaced by a module split by responsibility:

| File | Responsibility |
|---|---|
| `crates/iaam-store/src/journal/mod.rs` | The `SqliteStore` journal methods; re-exports. |
| `crates/iaam-store/src/journal/rows.rs` | Row structs — one per table — and their column lists. No SQL, no domain logic. |
| `crates/iaam-store/src/journal/kind.rs` | `EventKind` ↔ rows. The exhaustive match, both directions. |
| `crates/iaam-store/src/journal/write.rs` | The one insert primitive, taking an open transaction. |
| `crates/iaam-store/src/journal/read.rs` | Two-phase read and bulk hydration. |
| `crates/iaam-store/src/journal/query.rs` | `JournalQuery`, `JournalCursor`, the SQL builder. |
| `crates/iaam-store/src/journal/category_index.rs` | The `event_category_assignments` projection and its rebuild. |
| `crates/iaam-store/migrations/0001_schema.sql` | The whole schema. The only migration. |

Unchanged in place: `bundle.rs`, `rules.rs`, `categories.rs`, `schema.rs` (its `MIGRATIONS` array shrinks).

---

### Task 1: Remove `Event::schema_version`

**Files:**
- Modify: `crates/iaam-core/src/event/mod.rs` (the field, `SCHEMA_VERSION`, every constructor, the legacy branch at :993)
- Modify: `crates/iaam-core/src/event/provenance.rs` (`SOURCE_CATEGORY_IS_A_CATEGORY_FROM` and its users)
- Modify: `crates/iaam-ingest/src/classification.rs`, `operation.rs`, `journal_event.rs`
- Modify: `crates/iaam-app/src/scenarios/import_session.rs` (the `unvouched_word` gate), `ingest.rs`, `coverage_gap.rs`
- Modify: `crates/iaam-server/src/dto.rs:6362` and the route that stamps the version
- Test: the existing suites; delete tests that construct pre-14 shapes

**Interfaces:**
- Consumes: nothing.
- Produces: `Event` without `schema_version`; `iaam_core::event::SCHEMA_VERSION` gone. Every later task assumes the field does not exist.

**Acceptance Criteria:**
- `grep -rn "schema_version" crates/iaam-core crates/iaam-ingest crates/iaam-server/src` returns nothing that refers to an *event's* version.
- `Bundle.schema_version` and `BUNDLE_VERSION` are untouched — they are the store schema version and the bundle format version, two different numbers.
- `validate_import_coverage_gap` no longer has a pre-schema-8 escape: `rows` non-empty, `rows.len() == refused` and `dimensions == union(rows.dimensions)` are unconditional.
- `make check` green.

- [ ] **Step 1: Write the failing test** in `crates/iaam-core/src/event/mod.rs` tests:

```rust
#[test]
fn a_coverage_gap_with_no_rows_is_refused_unconditionally() {
    let event = coverage_gap_without_rows();
    assert!(matches!(
        event.validate_structure(),
        Err(EventValidationError::EmptySet { field: "rows", .. })
    ));
}
```

- [ ] **Step 2: Run it** — `cargo nextest run -p iaam-core a_coverage_gap_with_no_rows`. Expected: PASS is impossible while the pre-8 branch exists, so expect FAIL with `Ok(())`.
- [ ] **Step 3:** Delete the `if self.schema_version < 8 { return Ok(()); }` branch and the field it reads.
- [ ] **Step 4:** Remove the field from `Event`, delete `SCHEMA_VERSION`, and fix every constructor and test the compiler names. Work outward crate by crate: `iaam-core`, `iaam-ingest`, `iaam-app`, `iaam-server`.
- [ ] **Step 5:** Bump the snapshot digest domain tag from `iaam/journal-prefix/v2` to `v3` in `crates/iaam-core/src/projection/state.rs` — the serialisable shape of `Event` changed.
- [ ] **Step 6:** `make check`.
- [ ] **Step 7: Commit** — `git commit -m "Remove the event schema version, which only carried compatibility (<bead>)"`.

---

### Task 2: `validate_transfer` checks the signs

**Files:**
- Modify: `crates/iaam-core/src/event/mod.rs:545` (`validate_transfer`)
- Modify: `crates/iaam-core/src/event/mod.rs` (`EventValidationError`, a new variant if the existing ones do not fit)

**Interfaces:**
- Consumes: Task 1's `Event`.
- Produces: a transfer whose legs are reversed is rejected. Task 4 relies on this: it stores `from`/`to` as semantic roles and must not store a fact whose roles contradict its legs.

**Acceptance Criteria:**
- A transfer with `from = A`, `to = B`, legs `A: +100`, `B: −100` and `amount = −100` is rejected. It passes today.
- The `from` leg is negative, the `to` leg is positive, the declared amount is positive and equals the `to` leg, the two legs share a currency, and the event's own account is one of the two.
- Every existing transfer test still passes; ones that relied on the loose behaviour are corrected, not deleted.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn a_transfer_with_its_legs_reversed_is_refused() {
    let event = transfer_with_legs(/* from */ -100 /* on to */, /* to */ 100 /* on from */);
    assert!(event.validate_structure().is_err());
}
```

- [ ] **Step 2: Run it** — expect FAIL (`Ok(())` today).
- [ ] **Step 3:** Add the sign, currency and membership checks to `validate_transfer`.
- [ ] **Step 4:** Run the whole `iaam-core` suite; fix the fixtures that were relying on the gap.
- [ ] **Step 5:** `make check`. **Step 6:** Commit.

---

### Task 3: The single schema file

**Files:**
- Create: `crates/iaam-store/migrations/0001_schema.sql`
- Delete: the other 30 files under `crates/iaam-store/migrations/`
- Modify: `crates/iaam-store/src/schema.rs` (`SCHEMA_VERSION = 1`, `MIGRATIONS` becomes one entry)
- Delete: `crates/iaam-store/tests/migration_0008.rs` and any other per-migration test
- Test: `crates/iaam-store/tests/schema.rs`

**Interfaces:**
- Produces: every table, index and `CHECK` in spec §4. Later tasks bind to these exact column names.

**Acceptance Criteria:**
- The file contains, verbatim from spec §4: `events` (§4.1 including all five `CHECK`s and the two declared foreign keys), `event_legs` (§4.2 including the `CASE kind` check), the twelve detail tables (§4.3), the two collection tables (§4.4), the normalised rule tables (§4.6), `event_category_assignments` (§4.7), and the index list (§6.2).
- The out-of-scope tables (§3) are carried over unchanged from the migrations being deleted, **including their triggers** — reference data, contour versions and source documents keep theirs. Only the two journal immutability triggers are dropped.
- A fresh database applies the schema and reports `user_version = 1`.
- A vocabulary test asserts each `CHECK (x IN (...))` list equals the Rust enum it mirrors.
- `test` is red **only** for `no such column: payload` and `no such column: matcher` and their cascades. A `CHECK` violation, a missing table or a foreign-key failure is a defect in this task and must be fixed here.

- [ ] **Step 1: Write the failing test** in `crates/iaam-store/tests/schema.rs`:

```rust
#[test]
fn the_kind_check_lists_exactly_the_event_discriminants() {
    let store = SqliteStore::open_in_memory().expect("open");
    let ddl: String = store.connection().query_row(
        "SELECT sql FROM sqlite_master WHERE name = 'events'", [], |r| r.get(0),
    ).expect("events ddl");
    for kind in EventKind::discriminants() {
        assert!(ddl.contains(&format!("'{kind}'")), "kind {kind} missing from the CHECK");
    }
}
```

- [ ] **Step 2: Run it** — expect FAIL, no such table shape.
- [ ] **Step 3:** Write `0001_schema.sql`. Carry over the out-of-scope tables by concatenating them from the migrations being deleted, in dependency order, then append the journal, rule and projection tables from the spec.
- [ ] **Step 4:** `SCHEMA_VERSION = 1`; `MIGRATIONS` becomes `[(1, include_str!("../migrations/0001_schema.sql"))]`.
- [ ] **Step 5:** `rm dev.db`. **Step 6:** Run the test — PASS. **Step 7:** `make check`. **Step 8:** Commit.

---

### Task 4: Row types and the `EventKind` mapper

**Files:**
- Create: `crates/iaam-store/src/journal/mod.rs`, `rows.rs`, `kind.rs`
- Modify: `crates/iaam-store/src/lib.rs` (declare the module; keep `pub use` of the old names so callers still compile)

**Interfaces:**
- Consumes: Task 3's column names.
- Produces:
  ```rust
  pub(crate) struct EventRow { /* one field per events column */ }
  pub(crate) struct LegRow  { /* one field per event_legs column */ }
  pub(crate) enum DetailRows { Trade(TradeRow), CashTransfer(CashTransferRow), /* ... */ None }

  pub(crate) fn to_rows(event: &Event) -> Result<(EventRow, Vec<LegRow>, DetailRows), StoreError>;
  pub(crate) fn from_rows(header: EventRow, legs: Vec<LegRow>, detail: DetailRows)
      -> Result<Event, StoreError>;
  ```

**Acceptance Criteria:**
- `from_rows(to_rows(e))` equals `e` for every variant, every nested subvariant, every `Option` combination, `i64::MIN`/`i64::MAX` amounts, decimals of differing scale, and empty against non-empty collections.
- The amounts listed in spec §4.5 as *reconstructed* do not appear in any row struct; the ones listed as *stored* do.
- `CashTransfer` carries `from_account` and `to_account`.
- `BasisAllocation::Known` carries `share`, `inputs_hash`, `knowledge_as_of` and `algorithm_version`.
- No `..` in any match arm in `kind.rs`.

- [ ] **Step 1: Write the failing property test** in `crates/iaam-store/src/journal/kind.rs`:

```rust
#[test]
fn every_variant_round_trips_through_rows() {
    for event in every_event_shape() {
        let (header, legs, detail) = to_rows(&event).expect("to_rows");
        let back = from_rows(header, legs, detail).expect("from_rows");
        assert_eq!(back, event, "round trip lost something in {}", event.kind.discriminant());
    }
}
```

`every_event_shape()` is a fixture enumerating all seventeen variants with their subvariants and `Option` combinations. It is invented data — `Main`, `Savings`, `Shop One` — never derived from an export.

- [ ] **Step 2: Run it** — expect FAIL, functions not defined.
- [ ] **Step 3:** Write `rows.rs`, then `kind.rs`. Reconstruction of the §4.5 amounts goes through typed helpers `single_money_leg(kind, LegKind)` and siblings; never a search by `ordinal`.
- [ ] **Step 4: Run** — PASS. **Step 5:** `make check`. **Step 6:** Commit.

---

### Task 5: The normalised rule tables

**Files:**
- Modify: `crates/iaam-store/src/rules.rs` (classification), `categories.rs` (category rules)
- Modify: `crates/iaam-app/src/adapters/sqlite.rs` (the rule ports)
- Modify: `crates/iaam-app/src/scenarios/classification.rs`, `categories.rs` (they pass JSON strings today)

**Interfaces:**
- Consumes: Task 3's rule tables.
- Produces: `StoredRule` and `CategoryRuleView` carrying typed fields instead of `matcher: String` / `matcher_json: String`.

**Acceptance Criteria:**
- `RuleMatcher`'s seven optional conditions are seven columns, with the `asks_nothing` `CHECK` from spec §4.6.
- `Classification`'s six variants are `outcome_kind` plus conditional `to_account`, `fee_origin`, `income_kind`.
- `CategoryMatcher`'s four variants are `matcher_kind`, `value`, `text`, `description_mode`.
- No `serde_json` call remains in `rules.rs` or `categories.rs`.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn a_classification_rule_asking_nothing_is_refused_by_the_database() {
    let store = SqliteStore::open_in_memory().expect("open");
    let outcome = store.connection().execute(
        "INSERT INTO classification_rules
             (id, owner, version, outcome_kind, created_at)
         VALUES ('r1', 'o1', 1, 'external_flow', '2026-01-01T00:00:00Z')",
        [],
    );
    assert!(outcome.is_err(), "a matcher with no condition must not be storable");
}
```
- [ ] **Step 2: Run it** — expect FAIL.
- [ ] **Step 3:** Replace the JSON columns with typed ones and rewrite the read/write functions.
- [ ] **Step 4: Run** — PASS. **Step 5:** `make check`. **Step 6:** Commit.

---

### Task 6: Two-phase read and hydration

**Files:**
- Create: `crates/iaam-store/src/journal/read.rs`
- Modify: `crates/iaam-store/src/journal/mod.rs`

**Interfaces:**
- Consumes: Task 4's `from_rows`.
- Produces:
  ```rust
  pub(crate) fn hydrate(conn: &Connection, ids: &[EventId]) -> Result<Vec<Event>, StoreError>;
  pub(crate) fn hydrate_one(conn: &Connection, id: EventId) -> Result<Option<Event>, StoreError>;
  ```
  `hydrate` returns events in the order `ids` gave, and issues a fixed number of statements regardless of `ids.len()`.

**Acceptance Criteria:**
- A page of N events costs a constant number of SQL statements — asserted by counting through a `trace` hook, not by inspection.
- `hydrate` preserves the caller's order.
- An id with no row is absent from the result rather than an error; `hydrate_one` returns `None`.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn hydrating_a_page_costs_a_fixed_number_of_statements() {
    let (store, ids) = store_with_events(50);
    let counted = count_statements(&store, || hydrate(store.connection(), &ids));
    let (store2, ids2) = store_with_events(5);
    let counted2 = count_statements(&store2, || hydrate(store2.connection(), &ids2));
    assert_eq!(counted, counted2, "statement count must not grow with the page");
}
```

- [ ] **Step 2: Run it** — FAIL, `hydrate` not defined.
- [ ] **Step 3:** Implement `hydrate`: one `SELECT ... WHERE id IN (...)` per table touched, then group in Rust and call `from_rows`.
- [ ] **Step 4: Run** — PASS. **Step 5:** `make check`. **Step 6:** Commit.

---

### Task 7: The write primitive with a read-back postcondition

**Files:**
- Create: `crates/iaam-store/src/journal/write.rs`
- Modify: `crates/iaam-store/src/journal/mod.rs` (`append_event`, `append_event_in_order`)
- Modify: `crates/iaam-store/src/bundle.rs:294` (route it through the primitive)
- Modify: `crates/iaam-store/src/lib.rs` (`connection()` and `connection_mut()` behind `#[cfg(any(test, feature = "test-support"))]`)
- Modify: every test that called `.connection()` — add the feature to the dev-dependency

**Interfaces:**
- Consumes: Task 4's `to_rows`, Task 6's `hydrate_one`.
- Produces:
  ```rust
  pub(crate) fn insert_event_in(
      tx: &rusqlite::Transaction<'_>,
      event: &Event,
  ) -> Result<(), StoreError>;
  ```
  Takes an **already open** transaction and opens none. `append_event` becomes `&mut self`.

**Acceptance Criteria:**
- `insert_event_in` ends by calling `hydrate_one` inside the transaction and comparing the result to the event it was given; unequal or missing is `StoreError::IncompleteWrite`.
- All three write paths call it. `grep -n "insert_event" crates/iaam-store/src` shows one definition and no other insert sequence.
- The write path checks that every account and custody place named by a leg belongs to the event's owner.
- Fault injection: an induced failure at each Nth insert, a panic between header and legs and between legs and detail, and a foreign-key failure before commit — after each, no row of that event exists in any of the sixteen tables.
- **Digest stability** (spec §9): `encode(hydrate_one(insert(event))) == encode(event)`, where `encode` is the canonical CBOR the snapshot prefix digest hashes (`projection/state.rs:240`). A decimal whose representation changes on its way through `TEXT` — `1.0` becoming `1` — would change the digest and invalidate every snapshot; this test is what catches it.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn a_writer_that_forgets_the_detail_row_is_refused() {
    let store = SqliteStore::open_in_memory().expect("open");
    let event = a_trade();
    let outcome = insert_without_detail(&store, &event);   // test-only saboteur
    assert!(matches!(outcome, Err(StoreError::IncompleteWrite { .. })));
    assert_eq!(count_rows(&store, "events"), 0);
}
```

- [ ] **Step 2: Run it** — FAIL, no such error and no read-back.
- [ ] **Step 3:** Implement `insert_event_in` with the read-back; add `StoreError::IncompleteWrite`.
- [ ] **Step 4:** Route `append_event`, `append_event_in_order` and `bundle.rs` through it.
- [ ] **Step 5:** Write the fault-injection tests listed in the acceptance criteria.
- [ ] **Step 6:** Gate `connection()`/`connection_mut()` behind the test feature.
- [ ] **Step 7: Run** — PASS. **Step 8:** `make check`. **Step 9:** Commit.

---

### Task 8: The query builder and the indexes

**Files:**
- Create: `crates/iaam-store/src/journal/query.rs`
- Delete: the `json_extract`/`json_each` predicates in the old `journal_sql`
- Test: `crates/iaam-store/tests/query_plans.rs`

**Interfaces:**
- Consumes: Task 6's `hydrate`.
- Produces: `JournalQuery` unchanged in shape, with `currencies` and `touching` now built from columns; `list_journal_events` becomes phase 1 (ids) + `hydrate`.

**Acceptance Criteria:**
- No `json_extract` or `json_each` remains anywhere in `crates/iaam-store/src`.
- Every filter in spec §6.2 has its index, and `EXPLAIN QUERY PLAN` shows it used — one assertion per filter.
- Keyset pagination still returns each event exactly once across pages, asserted over a generated journal with duplicate `(effective_date)` values.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn the_currency_filter_uses_an_index_and_not_a_scan() {
    let store = store_with_events(100);
    let query = JournalQuery { currencies: vec![CurrencyCode::Rub], ..JournalQuery::default() };
    let plan = explain(&store, &journal_sql(owner, &query).expect("sql").0);
    assert!(plan.contains("USING INDEX"), "plan was: {plan}");
    assert!(!plan.contains("SCAN event_legs"), "plan was: {plan}");
}
```

- [ ] **Step 2: Run it** — FAIL (`json_each` scan today).
- [ ] **Step 3:** Rewrite the builder against columns; phase 1 selects ids only.
- [ ] **Step 4: Run** — PASS. **Step 5:** `make check`. **Step 6:** Commit.

---

### Task 9: The category-assignment projection

**Files:**
- Create: `crates/iaam-store/src/journal/category_index.rs`
- Modify: `crates/iaam-app/src/scenarios/categories.rs` (rebuild on rule create/edit/retire)
- Modify: `crates/iaam-store/src/journal/write.rs` (incremental insert on append)

**Interfaces:**
- Consumes: Task 5's typed category rules, Task 7's write primitive.
- Produces:
  ```rust
  pub(crate) fn rebuild(tx: &Transaction<'_>, owner: OwnerId) -> Result<u32, StoreError>;
  pub(crate) fn assign_for(tx: &Transaction<'_>, owner: OwnerId, events: &[Event])
      -> Result<(), StoreError>;
  pub(crate) fn revision(conn: &Connection, owner: OwnerId) -> Result<i64, StoreError>;
  ```

**Acceptance Criteria:**
- Every row equals `iaam_core::category::assign` computed directly, over a generated journal and rule set. There is one implementation of the priority ladder and this is one of its callers — the projection does not reimplement it.
- A rule edit followed by a rebuild leaves no stale row, and `NotDecomposed` is the absence of a row.
- A read whose `rules_revision` does not match the highest active `category_rules.version` rebuilds before answering.

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn the_projection_agrees_with_the_core_for_every_event() {
    let (store, events, rules) = journal_with_category_rules();
    rebuild(&tx, owner).expect("rebuild");
    for event in &events {
        let stored = stored_assignment(&store, event.id);
        let computed = iaam_core::category::assign(&subject(event), &rules);
        assert_eq!(stored, computed, "event {}", event.id);
    }
}
```

- [ ] **Step 2: Run it** — FAIL. **Step 3:** Implement. **Step 4: Run** — PASS. **Step 5:** `make check`. **Step 6:** Commit.

---

### Task 10: The bundle

**Files:**
- Modify: `crates/iaam-store/src/bundle.rs`

**Interfaces:**
- Consumes: Task 7's `insert_event_in`.

**Acceptance Criteria:**
- Export and import round-trip a journal containing every event variant.
- An import whose events reference a replacement target that appears later in the file succeeds — the deferred `(owner, relation_target)` key is what allows it.
- An import of events carrying `import_session` and `settled_by_rule` identifiers that the bundle does not carry succeeds; neither is a foreign key.
- `Bundle.schema_version` is 1 after the collapse; `BUNDLE_VERSION` is unchanged.

- [ ] **Step 1: Write the failing test** — a bundle whose replacement precedes its target imports cleanly.
- [ ] **Step 2: Run** — FAIL. **Step 3:** Implement. **Step 4: Run** — PASS. **Step 5:** `make check`. **Step 6:** Commit.

---

### Task 11: `category` and `counterparty` reach the API

**Files:**
- Modify: `crates/iaam-store/src/journal/query.rs` (two fields on `JournalQuery`)
- Modify: `crates/iaam-app/src/ports.rs` (`JournalStore`), `crates/iaam-app/src/adapters/sqlite.rs`
- Modify: `crates/iaam-app/src/scenarios/journal.rs` (`JournalReadQuery`, the aggregate)
- Modify: `crates/iaam-server/src/dto.rs`, `routes.rs`, the OpenAPI document
- Test: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: Task 8's builder, Task 9's projection.
- Produces: `category: Option<CategoryId>`, `uncategorised: bool`, `counterparty: Option<AccountId>` on both the journal route and the aggregate route.

**Acceptance Criteria:**
- Both filters work identically on `/v1/journal/events` and `/v1/journal/aggregate` — that pairing is the design of the two routes (`iaam-9xku`).
- `counterparty` matches only the far endpoint: a row where the named account is the *near* side is not returned, which is what distinguishes it from `touching`.
- `uncategorised` returns exactly the events with no projection row.
- The OpenAPI document describes both, and the contract test asserts the exact parameter names.
- `iaam-fg0e` closes on this task.

- [ ] **Step 1: Write the failing contract test** — a transfer `Main → Savings` is returned by `counterparty=Savings` when read from `Main`, and not returned by `counterparty=Main` when read from `Main`.
- [ ] **Step 2: Run** — FAIL, unknown parameter. **Step 3:** Implement through the layers. **Step 4: Run** — PASS. **Step 5:** `make check`. **Step 6:** Commit.

---

## Execution order

Tasks 1 → 2, 3 → 4, 5 → 6 → 7 → 8, 9 → 10, 11. Parallel where the file sets do not intersect:

| Wave | Tasks | Why they can share a wave |
|---|---|---|
| 1 | T1 | Cross-cutting; everything downstream mirrors the domain it changes. |
| 2 | T2 ∥ T3 | T2 is `iaam-core`, T3 is `iaam-store/migrations`. |
| 3 | T4 ∥ T5 | T4 is `journal/`, T5 is `rules.rs`/`categories.rs`. |
| 4 | T6 | `read.rs`; T7 needs it for the postcondition. |
| 5 | T7 | `write.rs`, `bundle.rs` call site, `lib.rs`. |
| 6 | T8 ∥ T9 | `query.rs` against `category_index.rs`. |
| 7 | T10 ∥ T11 | `bundle.rs` against the app and server layers. |
