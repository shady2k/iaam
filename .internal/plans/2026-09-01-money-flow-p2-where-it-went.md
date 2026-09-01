# Money flow P2 — "and where it went": implementation plan

> **For agentic workers:** each Task below is one bead and one worker. Steps use
> `- [ ]` for human readability; tracking is in beads. Do not run repo-wide
> gates or the full test suite — see Global Constraints.

**Goal:** the flow report decomposes the outflow by the owner's own two-level
category list, with categories assigned automatically from rules rather than by
hand, and a rule that changes shows what it moved before it stands.

**Architecture:** a category is **never a field on an event**. It is derived
from versioned rules over `(row attributes, date)`, so renaming, splitting,
merging and retiring categories touch reference data only and never the
append-only journal. Every rule carries an interval of validity, because a
merchant that sold pies in 2024 and umbrellas in 2026 is one string with two
meanings. The source's own category column is captured at ingest and is one rule
input among several, never the owner's decision.

**Tech Stack:** Rust 2024, workspace of ten crates. SQLite migrations under
`crates/iaam-store/migrations/` (next number is `0015`). `serde` for the journal
payload, so new optional event fields need no SQL migration. Tests are `#[test]`
unit tests beside the code plus integration tests under `crates/<crate>/tests/`.

**Spec:** `.internal/specs/2026-09-01-money-flow-design.md` — read §3
("Categories: a living list, derived rather than stored") and R6, R11, R12
before starting any task.

**Depends on:** epic E5 (`.internal/plans/2026-09-01-money-flow-p1-august-visible.md`),
merged. `MoneyFlow`, `money_flow`, `account_balances` and
`GET /v1/reports/flow` all exist.

**Not in this plan:** the deterministic file importer and its column mappings
(P3), the Actual Budget migration, the web UI, budgets.

## The distinction that shapes everything

`iaam-ingest`'s existing `Classification` — `InternalTransfer`, `ExternalFlow`,
`Fee`, `Income` — answers **"what kind of operation is this"**. A category
answers **"what did the money go to"**. They are different questions and must
not share a type: a rule reading "Corner Shop → Продукты" says nothing about
whether a row is a fee or a withdrawal.

One consequence is load-bearing and makes P2 *simpler* than the classification
machinery it sits beside:

- **Changing a classification rule writes to the journal** — reversal and
  replacement — because the classification determines the event *kind*
  (`crates/iaam-ingest/src/classification.rs`, `Correction`).
- **Changing a category rule writes nothing at all.** The category is not an
  event and never was. Recomputation is a pure re-evaluation of a projection.

So R12's "recomputation shows its work" is a **diff computed on demand**, not a
journal correction. No task in this plan appends an event.

## Global Constraints

- **English only** in everything new: identifiers, test names, doc comments,
  inline comments, `#[error(...)]` text. Existing Russian comments and SQL
  comments are left alone, not retranslated. Domain terms come from
  `docs/glossary-ru-en.md`.
- `unsafe_code = "forbid"`, `clippy::all` denied at workspace level. **Do not put
  `#[must_use]` on a function returning `Result`** — `Result` is already
  `must_use` and `clippy::double_must_use` is denied.
- **Validation logic never goes in a function named `new`** — `cargo-mutants`
  skips those (§15.7).
- **Workers run targeted tests only.** `cargo test -p <crate> <filter>` and
  `cargo check -p <crate>`, each **with the `direnv exec <worktree>` prefix** —
  `cargo` is not on `PATH`; the toolchain comes from the nix flake. `cargo test`
  takes **one** filter, not two.
- Do **not** run `make check`, `cargo-mutants`, `cargo fmt`, or workspace-wide
  clippy. The coordinator runs those once at the end of the epic.
- **`cargo check -p <crate>` is mandatory before claiming a task done.**
- **No `f64`.** Amounts are `PostedMinor` / `Money`; addition is `checked_add`.
- **Currencies are never silently added.**
- **A new `EventKind` variant requires a schema-version bump** — the ledger at
  the end of `crates/iaam-core/src/event/mod.rs`'s `mod tests` records why, and
  two hard-coded assertions follow it (`crates/iaam-server/tests/contract.rs`
  and `crates/iaam-app/tests/sync.rs`). **No task in this plan adds one**; if you
  think yours does, stop and escalate.
- `crates/iaam-server/tests/snapshots/contract__the_report_shape_is_frozen_by_a_snapshot.snap`
  freezes the returns report shape. No task here may change it.
- Do not touch the issue tracker. Do not commit, push, or branch unless the task
  says so.

## File Structure

| File | Responsibility |
|---|---|
| `crates/iaam-ingest/src/operation.rs` | `SubmittedOperation` gains `source_category` |
| `crates/iaam-core/src/event/provenance.rs` | provenance carries the source's category verbatim |
| `crates/iaam-store/migrations/0015_categories.sql` | **new** — groups, categories, category rules |
| `crates/iaam-core/src/category.rs` | **new** — `CategoryId`, `CategoryRule`, interval, `assign` |
| `crates/iaam-store/src/categories.rs` | **new** — reference and rule persistence |
| `crates/iaam-app/src/ports.rs` | `CategoryStore` port |
| `crates/iaam-app/src/scenarios/categories.rs` | **new** — list/create/retire, and the rule diff |
| `crates/iaam-app/src/scenarios/reports.rs` | the flow report gains its decomposition |
| `crates/iaam-core/src/projection/money_flow.rs` | outflow accumulated per category |
| `crates/iaam-server/src/dto.rs`, `routes.rs`, `lib.rs` | DTOs and routes |

---

### Task 1: The source's own category survives ingestion

Without this, the cheapest rule source — "map the bank's thirty category values
onto mine, once" — has nothing to match on, and the owner is back to answering
per merchant.

**Files:**
- Modify: `crates/iaam-core/src/event/provenance.rs`
- Modify: `crates/iaam-ingest/src/operation.rs`
- Modify: `crates/iaam-server/src/dto.rs` (`OperationDto`)
- Test: `crates/iaam-ingest/tests/normalization.rs`, `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Produces: `Provenance::with_source_category(self, category: impl Into<String>) -> Self`
  and `Provenance::source_category(&self) -> Option<&str>`, following
  `with_source_operation_id` / `source_operation_id` exactly.
  `SubmittedOperation { source_category: Option<String>, .. }`.

**Acceptance Criteria:**
- A submitted operation carrying `source_category: "Супермаркеты"` produces an
  event whose provenance returns that exact string.
- The value is stored **verbatim**, never normalised, lower-cased or trimmed
  into a different string: it is the source's word, and rewriting it silently
  breaks the mapping rules that key on it.
- An operation without the field round-trips as `None`; existing stored events
  deserialize unchanged (`#[serde(default)]`).
- No schema-version bump: this adds an optional provenance field, not an event
  kind. State that in the commit message.

- [ ] **Step 1: Write the failing test**

Append to `crates/iaam-ingest/tests/normalization.rs`:

```rust
#[test]
fn the_sources_own_category_survives_normalisation_verbatim() {
    let mut operation = submit(OperationKind::Withdrawal {
        amount_minor: 120_000,
        currency: CurrencyCode::Rub,
    });
    // The source's word, with its capital letter and its spacing. A rule maps
    // it to the owner's category by exact value; normalising it here would
    // silently stop that rule matching.
    operation.source_category = Some("Супермаркеты".to_owned());
    let event = normalize(&operation, context()).expect("normalises").event;
    assert_eq!(event.provenance.source_category(), Some("Супермаркеты"));
}

#[test]
fn an_operation_without_a_source_category_carries_none() {
    let operation = submit(OperationKind::Withdrawal {
        amount_minor: 120_000,
        currency: CurrencyCode::Rub,
    });
    let event = normalize(&operation, context()).expect("normalises").event;
    assert_eq!(event.provenance.source_category(), None);
}
```

`submit` builds `SubmittedOperation` with a struct literal — add the new field
there too, as `None`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `direnv exec $WORKTREE cargo test -p iaam-ingest --test normalization the_sources_own_category`
Expected: FAIL — no field `source_category`.

- [ ] **Step 3: Write the implementation**

In `crates/iaam-core/src/event/provenance.rs`, add the field beside
`source_operation_id`:

```rust
    /// The category the source itself assigned to the row.
    ///
    /// Retained separately from any owner category and never rewritten. It is
    /// evidence about what the source said, not a decision: a bank calling a
    /// subscription "Развлечения" is a hint the owner may map or override, and
    /// storing it as the owner's own category would let the bank decide what
    /// his spending was.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_category: Option<String>,
```

with the builder and accessor mirroring `with_source_operation_id` and
`source_operation_id`. Initialise it to `None` in `new`.

In `crates/iaam-ingest/src/operation.rs`, add
`#[serde(default, skip_serializing_if = "Option::is_none")] pub source_category: Option<String>`
to `SubmittedOperation`, and in `normalize` attach it to the provenance when
present. In `crates/iaam-server/src/dto.rs`, add the matching optional field to
`OperationDto` and pass it through `to_domain`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `direnv exec $WORKTREE cargo test -p iaam-ingest --test normalization the_sources_own_category`, then the same with `an_operation_without_a_source_category`.
Expected: PASS, one test each.

- [ ] **Step 5: Prove it survives the API and the store**

Add to `crates/iaam-server/tests/contract.rs`, using `harness_on_disk()` and the
pattern of `the_same_declared_source_yields_the_same_source_id`: submit an
operation with a source category, read the event back from the store, assert the
provenance carries it verbatim.

Run: `direnv exec $WORKTREE cargo test -p iaam-server --test contract source_category`
Expected: PASS.

- [ ] **Step 6: Check the crates compile**

Run: `direnv exec $WORKTREE cargo check -p iaam-core`, then `-p iaam-ingest`, then `-p iaam-server`.
Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add crates/iaam-core/src/event/provenance.rs crates/iaam-ingest/ crates/iaam-server/src/dto.rs crates/iaam-server/tests/contract.rs
git commit -m "feat(core,ingest,server): the source's own category survives ingestion"
```

---

### Task 2: The category reference — two levels, and nothing is ever deleted

**Files:**
- Create: `crates/iaam-store/migrations/0015_categories.sql`
- Create: `crates/iaam-store/src/categories.rs`
- Modify: `crates/iaam-store/src/lib.rs` (module declaration)
- Test: `crates/iaam-store/tests/` — new `categories.rs`, following a neighbouring store test

**Interfaces:**
- Produces: `CategoryGroupRow { id, owner, title, retired_at }`,
  `CategoryRow { id, owner, group_id, title, retired_at }`, and store methods
  `insert_category_group`, `insert_category`, `retire_category`,
  `list_categories(owner) -> Vec<CategoryRow>` (including retired, with the flag).

**Acceptance Criteria:**
- A category belongs to exactly one group; a group holds many categories.
- **Retiring never deletes.** `retire_category` sets `retired_at`; the row stays,
  because rules and past reports point at it and a deleted category would turn a
  historical report into a lie about what it used to say.
- `list_categories` returns retired categories too, flagged, so a caller can
  render history without resurrecting the category for new assignments.
- Re-running the migration on a populated database is a no-op.

- [ ] **Step 1: Write the migration**

Create `crates/iaam-store/migrations/0015_categories.sql`. Follow the style of
`0002_sources_and_rules.sql` — `STRICT` tables, an index per access path, and a
comment above each table saying *why* it is shaped that way. Comments in this
file may be Russian to match its neighbours, or English; be consistent within the
file.

```sql
CREATE TABLE category_groups (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    retired_at TEXT
) STRICT;

CREATE UNIQUE INDEX category_groups_by_title ON category_groups (owner, title);

CREATE TABLE categories (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    group_id   TEXT NOT NULL REFERENCES category_groups (id),
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    -- Retirement, never deletion: rules and printed reports point at this row,
    -- and removing it would turn a past report into a lie about what it said.
    retired_at TEXT
) STRICT;

CREATE UNIQUE INDEX categories_by_title ON categories (owner, group_id, title);
CREATE INDEX categories_by_owner ON categories (owner, retired_at);
```

- [ ] **Step 2: Write the failing test**

Create `crates/iaam-store/tests/categories.rs`, copying the setup of a
neighbouring store test (read `crates/iaam-store/tests/migration_0013.rs` first).

```rust
#[test]
fn a_retired_category_is_still_listed_and_still_flagged() {
    let store = open_temp_store();
    let owner = OwnerId::new_random();
    let group = store.insert_category_group(owner, "Usual Expenses").expect("group");
    let food = store.insert_category(owner, group, "Продукты").expect("category");
    store.retire_category(owner, food).expect("retired");

    let listed = store.list_categories(owner).expect("listed");
    let row = listed.iter().find(|row| row.id == food).expect("still listed");
    // Present and marked, not gone: a past report decomposed spending into this
    // category, and dropping the row would make that report unreadable.
    assert!(row.retired_at.is_some());
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `direnv exec $WORKTREE cargo test -p iaam-store --test categories`
Expected: FAIL — the methods do not exist.

- [ ] **Step 4: Write the implementation**

Create `crates/iaam-store/src/categories.rs` with the row types and the four
methods, following how `crates/iaam-store/src/events.rs` opens statements and
maps rows. Declare the module in `crates/iaam-store/src/lib.rs`.

- [ ] **Step 5: Run the test to verify it passes**

Run: `direnv exec $WORKTREE cargo test -p iaam-store --test categories`
Expected: PASS.

- [ ] **Step 6: Check the crate compiles**

Run: `direnv exec $WORKTREE cargo check -p iaam-store`
Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add crates/iaam-store/
git commit -m "feat(store): the category reference, two levels, retired never deleted"
```

---

### Task 3: A category rule, valid over an interval

This is the task the whole epic turns on. Read spec §3 and R11 before writing a
line.

**Files:**
- Create: `crates/iaam-core/src/category.rs`
- Modify: `crates/iaam-core/src/lib.rs` (module declaration)
- Modify: `crates/iaam-core/src/ids.rs` (`CategoryId`, `CategoryGroupId`, `CategoryRuleId` via `typed_id!`)
- Test: in `crates/iaam-core/src/category.rs`

**Interfaces:**

```rust
pub struct CategoryInterval { pub from: Option<Date>, pub to: Option<Date> }
impl CategoryInterval { pub fn covers(&self, on: Date) -> bool; }

pub struct CategorySubject<'a> {
    pub source_category: Option<&'a str>,
    pub counterparty: Option<&'a str>,
    pub description: Option<&'a str>,
    pub on: Date,
}

pub enum CategoryBasis {
    Row { rule: CategoryRuleId },
    SourceCategory { rule: CategoryRuleId },
    Description { rule: CategoryRuleId },
}

pub struct CategoryRule {
    pub id: CategoryRuleId,
    pub version: u32,
    pub interval: CategoryInterval,
    pub matcher: CategoryMatcher,
    pub category: CategoryId,
}

pub enum CategoryMatcher {
    /// One specific row, by its stable key. The owner's hand-made decision.
    Row { key: String },
    /// The source's own category value, matched exactly.
    SourceCategory { value: String },
    /// A case-insensitive substring of the counterparty or description.
    DescriptionContains { text: String },
}

pub enum CategoryAssignment {
    Assigned { category: CategoryId, basis: CategoryBasis },
    /// No rule covers this row on this date. Never a silent bucket.
    NotDecomposed,
}

pub fn assign(subject: &CategorySubject<'_>, rules: &[CategoryRule]) -> CategoryAssignment;
```

**Acceptance Criteria:**
- `assign` considers only rules whose interval covers `subject.on`.
- Precedence is strict and tested: a `Row` rule beats a `SourceCategory` rule,
  which beats a `DescriptionContains` rule. Within one kind, the highest
  `version` wins, as `classify` already does for classification rules.
- A merchant matched by a `DescriptionContains` rule valid only in 2024 is **not**
  categorised by it for a 2026 row — the pies-and-umbrellas case, tested
  explicitly.
- A row no rule covers yields `NotDecomposed`. There is no "Other" category and
  no fallback; inventing one is how a decomposition stops being informative.
- A rule whose matcher is an empty string matches nothing, mirroring
  `RuleMatcher::asks_nothing` (`crates/iaam-ingest/src/classification.rs:66`):
  a rule that catches everything can only be written by mistake.

- [ ] **Step 1: Write the failing test**

In the new file's `#[cfg(test)] mod tests`:

```rust
    fn rule(id: u128, version: u32, matcher: CategoryMatcher, from: Option<Date>, to: Option<Date>, category: u128) -> CategoryRule {
        CategoryRule {
            id: CategoryRuleId(uuid::Uuid::from_u128(id)),
            version,
            interval: CategoryInterval { from, to },
            matcher,
            category: CategoryId(uuid::Uuid::from_u128(category)),
        }
    }

    #[test]
    fn a_merchant_that_changed_its_trade_is_not_miscategorised_backwards() {
        // The shop sold pies until 2025 and umbrellas after. One string, two
        // meanings — the same problem instrument aliases solve with an interval
        // (crates/iaam-ingest/src/csv_source.rs:47).
        let pies = 10;
        let umbrellas = 20;
        let rules = vec![
            rule(1, 1, CategoryMatcher::DescriptionContains { text: "ЛАВКА".into() },
                 None, Some(date!(2025 - 12 - 31)), pies),
            rule(2, 2, CategoryMatcher::DescriptionContains { text: "ЛАВКА".into() },
                 Some(date!(2026 - 01 - 01)), None, umbrellas),
        ];
        let subject = |on| CategorySubject { source_category: None, counterparty: None,
                                             description: Some("Лавка на углу"), on };

        assert!(matches!(
            assign(&subject(date!(2024 - 06 - 01)), &rules),
            CategoryAssignment::Assigned { category, .. } if category == CategoryId(uuid::Uuid::from_u128(pies))
        ));
        assert!(matches!(
            assign(&subject(date!(2026 - 08 - 01)), &rules),
            CategoryAssignment::Assigned { category, .. } if category == CategoryId(uuid::Uuid::from_u128(umbrellas))
        ));
    }

    #[test]
    fn a_hand_made_row_decision_outranks_a_later_blanket_rule() {
        // R12: the owner said what this particular purchase was. A rule written
        // afterwards about the same merchant must not overwrite that.
        let by_hand = 10;
        let blanket = 20;
        let rules = vec![
            rule(1, 1, CategoryMatcher::Row { key: "row-7".into() }, None, None, by_hand),
            rule(2, 9, CategoryMatcher::DescriptionContains { text: "ЛАВКА".into() }, None, None, blanket),
        ];
        let subject = CategorySubject {
            source_category: None,
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        // The row key reaches `assign` through the subject; see Step 3.
        assert!(matches!(
            assign_with_row(&subject, Some("row-7"), &rules),
            CategoryAssignment::Assigned { basis: CategoryBasis::Row { .. }, category }
                if category == CategoryId(uuid::Uuid::from_u128(by_hand))
        ));
    }

    #[test]
    fn the_sources_category_outranks_a_description_rule() {
        let from_source = 10;
        let from_text = 20;
        let rules = vec![
            rule(1, 1, CategoryMatcher::SourceCategory { value: "Супермаркеты".into() }, None, None, from_source),
            rule(2, 5, CategoryMatcher::DescriptionContains { text: "ЛАВКА".into() }, None, None, from_text),
        ];
        let subject = CategorySubject {
            source_category: Some("Супермаркеты"),
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        assert!(matches!(
            assign(&subject, &rules),
            CategoryAssignment::Assigned { basis: CategoryBasis::SourceCategory { .. }, category }
                if category == CategoryId(uuid::Uuid::from_u128(from_source))
        ));
    }

    #[test]
    fn a_row_no_rule_covers_is_not_decomposed_rather_than_bucketed() {
        let rules = vec![];
        let subject = CategorySubject {
            source_category: Some("Супермаркеты"),
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        // No "Other". A silent catch-all is how a decomposition stops meaning
        // anything, which is exactly the state Actual Budget's categories are in.
        assert!(matches!(assign(&subject, &rules), CategoryAssignment::NotDecomposed));
    }

    #[test]
    fn an_empty_matcher_matches_nothing() {
        let rules = vec![
            rule(1, 1, CategoryMatcher::DescriptionContains { text: String::new() }, None, None, 10),
        ];
        let subject = CategorySubject {
            source_category: None,
            counterparty: None,
            description: Some("Лавка на углу"),
            on: date!(2026 - 08 - 01),
        };
        assert!(matches!(assign(&subject, &rules), CategoryAssignment::NotDecomposed));
    }

    #[test]
    fn among_rules_of_one_kind_the_highest_version_wins() {
        let old = 10;
        let new = 20;
        let rules = vec![
            rule(1, 1, CategoryMatcher::SourceCategory { value: "Супермаркеты".into() }, None, None, old),
            rule(2, 2, CategoryMatcher::SourceCategory { value: "Супермаркеты".into() }, None, None, new),
        ];
        let subject = CategorySubject {
            source_category: Some("Супермаркеты"),
            counterparty: None,
            description: Some(""),
            on: date!(2026 - 08 - 01),
        };
        assert!(matches!(
            assign(&subject, &rules),
            CategoryAssignment::Assigned { category, .. } if category == CategoryId(uuid::Uuid::from_u128(new))
        ));
    }
```

The `assign_with_row` helper in the second test signals a design choice you must
make in Step 3: the row key belongs in `CategorySubject`. Put it there as
`pub row_key: Option<&'a str>` and drop the helper — the test above is written
with it only to make the precedence explicit. Fix the test to use the field.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `direnv exec $WORKTREE cargo test -p iaam-core category::`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Write the implementation**

Add the three ids to `crates/iaam-core/src/ids.rs` via `typed_id!`, beside
`ClassificationRuleId`. Create `crates/iaam-core/src/category.rs` with a module
doc comment stating the two things a reader must not miss:

```rust
//! Owner categories: what the money went to (spec §3).
//!
//! **A category is not a field on an event and never becomes one.** It is
//! derived here from versioned rules over the row's attributes and its date, so
//! renaming, splitting, merging and retiring categories touch reference data
//! only. Had the category been written onto the event, every reorganisation
//! would demand a journal migration and the owner would stop reorganising —
//! which is how a category list ossifies into one nobody opens.
//!
//! This is a different question from `iaam_ingest::classification`, which
//! answers "what kind of operation is this". A rule reading "Corner Shop →
//! Продукты" says nothing about whether a row is a fee or a withdrawal, and the
//! two must not share a type.
//!
//! **Every rule is valid over an interval.** A merchant that sold pies in 2024
//! and umbrellas in 2026 is one string with two meanings; a rule claiming to
//! hold forever misclassifies half of history. Instrument aliases already solve
//! exactly this, with the reasoning at `crates/iaam-ingest/src/csv_source.rs:47`.
```

Implement `assign` as three ordered passes — `Row`, then `SourceCategory`, then
`DescriptionContains` — each filtering by `interval.covers(subject.on)` and
taking `max_by_key(|rule| rule.version)`. Return on the first pass that matches.
Write the passes out; do **not** collapse them into one sorted comparison, or
the precedence stops being readable and a later edit will invert it silently.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `direnv exec $WORKTREE cargo test -p iaam-core category::`
Expected: PASS, 6 tests.

- [ ] **Step 5: Check the crate compiles**

Run: `direnv exec $WORKTREE cargo check -p iaam-core`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-core/src/category.rs crates/iaam-core/src/lib.rs crates/iaam-core/src/ids.rs
git commit -m "feat(core): a category rule, valid over an interval"
```

---

### Task 4: Category rules persist, versioned like classification rules

**Files:**
- Modify: `crates/iaam-store/migrations/0015_categories.sql` (add the rules table)
- Modify: `crates/iaam-store/src/categories.rs`
- Test: `crates/iaam-store/tests/categories.rs`

**Interfaces:**
- Produces: `insert_category_rule(owner, matcher_json, category, from, to, replaces) -> CategoryRuleRow`,
  `list_category_rules(owner) -> Vec<CategoryRuleRow>`, `retire_category_rule(owner, id)`.

**Acceptance Criteria:**
- The version number is unique per owner, exactly as
  `classification_rules_by_version` enforces: without that, two concurrent
  requests take the same number and the order of rules stops being an order.
- Editing a rule inserts a new row referencing the previous one through
  `replaces`, so "how did this rule reach its present form" has an answer.
- A retired rule is still listed, flagged, because a past report was computed
  under it.

- [ ] **Step 1: Extend the migration**

Append to `0015_categories.sql`, mirroring `classification_rules`:

```sql
CREATE TABLE category_rules (
    id          TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    version     INTEGER NOT NULL,
    matcher     TEXT NOT NULL,
    category    TEXT NOT NULL REFERENCES categories (id),
    -- Validity interval. NULL means open at that end. A rule that claims to
    -- hold forever miscategorises a merchant that changed its trade.
    valid_from  TEXT,
    valid_to    TEXT,
    created_at  TEXT NOT NULL,
    retired_at  TEXT,
    replaces    TEXT REFERENCES category_rules (id)
) STRICT;

CREATE UNIQUE INDEX category_rules_by_version ON category_rules (owner, version);
CREATE INDEX category_rules_by_owner ON category_rules (owner, retired_at);
```

- [ ] **Step 2: Write the failing test**

Append to `crates/iaam-store/tests/categories.rs`:

```rust
#[test]
fn two_rules_cannot_share_a_version_number() {
    let store = open_temp_store();
    let owner = OwnerId::new_random();
    let group = store.insert_category_group(owner, "Usual Expenses").expect("group");
    let food = store.insert_category(owner, group, "Продукты").expect("category");

    let first = store.insert_category_rule(owner, r#"{"SourceCategory":{"value":"Супермаркеты"}}"#, food, None, None, None).expect("first");
    let second = store.insert_category_rule(owner, r#"{"DescriptionContains":{"text":"ЛАВКА"}}"#, food, None, None, None).expect("second");
    // Without a unique version per owner, two concurrent requests take the same
    // number and the order of rules stops being an order.
    assert_ne!(first.version, second.version);
}

#[test]
fn an_edited_rule_points_at_the_one_it_replaces() {
    let store = open_temp_store();
    let owner = OwnerId::new_random();
    let group = store.insert_category_group(owner, "Usual Expenses").expect("group");
    let food = store.insert_category(owner, group, "Продукты").expect("category");
    let first = store.insert_category_rule(owner, r#"{"SourceCategory":{"value":"Супермаркеты"}}"#, food, None, None, None).expect("first");
    let second = store.insert_category_rule(owner, r#"{"SourceCategory":{"value":"Супермаркет"}}"#, food, None, None, Some(first.id)).expect("second");
    assert_eq!(second.replaces, Some(first.id));
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `direnv exec $WORKTREE cargo test -p iaam-store --test categories two_rules_cannot_share`
Expected: FAIL — the method does not exist.

- [ ] **Step 4: Write the implementation**

Add the row type and three methods to `crates/iaam-store/src/categories.rs`,
allocating the version as `classification_rules` does — read that code first and
use the same allocation, so the two rule tables cannot drift into different
concurrency behaviour.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `direnv exec $WORKTREE cargo test -p iaam-store --test categories`
Expected: PASS, 3 tests.

- [ ] **Step 6: Check the crate compiles and commit**

Run: `direnv exec $WORKTREE cargo check -p iaam-store`

```bash
git add crates/iaam-store/
git commit -m "feat(store): category rules, versioned and intervalled"
```

---

### Task 5: The outflow decomposes, and what it cannot decompose it names

**Files:**
- Modify: `crates/iaam-core/src/projection/money_flow.rs`
- Test: same file

**Interfaces:**
- `MoneyFlow::apply` gains a `categories: &CategoryIndex` parameter, where
  `CategoryIndex` resolves an event to a `CategoryAssignment` — define it in
  `money_flow.rs` as a small trait so core stays free of storage.
- Produces: `MoneyFlow::went_out_by_category(&self, currency) -> Result<Vec<(CategoryId, Money)>, MoneyFlowError>`
  and `MoneyFlow::not_decomposed(&self, currency) -> Result<(u64, Money), MoneyFlowError>`.

**Acceptance Criteria:**
- Only the **outflow** decomposes. Inflows, internal transfers, fees, taxes and
  asset movements keep their own lines and are not given categories: a category
  answers "what did I spend it on", and asking it of a transfer to one's own
  deposit is what made `Переводы` a spending category in Actual Budget.
- The sum of the per-category amounts plus the not-decomposed amount equals
  `went_out` exactly, per currency. Assert this — it is the decomposition's own
  identity, and without it a rounding or filtering bug hides.
- `not_decomposed` reports both a **count of rows** and an **amount**. A count
  alone hides one large unclassified purchase among many small ones; an amount
  alone hides a hundred tiny ones.
- The existing six quantities, the residual and `residuals_by_account` are
  unchanged. Re-run the whole existing `money_flow` suite to prove it.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn the_decomposition_sums_to_the_outflow_it_decomposes() {
        // The decomposition's own identity. Without asserting it, a filtering
        // bug drops a row from the breakdown while the headline stays right,
        // and the two numbers disagree with nobody noticing.
        let card = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let food = CategoryId(uuid::Uuid::from_u128(10));
        let index = FixedIndex::new(vec![
            ("row-1", CategoryAssignment::Assigned { category: food, basis: CategoryBasis::SourceCategory { rule: CategoryRuleId(uuid::Uuid::from_u128(1)) } }),
            ("row-2", CategoryAssignment::NotDecomposed),
        ]);
        let mut flow = MoneyFlow::new();
        for (row, amount) in [("row-1", rub(-30_000)), ("row-2", rub(-12_000))] {
            flow.apply(&outflow(card, row, amount), &contour, august(), &index)
                .expect("applies");
        }

        let by_category = flow.went_out_by_category(CurrencyCode::Rub).expect("fits");
        let (count, undecomposed) = flow.not_decomposed(CurrencyCode::Rub).expect("fits");
        let decomposed: i64 = by_category.iter().map(|(_, money)| money.amount().raw()).sum();

        assert_eq!(count, 1);
        assert_eq!(undecomposed.amount().raw(), 12_000);
        assert_eq!(
            decomposed + undecomposed.amount().raw(),
            flow.went_out(CurrencyCode::Rub).expect("fits").amount().raw()
        );
    }

    #[test]
    fn an_internal_transfer_is_never_given_a_category() {
        // Asking "what did I spend it on" of a transfer to one's own deposit is
        // exactly what made `Переводы` a spending category in the tool being
        // replaced.
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card, deposit]);
        let food = CategoryId(uuid::Uuid::from_u128(10));
        // An index that would categorise everything it is asked about.
        let index = AlwaysIndex(food);
        let mut flow = MoneyFlow::new();
        flow.apply(&transfer(card, deposit, rub(480_000)), &contour, august(), &index)
            .expect("applies");
        assert!(flow.went_out_by_category(CurrencyCode::Rub).expect("fits").is_empty());
        let (count, amount) = flow.not_decomposed(CurrencyCode::Rub).expect("fits");
        assert_eq!(count, 0);
        assert_eq!(amount.amount().raw(), 0);
    }
```

`FixedIndex`, `AlwaysIndex`, `outflow` and `transfer` are test helpers you write
in the same module; build events with
`crate::event::test_support::event_with`, as the existing tests there do.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `direnv exec $WORKTREE cargo test -p iaam-core projection::money_flow::tests::the_decomposition_sums`
Expected: FAIL — `went_out_by_category` does not exist.

- [ ] **Step 3: Write the implementation**

Add a `Ledger`-shaped map keyed by `(CategoryId, CurrencyCode)` and a
`(count, Ledger)` pair for the undecomposed rows. Accumulate **only** in the
`EventKind::CashOut` arm of the existing match — every other arm is untouched,
and that is the point. Keep `#[must_use]` off anything returning `Result`.

- [ ] **Step 4: Run the tests to verify they pass, and the old ones still do**

Run: `direnv exec $WORKTREE cargo test -p iaam-core projection::money_flow`
Expected: PASS — the two new tests plus all 12 existing ones.

Run: `direnv exec $WORKTREE cargo test -p iaam-core projection::flows`
Expected: PASS, 10 tests. The returns path stays untouched.

- [ ] **Step 5: Check the crate compiles and commit**

Run: `direnv exec $WORKTREE cargo check -p iaam-core`

```bash
git add crates/iaam-core/src/projection/money_flow.rs
git commit -m "feat(core): the outflow decomposes, and names what it cannot"
```

---

### Task 6: The scenarios — categories, rules, and the report that uses them

**Files:**
- Create: `crates/iaam-app/src/scenarios/categories.rs`
- Modify: `crates/iaam-app/src/scenarios/mod.rs`, `crates/iaam-app/src/ports.rs`,
  `crates/iaam-app/src/adapters/sqlite.rs`, `crates/iaam-app/src/scenarios/reports.rs`
- Test: `crates/iaam-app/tests/categories.rs` (new)

**Interfaces:**
- `CategoryStore` port mirroring `ClassificationRuleStore`'s shape.
- `list_categories`, `create_category`, `retire_category`, `create_category_rule`.
- `money_flow` loads the owner's category rules, builds the index, and passes it
  to `MoneyFlow::apply`.

**Acceptance Criteria:**
- The flow report's decomposition reflects the owner's rules, end to end from
  the store.
- A rule whose interval excludes the report's month does not affect it.
- Creating a category under a retired group is refused with `AppError::Invalid`
  naming the field — a new assignment into a retired branch of the tree is a
  mistake, not a resurrection.
- The report names the **rule version set** it used, alongside the contour
  version it already names. Two reports of the same month under different rules
  are different reports, and a reader cannot tell them apart otherwise.

- [ ] **Step 1: Write the failing test**

Create `crates/iaam-app/tests/categories.rs`, copying the harness pattern from
`crates/iaam-app/tests/money_flow.rs` (which E5 added) — `principal(owner)`,
`services(...)`, `append(...)`, plus `insert_contour_version`.

```rust
/// Two August outflows on one card: one the source called "Супермаркеты",
/// one it named nothing.
async fn august_card(ctx: &Ctx) -> (AccountId, ContourId) {
    let card = ctx.account("Card").await;
    let contour = ctx.contour(&[card]).await;
    ctx.submit_outflow(card, 30_000, "2026-08-05", Some("Супермаркеты")).await;
    ctx.submit_outflow(card, 12_000, "2026-08-12", None).await;
    (card, contour)
}

#[tokio::test]
async fn the_flow_report_decomposes_by_the_owners_rules() {
    let ctx = harness().await;
    let (_card, contour) = august_card(&ctx).await;
    let group = ctx.create_group("Usual Expenses").await;
    let food = ctx.create_category(group, "Продукты").await;
    ctx.create_rule_on_source_category("Супермаркеты", food, None, None).await;

    let report = money_flow(&ctx.services, &ctx.principal, &MoneyFlowQuery {
        contour, contour_version: None,
        from: date!(2026 - 08 - 01), to: date!(2026 - 08 - 31),
    }).await.expect("report");

    let by_category = report.flow.went_out_by_category(CurrencyCode::Rub).expect("fits");
    assert_eq!(by_category, vec![(food, Money::new(PostedMinor::new(30_000), CurrencyCode::Rub))]);

    // The row the rules could not place is named, not folded into the one they
    // could. A silent catch-all is how a decomposition stops being informative.
    let (rows, amount) = report.flow.not_decomposed(CurrencyCode::Rub).expect("fits");
    assert_eq!(rows, 1);
    assert_eq!(amount.amount().raw(), 12_000);
}

#[tokio::test]
async fn a_rule_outside_the_month_does_not_touch_it() {
    let ctx = harness().await;
    let (_card, contour) = august_card(&ctx).await;
    let group = ctx.create_group("Usual Expenses").await;
    let food = ctx.create_category(group, "Продукты").await;
    // Valid only through July: the merchant meant something else in August.
    ctx.create_rule_on_source_category(
        "Супермаркеты", food, None, Some(date!(2026 - 07 - 31)),
    ).await;

    let report = money_flow(&ctx.services, &ctx.principal, &MoneyFlowQuery {
        contour, contour_version: None,
        from: date!(2026 - 08 - 01), to: date!(2026 - 08 - 31),
    }).await.expect("report");

    assert!(report.flow.went_out_by_category(CurrencyCode::Rub).expect("fits").is_empty());
    let (rows, amount) = report.flow.not_decomposed(CurrencyCode::Rub).expect("fits");
    assert_eq!(rows, 2);
    assert_eq!(amount.amount().raw(), 42_000);
}

#[tokio::test]
async fn a_category_cannot_be_created_under_a_retired_group() {
    let ctx = harness().await;
    let group = ctx.create_group("Usual Expenses").await;
    ctx.retire_group(group).await;
    // Retiring keeps history readable; it must not keep accepting new
    // assignments into a branch the owner has closed.
    let error = create_category(&ctx.services, &ctx.principal, group, "Продукты")
        .await
        .expect_err("refused");
    assert!(matches!(error, AppError::Invalid { ref field, .. } if field == "group"));
}
```

`Ctx` and its helpers are yours to write in this file, following
`crates/iaam-app/tests/money_flow.rs`, which E5 added and which already builds
accounts, a contour and operations. `submit_outflow` normalises a
`SubmittedOperation` carrying `source_category` and appends it. Do not add a
shared harness module — this crate's tests each carry their own by convention.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `direnv exec $WORKTREE cargo test -p iaam-app --test categories`
Expected: FAIL — the scenario does not exist.

- [ ] **Step 3: Write the implementation**

Follow how `crates/iaam-app/src/scenarios/classification.rs` reaches its rule
store and how `reports.rs::money_flow` resolves the contour. Add the port to
`ports.rs` with an `Unavailable…` implementation beside the existing ones, and
wire the SQLite adapter.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `direnv exec $WORKTREE cargo test -p iaam-app --test categories`, then
`direnv exec $WORKTREE cargo test -p iaam-app --test money_flow` to prove E5's
scenarios still hold.
Expected: PASS.

- [ ] **Step 5: Check the crates compile and commit**

Run: `direnv exec $WORKTREE cargo check -p iaam-app`, then `-p iaam-server`.

```bash
git add crates/iaam-app/
git commit -m "feat(app): categories, their rules, and the report that uses them"
```

---

### Task 7: A rule change shows what it moved, before it stands

R12. This is the task that answers the owner's own objection — "я же потом не
буду помнить" — and it is the reason retroactive recomputation is safe here.

**Files:**
- Modify: `crates/iaam-app/src/scenarios/categories.rs`
- Test: `crates/iaam-app/tests/categories.rs`

**Interfaces:**

```rust
pub struct CategoryRuleImpact {
    pub rows: u64,
    /// By month, oldest first: what moved, and between which categories.
    pub months: Vec<MonthlyImpact>,
}
pub struct MonthlyImpact {
    pub month: Date,          // first day of the month
    pub moved: Vec<CategoryMove>,
}
pub struct CategoryMove {
    pub from: Option<CategoryId>,   // None = was not decomposed
    pub to: CategoryId,
    pub amount: Money,
    pub rows: u64,
}

pub async fn preview_category_rule(
    services: &AppServices, principal: &Principal, proposed: &CategoryRule,
) -> Result<CategoryRuleImpact, AppError>;
```

**Acceptance Criteria:**
- The preview is **read-only**. It appends nothing to the journal and writes no
  rule. Assert that the store holds the same number of rules before and after.
- A rule that changes nothing yields `rows: 0` and an empty `months`.
- A rule that recategorises past rows reports them by month, with the amount and
  the pair of categories, including rows that were previously not decomposed
  (`from: None`).
- The months are ordered oldest first, so a reader sees when the change begins.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_preview_reports_what_would_move_and_writes_nothing() {
    // The owner will not remember, in a year, that the shop changed its trade.
    // He does not have to: he sees the numbers move at the moment they move.
    // …build two months of outflows, one existing rule, then preview a second…
    let before = services.store.list_category_rules(owner).await.expect("rules").len();
    let impact = preview_category_rule(&services, &principal, &proposed).await.expect("preview");
    let after = services.store.list_category_rules(owner).await.expect("rules").len();

    assert_eq!(before, after, "a preview must not write a rule");
    assert_eq!(impact.rows, 3);
    assert_eq!(impact.months.len(), 2);
    assert_eq!(impact.months[0].month, date!(2026 - 07 - 01));
    assert_eq!(impact.months[1].month, date!(2026 - 08 - 01));
}
```

Write the setup out in full.

- [ ] **Step 2: Run the test to verify it fails**

Run: `direnv exec $WORKTREE cargo test -p iaam-app --test categories a_preview_reports`
Expected: FAIL — `preview_category_rule` does not exist.

- [ ] **Step 3: Write the implementation**

Compute the assignment for every outflow in the journal twice — under the
current rules, and under the current rules plus the proposed one — and diff. Do
**not** persist anything. Group by calendar month of the event's effective date.

- [ ] **Step 4: Run the test to verify it passes**

Run: `direnv exec $WORKTREE cargo test -p iaam-app --test categories a_preview_reports`
Expected: PASS.

- [ ] **Step 5: Check the crate compiles and commit**

Run: `direnv exec $WORKTREE cargo check -p iaam-app`

```bash
git add crates/iaam-app/src/scenarios/categories.rs crates/iaam-app/tests/categories.rs
git commit -m "feat(app): a category rule shows what it would move before it stands"
```

---

### Task 8: The routes

**Files:**
- Modify: `crates/iaam-server/src/dto.rs`, `routes.rs`, `lib.rs`
- Test: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- `GET /v1/categories`, `POST /v1/categories`, `DELETE /v1/categories/{id}` (retire)
- `GET /v1/category-rules`, `POST /v1/category-rules`
- `POST /v1/category-rules/preview` → `CategoryRuleImpactDto`
- `GET /v1/reports/flow` gains `went_out_by_category` and `not_decomposed`

**Acceptance Criteria:**
- Every route is inside `protected` and requires authentication.
- `GET /v1/reports/flow` carries the decomposition **and** the not-decomposed
  count and amount, and names the rule version set it used.
- `DELETE /v1/categories/{id}` retires and does not delete; a subsequent
  `GET /v1/categories` still lists it, flagged.
- `POST /v1/category-rules/preview` writes nothing — assert the rule list is
  unchanged after the call.
- `every_documented_path_answers_something_other_than_404` still passes.
- The frozen returns snapshot does not change.

- [ ] **Step 1–5**

Follow Task 7 of the E5 plan (`.internal/plans/2026-09-01-money-flow-p1-august-visible.md`)
step for step: it is the same shape of work against the same helpers. There is
no `MoneyDto` — amounts go out as strings; `CurrencyDto` exists at
`crates/iaam-server/src/dto.rs:55`. Tests go in `contract.rs` with
`harness()` / `harness_on_disk()`, `post`, `get`, `call`.

Run, one filter per invocation:

```
direnv exec $WORKTREE cargo test -p iaam-server --test contract categories
direnv exec $WORKTREE cargo test -p iaam-server --test contract category_rules
direnv exec $WORKTREE cargo test -p iaam-server --test contract flow_report
direnv exec $WORKTREE cargo test -p iaam-server --test contract every_documented_path
direnv exec $WORKTREE cargo check -p iaam-server
```

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-server/
git commit -m "feat(server): category routes and the decomposed flow report"
```

---

## Closing the epic

The orchestrator — not the task workers — runs the gates once:

```bash
make check
make diff-coverage BASE=main
```

Then the acceptance test for the whole epic: re-run August's flow report and read
the decomposition. **The number to watch is `not_decomposed`.** If it is a large
share of the outflow, the rules are too few, not the report wrong — and the
report saying so plainly is the whole point.
