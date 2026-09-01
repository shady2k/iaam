# E4 — a coverage gap names its rows: implementation plan

> **For agentic workers:** each Task below is one bead and one worker. Steps use
> `- [ ]` for human readability; tracking is in beads. Do not run repo-wide
> gates or the full test suite — see Global Constraints.

**Goal:** a coverage gap stops applying when the rows it named have been
explicitly disposed of, so an import we fixed on our side recovers without a
parser version change, while re-running an unchanged import recovers nothing.

**Architecture:** the gap carries a list of refused source rows, each with a
`SourceRowKey` and the dimensions it alone cannot confirm. A new journal fact,
`EventKind::ImportRowResolution`, disposes of a row — `Supplied { events }` or
`NoFact { classification }`. Reconciliation treats outstanding rows as a
first-class ledger constraint over `(account, period, dimension)`, dropping the
old correlation on source and parser version entirely.

**Tech Stack:** Rust 2024, workspace of ten crates. `serde` for the journal
payload (JSON in a single SQLite column, so new optional fields need no SQL
migration). Tests are `#[test]` unit tests beside the code plus integration
tests under `crates/<crate>/tests/`.

**Spec:** `.internal/specs/2026-09-01-e4-coverage-gap-names-its-rows-design.md`
— read it before starting any task. It records two rejected designs and why;
do not re-propose them.

## Global Constraints

- **English only** in everything new: identifiers, test names, doc comments,
  inline comments, `#[error(...)]` text. Existing Russian comments are left
  alone, not retranslated. Domain terms come from `docs/glossary-ru-en.md`.
- `unsafe_code = "forbid"`, `clippy::all` denied at workspace level.
- **Validation logic never goes in a function named `new`** — `cargo-mutants`
  skips those, so the mutation gate would not see it (§15.7; see the comment at
  `crates/iaam-core/src/event/provenance.rs:17`).
- **Workers run targeted tests only.** `cargo test -p <crate> <filter>` and
  `cargo check -p <crate>`. Do **not** run `make check`, the full suite,
  `cargo-mutants`, or any formatter — the orchestrator runs those once at the
  end of the epic. A repo-wide gate in a shared tree observes other work in
  progress and produces phantom failures.
- **`cargo check -p <crate>` is mandatory before claiming a task done.** It is
  not a repo-wide gate; it is how you learn that an added `EventKind` variant
  broke an exhaustive match you did not know about.
- **`tests/fixtures/` is policy-gated.** Changing anything under it requires a
  separate commit with `POLICY_CHANGE_APPROVED=1` and the `policy-change` label
  (`scripts/check-diff-lint.sh`). No task below needs it; if you think yours
  does, stop and escalate.
- Do not touch the issue tracker. Do not commit, push, or branch unless the
  task says so.

## Where the exhaustive matches are

Adding an `EventKind` variant (Task 2) fails to compile at every exhaustive
match. These are the ones present today — treat the list as a starting point
and let `cargo check -p` find the rest, because the count has grown before:

| File | Line | What it decides |
|---|---|---|
| `crates/iaam-core/src/event/kind.rs` | 282 | `discriminant()` — the stored kind string |
| `crates/iaam-core/src/event/kind.rs` | 309 | money movement endpoints |
| `crates/iaam-core/src/event/mod.rs` | 236 | `validate_structure` |
| `crates/iaam-core/src/projection/active_instruments.rs` | 108 | instruments touched |
| `crates/iaam-core/src/projection/income.rs` | 161 | income projection |
| `crates/iaam-core/src/projection/lots.rs` | 713 | lot projection |
| `crates/iaam-ingest/src/classification.rs` | 274 | contour classification |
| `crates/iaam-app/src/scenarios/classification.rs` | 181 | classification scenario |

`ImportRowResolution` is projection-inert everywhere: it has no legs, moves no
money, touches no instrument. Every arm above gets the same treatment
`ImportCoverageGap` already has.

---

### Task 1: `SourceRowKey`, and the gap carries its rows

**Files:**
- Create: `crates/iaam-core/src/event/source_row.rs`
- Modify: `crates/iaam-core/src/event/mod.rs` — the `mod` list, `SCHEMA_VERSION`
  (line 169-170), the `validate_structure` arm (line 279), and
  `validate_import_coverage_gap` (line 739)
- Modify: `crates/iaam-core/src/event/kind.rs:231` — the `ImportCoverageGap`
  variant
- Test: `crates/iaam-core/src/event/source_row.rs` (unit tests in-file, as
  `provenance.rs` does), and `crates/iaam-core/src/event/mod.rs` tests module

**Interfaces:**
- Produces: `iaam_core::event::source_row::{SourceRowKey, RowName, RefusedRow}`;
  `EventKind::ImportCoverageGap` gains `rows: Vec<RefusedRow>`.
- Consumes: nothing.

**Acceptance Criteria:**
- A row the source identified and a row keyed by fingerprint are distinguishable
  and both round-trip through serde.
- A schema-8 gap whose row dimensions do not union to `dimensions` is refused.
- A schema-8 gap whose `rows.len()` disagrees with `refused` is refused.
- A schema-7 gap with no `rows` still validates and still deserialises.
- `SCHEMA_VERSION` is 8 and the migration comment records why.

- [ ] **Step 1: Write the failing tests**

In the new file `crates/iaam-core/src/event/source_row.rs`:

```rust
//! Identity of a row as the source presented it (§10.3).
//!
//! Deliberately distinct from [`crate::event::provenance::Provenance::source_operation_id`],
//! which identifies an EVENT. One source row can expand into several events —
//! a trade order becomes one event per fill — so an event identifier cannot
//! answer "is this row represented".

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::SourceId;
use crate::reconciliation::Dimension;

/// How a refused row is named.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RowName {
    /// The identifier the source gave the row.
    Given(String),
    /// The source gave none: a hexadecimal SHA-256 of the row's raw payload.
    Fingerprint(String),
}

/// Identity of a source row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceRowKey {
    pub source: SourceId,
    pub row: RowName,
}

/// A row an import refused, and what it alone cannot confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedRow {
    pub key: SourceRowKey,
    pub dimensions: BTreeSet<Dimension>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_given_name_and_a_fingerprint_of_the_same_text_are_different_rows() {
        let source = SourceId::new_random();
        let given = SourceRowKey {
            source,
            row: RowName::Given("OP-1".to_owned()),
        };
        let fingerprint = SourceRowKey {
            source,
            row: RowName::Fingerprint("OP-1".to_owned()),
        };
        assert_ne!(given, fingerprint);
    }

    #[test]
    fn a_row_key_round_trips_through_serde() {
        let key = SourceRowKey {
            source: SourceId::new_random(),
            row: RowName::Given("OP-1".to_owned()),
        };
        let json = serde_json::to_string(&key).unwrap();
        let back: SourceRowKey = serde_json::from_str(&json).unwrap();
        assert_eq!(key, back);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p iaam-core source_row`
Expected: FAIL — the module is not declared in `crates/iaam-core/src/event/mod.rs`.

- [ ] **Step 3: Declare the module and widen the variant**

Add `pub mod source_row;` to the module list in
`crates/iaam-core/src/event/mod.rs` beside the existing `pub mod provenance;`.

In `crates/iaam-core/src/event/kind.rs:231`, the variant becomes:

```rust
    ImportCoverageGap {
        period: AssertionPeriod,
        /// What this attempt cannot confirm. Never empty—a gap that taints
        /// nothing is not a fact.
        dimensions: BTreeSet<Dimension>,
        /// How many rows were refused. Carried for the owner, not for the rule.
        refused: u32,
        /// The rows themselves. Empty only in records written before schema 8:
        /// such a gap taints its whole `dimensions` and is never lifted
        /// automatically, because it cannot say what is missing.
        #[serde(default)]
        rows: Vec<crate::event::source_row::RefusedRow>,
    },
```

- [ ] **Step 4: Run `cargo check` and fix every construction site**

Run: `cargo check -p iaam-core`
Expected: errors at every `ImportCoverageGap { .. }` construction — the struct
literal now misses a field. Pattern matches using `..` are unaffected.

Fix each by adding `rows: Vec::new()` where the site predates this change, and
leave a note in the plan's Task 4 for the one site that will really fill it
(`crates/iaam-app/src/scenarios/sync.rs:381`).

- [ ] **Step 5: Write the failing validation tests**

In the tests module of `crates/iaam-core/src/event/mod.rs`, beside the existing
gap tests near line 2616:

```rust
    #[test]
    fn a_schema_eight_gap_whose_rows_do_not_cover_its_dimensions_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 8;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash, Dimension::Positions].into_iter().collect(),
            refused: 1,
            rows: vec![RefusedRow {
                key: row_key("OP-1"),
                dimensions: [Dimension::Cash].into_iter().collect(),
            }],
        };
        assert!(event.validate_structure().is_err());
    }

    #[test]
    fn a_schema_eight_gap_whose_row_count_disagrees_with_refused_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 8;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash].into_iter().collect(),
            refused: 2,
            rows: vec![RefusedRow {
                key: row_key("OP-1"),
                dimensions: [Dimension::Cash].into_iter().collect(),
            }],
        };
        assert!(event.validate_structure().is_err());
    }

    #[test]
    fn a_legacy_gap_without_rows_still_validates() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 7;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash].into_iter().collect(),
            refused: 1,
            rows: Vec::new(),
        };
        assert!(event.validate_structure().is_ok());
    }

    #[test]
    fn a_schema_eight_gap_without_rows_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.schema_version = 8;
        event.legs = Vec::new();
        event.kind = EventKind::ImportCoverageGap {
            period: march_period(),
            dimensions: [Dimension::Cash].into_iter().collect(),
            refused: 1,
            rows: Vec::new(),
        };
        assert!(event.validate_structure().is_err());
    }
```

Write `march_period()` and `row_key(&str)` as small helpers in the same tests
module; follow the construction already used by the neighbouring gap tests at
`crates/iaam-core/src/event/mod.rs:2616`.

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test -p iaam-core schema_eight`
Expected: the two rejection tests FAIL (validation still passes them).

- [ ] **Step 7: Make validation schema-aware**

`validate_structure` at `crates/iaam-core/src/event/mod.rs:279` passes the new
field through, and `validate_import_coverage_gap` (line 739) gains the rule.
The existing checks — well-formed period, `refused >= 1`, non-empty
`dimensions`, no legs — stay exactly as they are. Append:

```rust
        // Schema-aware on purpose. `validate_structure` runs on the READ path
        // too: the projection re-checks every effective event because the core
        // does not trust storage it did not write (crates/iaam-core/src/
        // projection/invariants.rs). Refusing an empty `rows` outright would
        // make every report fail on a journal that holds a gap written before
        // schema 8.
        if self.schema_version < 8 {
            return Ok(());
        }
        if rows.is_empty() {
            return Err(EventValidationError::EmptySet {
                kind: name,
                field: "rows",
            });
        }
        if rows.len() != refused as usize {
            return Err(EventValidationError::NonPositive {
                kind: name,
                field: "refused",
                value: format!("{refused} declared, {} rows listed", rows.len()),
            });
        }
        let union: std::collections::BTreeSet<crate::reconciliation::Dimension> =
            rows.iter().flat_map(|row| row.dimensions.iter().copied()).collect();
        if &union != dimensions {
            return Err(EventValidationError::EmptySet {
                kind: name,
                field: "rows",
            });
        }
        Ok(())
```

If `EventValidationError` has no variant that reads well for "the rows disagree
with the union", add one rather than reusing `EmptySet` for a non-empty set —
an error message that lies is worse than a new variant. Follow the existing
variants' shape in the same file.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p iaam-core -- import_coverage_gap schema_eight legacy_gap`
Expected: PASS.

- [ ] **Step 9: Bump the schema version**

`crates/iaam-core/src/event/mod.rs:169-170`:

```rust
/// Version 7 adds [`EventKind::ImportCoverageGap`].
/// Version 8 adds the refused rows inside that variant and the variant
/// [`EventKind::ImportRowResolution`]: a coverage gap now says WHICH rows are
/// missing, and a row is disposed of by an explicit fact rather than inferred
/// from the presence of an event.
pub const SCHEMA_VERSION: u32 = 8;
```

And the migration-comment test near line 2831 gains its line:

```rust
        // 7 → 8: `ImportCoverageGap` gained `rows`; added
        //        `EventKind::ImportRowResolution` (§10.3).
        assert_eq!(SCHEMA_VERSION, 8);
```

- [ ] **Step 10: Close the legacy allowance on the write path**

Step 7 lets a gap skip the row rules when `schema_version < 8`. Nothing today
stops a caller from *writing* an event stamped 7 and claiming that allowance,
because the write gate never checks the version. Add the check where events are
validated before being appended — the same gate `validate_structure` is called
from in `crates/iaam-app/src/scenarios/ingest.rs` (`append_checked`):

```rust
    // A newly written event may not claim a version other than the one this
    // build produces. Without this, the schema-aware allowance in
    // `validate_import_coverage_gap` becomes a way to write a gap that names
    // no rows and can never be lifted.
    if event.schema_version != SCHEMA_VERSION {
        return Err(/* the gate's existing rejection type */);
    }
```

Find the exact call site with:

```bash
grep -rn "validate_structure" crates/iaam-app/src crates/iaam-core/src --include=*.rs | grep -v "fn validate_structure"
```

Write the test first:

```rust
    #[test]
    fn an_event_claiming_an_older_schema_version_is_refused_on_write() {
        // Build a valid gap, stamp schema_version = 7, empty rows, and append
        // it through the same gate the ingest scenario uses.
        // Assert the append is refused.
    }
```

- [ ] **Step 11: Verify and commit**

Run: `cargo check -p iaam-core -p iaam-app && cargo test -p iaam-core event::`
Expected: PASS.

```bash
git add crates/iaam-core/src/event/ crates/iaam-app/src
git commit -m "feat(core,app): a coverage gap carries the rows it refused (iaam-szl3)"
```

---

### Task 2: `ImportRowResolution` — disposal is a fact

**Files:**
- Modify: `crates/iaam-core/src/event/kind.rs` — new variant, `discriminant()`
  arm at 282, money-movement arm at 309
- Modify: `crates/iaam-core/src/event/mod.rs` — `validate_structure` arm and a
  new `validate_import_row_resolution`
- Modify: `crates/iaam-core/src/projection/{active_instruments.rs:108,
  income.rs:161, lots.rs:713}` — inert arms
- Modify: `crates/iaam-ingest/src/classification.rs:274`,
  `crates/iaam-app/src/scenarios/classification.rs:181`
- Test: `crates/iaam-core/src/event/mod.rs` tests module

**Interfaces:**
- Consumes: `SourceRowKey` from Task 1.
- Produces: `EventKind::ImportRowResolution { key, disposition }`,
  `RowDisposition::{Supplied, NoFact}`, `InertRow`.

**Acceptance Criteria:**
- A `Supplied` disposition with an empty event set is refused.
- A `NoFact` disposition with an empty reason is refused.
- A resolution carrying legs is refused — it is not an economic fact.
- The resolution changes no projection: balances, lots, income and active
  instruments are identical with and without it in the journal.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_supplied_resolution_without_events_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.legs = Vec::new();
        event.kind = EventKind::ImportRowResolution {
            key: row_key("OP-1"),
            disposition: RowDisposition::Supplied {
                events: std::collections::BTreeSet::new(),
            },
        };
        assert!(event.validate_structure().is_err());
    }

    #[test]
    fn a_resolution_with_a_leg_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.kind = EventKind::ImportRowResolution {
            key: row_key("OP-1"),
            disposition: RowDisposition::NoFact {
                classification: InertRow::OrderWithoutFills,
                reason: "cancelled order, no fills".to_owned(),
            },
        };
        // `sample_event` carries a cash leg; a resolution must have none.
        assert!(event.validate_structure().is_err());
    }

    #[test]
    fn a_no_fact_resolution_without_a_reason_is_rejected() {
        let mut event = test_support::sample_event(1);
        event.legs = Vec::new();
        event.kind = EventKind::ImportRowResolution {
            key: row_key("OP-1"),
            disposition: RowDisposition::NoFact {
                classification: InertRow::OwnerDetermined,
                reason: String::new(),
            },
        };
        assert!(event.validate_structure().is_err());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p iaam-core resolution`
Expected: FAIL — the variant does not exist, so this will not compile. That is
the expected failure.

- [ ] **Step 3: Add the variant**

In `crates/iaam-core/src/event/kind.rs`, beside `ImportCoverageGap`:

```rust
    /// How a refused source row was disposed of (§10.3).
    ///
    /// A coverage gap says which rows are missing; this says a row is missing
    /// no longer. It is deliberately a fact rather than an inference: a source
    /// row can expand into several events, or into none at all — a cancelled
    /// order with no fills — so neither the presence nor the absence of an
    /// event answers the question.
    ImportRowResolution {
        key: crate::event::source_row::SourceRowKey,
        disposition: RowDisposition,
    },
```

And, in the same file:

```rust
/// What became of a refused source row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowDisposition {
    /// The row is represented by these events. Never empty.
    ///
    /// The events are named rather than counted so that the disposition lapses
    /// on its own: `resolve()` drops reversed and replaced events, so a
    /// retracted fact takes its resolution's effect with it and the taint
    /// returns without anyone acting.
    Supplied {
        events: std::collections::BTreeSet<crate::ids::EventId>,
    },
    /// The row is understood and yields no journal fact.
    NoFact { classification: InertRow, reason: String },
}

/// Why a row correctly produces nothing.
///
/// A closed enumeration on purpose: "we understood it and it is inert" must be
/// an auditable determination. An adapter returning an empty vector must never
/// count as one — that is indistinguishable from an adapter that silently
/// produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InertRow {
    /// An order that was cancelled or never filled.
    OrderWithoutFills,
    /// The owner determined the row needs no fact.
    OwnerDetermined,
}
```

- [ ] **Step 4: Let the compiler find every dispatcher**

Run: `cargo check -p iaam-core`
Expected: non-exhaustive-match errors. Add an arm at each. Every one is the
same shape as the `ImportCoverageGap` arm beside it:

- `kind.rs:282` — `Self::ImportRowResolution { .. } => "import_row_resolution",`
- `kind.rs:309` — join the arm that yields no movement
- `projection/active_instruments.rs:108` — join the arm yielding `Vec::new()`
- `projection/income.rs:161` — join the arm yielding `Ok(())`
- `projection/lots.rs:713` — join the arm yielding `Ok(())`

Then: `cargo check -p iaam-ingest -p iaam-app` and do the same at
`classification.rs:274` and `scenarios/classification.rs:181`.

Do not guess the list — the compiler is authoritative and the count has grown
between epics.

- [ ] **Step 5: Write the validation**

`validate_structure` gains its arm, delegating to a new
`validate_import_row_resolution` that refuses: any leg, an empty `Supplied`
event set, and an empty `NoFact` reason. Follow the shape of
`validate_import_coverage_gap` at `crates/iaam-core/src/event/mod.rs:739`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p iaam-core resolution`
Expected: PASS.

- [ ] **Step 7: Prove projection-inertness**

```rust
    #[test]
    fn a_row_resolution_changes_no_projection() {
        // Build a journal, project it, then project it again with a resolution
        // appended, and assert the two projections are equal. Follow the
        // journal construction used by the neighbouring projection tests.
    }
```

Write it against the projection entry point the neighbouring tests use; assert
equality of the projected state with and without the resolution event.

- [ ] **Step 8: Verify and commit**

Run: `cargo check -p iaam-core -p iaam-ingest -p iaam-app && cargo test -p iaam-core event::`

```bash
git add crates/iaam-core/src crates/iaam-ingest/src crates/iaam-app/src
git commit -m "feat(core): a refused row is disposed of by an explicit fact (iaam-szl3)"
```

---

### Task 3: the gap's idempotency key becomes row-aware

**Files:**
- Modify: `crates/iaam-app/src/scenarios/sync.rs:350-394` — `coverage_gap_event`
- Test: `crates/iaam-app/src/scenarios/sync.rs` tests module

**Interfaces:**
- Consumes: `RefusedRow` from Task 1.
- Produces: `coverage_gap_event` takes `rows: &[RefusedRow]` instead of
  `dimensions` and `refused`, deriving both.

**Acceptance Criteria:**
- Two refusals with identical dimension sets and counts but different rows
  produce two different identities.
- The same refusal set in a different order produces the same identity.
- The identity does not use `Debug` or `Display` formatting for the rows.

Closes bead **iaam-lg4q**.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn two_refusals_of_different_rows_are_two_different_gaps() {
        let target = sample_target();
        let channel = sample_channel();
        let first = coverage_gap_event(target, &[refused_row("OP-1", Dimension::Cash)], &channel);
        let second = coverage_gap_event(target, &[refused_row("OP-2", Dimension::Cash)], &channel);
        assert_ne!(first.idempotency_key, second.idempotency_key);
    }

    #[test]
    fn the_order_of_refused_rows_does_not_change_the_gap_identity() {
        let target = sample_target();
        let channel = sample_channel();
        let forward = coverage_gap_event(
            target,
            &[refused_row("OP-1", Dimension::Cash), refused_row("OP-2", Dimension::Positions)],
            &channel,
        );
        let reverse = coverage_gap_event(
            target,
            &[refused_row("OP-2", Dimension::Positions), refused_row("OP-1", Dimension::Cash)],
            &channel,
        );
        assert_eq!(forward.idempotency_key, reverse.idempotency_key);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p iaam-app coverage_gap`
Expected: FAIL — today's identity carries only the dimension union and the
count (`crates/iaam-app/src/scenarios/sync.rs:365`), so the two rows collide.

- [ ] **Step 3: Rewrite the identity**

Replace the `identity` construction. The current one interpolates `{dimensions:?}`
— Debug formatting, which is not a stable encoding and must not be part of a
persisted key. Sort the rows, then encode each explicitly:

```rust
    // Canonical, not Debug: this string is a persisted idempotency key, and a
    // Debug rendering is free to change between compiler versions.
    let mut encoded: Vec<String> = rows
        .iter()
        .map(|row| {
            let name = match &row.key.row {
                RowName::Given(id) => format!("given:{}", escape_component(id)),
                RowName::Fingerprint(hex) => format!("fp:{hex}"),
            };
            let dimensions: Vec<&str> =
                row.dimensions.iter().map(Dimension::code).collect();
            format!("{}/{}/{}", row.key.source.inner(), name, dimensions.join("+"))
        })
        .collect();
    encoded.sort();
    let identity = format!(
        "sync-coverage-gap/{account:?}/{from}/{to}/{:?}/{:?}/{}",
        channel.source,
        channel.parser_version,
        encoded.join(",")
    );
```

`Dimension::code` and `escape_component` may not exist under those names — use
whatever stable string accessor the tree already has for each (`escape_component`
is used at `crates/iaam-app/src/adapters/tinkoff.rs:702`). If `Dimension` has no
stable string form, add one; do not reach for `Debug`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p iaam-app coverage_gap`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/iaam-app/src/scenarios/sync.rs
git commit -m "fix(app): the coverage gap's identity names the rows it refused (iaam-lg4q)"
```

---

### Task 4: the importer fills the rows and disposes of them

**Files:**
- Modify: `crates/iaam-app/src/ports.rs:544` — `Quarantined` gains the row key
- Modify: `crates/iaam-app/src/adapters/tinkoff.rs:212-272` — every
  `quarantined.push` site supplies the key
- Modify: `crates/iaam-app/src/scenarios/sync.rs` — `sync_broker`: read
  outstanding rows, fill `rows`, append resolutions
- Test: `crates/iaam-app/src/scenarios/sync.rs` tests, and the integration tests
  that already exercise `sync_broker`

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: `sync_broker` writes `ImportCoverageGap` with populated `rows` and
  appends `ImportRowResolution` events.

**Acceptance Criteria:**
- A refused row whose source gave an identifier is keyed `Given`; one that did
  not is keyed `Fingerprint` over the raw payload.
- Refusals from **both** layers are represented: the adapter's quarantine list
  and the normalisation rejections added by iaam-bl07.
- A row refused in one import and accepted in the next produces a `Supplied`
  resolution naming the events just recorded.
- The resolution is appended **after** the facts it references.
- A trade order accepted with no fills produces `NoFact { OrderWithoutFills }`.

- [ ] **Step 1: Write the failing test — the key**

```rust
    #[test]
    fn a_refused_row_without_a_source_identifier_is_keyed_by_its_payload() {
        // Two refusals of the same raw payload produce the same Fingerprint key;
        // a different payload produces a different one.
    }
```

- [ ] **Step 2: Run it, verify it fails, then carry the key**

`Quarantined` becomes:

```rust
pub struct Quarantined {
    pub raw: Value,
    pub reason: String,
    pub dimensions: std::collections::BTreeSet<Dimension>,
    /// Identity of the row this refusal is about.
    ///
    /// Carried rather than recomputed downstream: reparsing `raw` in the
    /// scenario would put channel knowledge where it does not belong, which is
    /// the same reason `dimensions` is carried (iaam-ep05).
    pub key: SourceRowKey,
}
```

Each of the six `quarantined.push` sites in
`crates/iaam-app/src/adapters/tinkoff.rs` supplies it. `operation.operation_id`
is available at every one of them (`crates/iaam-broker/src/tinkoff/parse.rs:113`)
— but it is `String`, and `crates/iaam-broker/src/tinkoff/parse.rs:359` sets it
to the empty string for a row the gateway did not identify. **An empty
identifier is not an identifier:** map it to `RowName::Fingerprint` over the
SHA-256 of `raw`, not to `RowName::Given("")`.

Take the key before `std::mem::take(&mut operation.raw)` at line 204 consumes
the payload, or compute the fingerprint from the taken value — but do not read
`operation.operation_id` after the operation has been moved into conversion.

- [ ] **Step 3: Write the failing test — resolutions are appended after facts**

```rust
    #[test]
    fn a_resolution_follows_the_facts_it_names() {
        // Sync an interval whose journal already holds a gap naming OP-1.
        // The response now carries OP-1 successfully.
        // Assert: the recorded events contain the fact(s) for OP-1 AND a
        // Supplied resolution naming exactly their ids, and that the
        // resolution's position in the append order is after them.
    }
```

- [ ] **Step 4: Implement**

Before importing, `sync_broker` reads the outstanding rows for
`(account, from..to)` — gaps in the effective set whose rows have no effective
resolution. After appending accepted operations, for each outstanding row the
response has now supplied, append `ImportRowResolution::Supplied` naming the
event identifiers just recorded.

Facts first, resolution second, always. Writes are not batch-atomic —
`append_events` inserts one event at a time
(`crates/iaam-app/src/adapters/sqlite.rs:197`) — so a crash between them leaves
the facts recorded and the row outstanding. That is a false taint, which is the
safe direction; the reverse order would leave a row marked supplied by events
that are not there.

- [ ] **Step 5: Run the targeted tests**

Run: `cargo test -p iaam-app sync`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-app/src
git commit -m "feat(app): the importer names its refused rows and disposes of them (iaam-szl3)"
```

---

### Task 5: reconciliation applies outstanding rows

**Files:**
- Modify: `crates/iaam-core/src/reconciliation/mod.rs` — `build_with` (220-222),
  `collect_coverage_gaps` (390), `tainted_dimensions` (411), `build_status`,
  `merge_status`, `with_external_evidence` (278)
- Test: `crates/iaam-core/tests/reconciliation_ledger.rs`

**Interfaces:**
- Consumes: Tasks 1-2.
- Produces: outstanding-row taint as a ledger constraint; `ReconciliationStatus`
  carries the outstanding count per dimension for Task 7.

**Acceptance Criteria:**
- Both `collect_groups` and `collect_coverage_gaps` read the effective set.
- A row with an effective `Supplied` resolution whose events are all effective
  no longer taints; reversing any one of those events restores the taint.
- A `NoFact` resolution clears a row that produced no events.
- A gap taints every overlapping period of the same account, regardless of
  source or parser version.
- A gap for a period with no assertion group still constrains that period.
- `with_external_evidence` cannot raise a dimension past an outstanding row.

Closes bead **iaam-ueo1**.

- [ ] **Step 1: Change the two tests that encode the defect**

`crates/iaam-core/tests/reconciliation_ledger.rs:270` —
`a_gap_from_another_source_or_parser_leaves_the_group_intact` — is renamed and
inverted: a gap from another source or parser now **does** withhold, because a
missing fact is missing from the journal and not from a channel.

`:305` — `a_later_group_without_a_gap_can_restore_independent_confirmation` — is
iaam-dvki in another form and is replaced by Task 8's test: a later group
restores confirmation only when the row was **disposed of**.

Both changes are deliberate. Say so in the commit message; a reviewer seeing two
deleted assertions needs to know they were asserting the bug.

- [ ] **Step 2: Run to see them fail as expected**

Run: `cargo test -p iaam-core --test reconciliation_ledger`
Expected: the two rewritten tests FAIL; `:225`, `:248` and `:344` still PASS —
those three encode requirements and must not change.

- [ ] **Step 3: Implement**

`build_with` passes `&effective_events` to `collect_groups` and
`collect_coverage_gaps`. Gaps collect their `rows`. A new
`collect_row_resolutions` builds the set of disposed keys: an effective
`ImportRowResolution` whose `Supplied` events are all present in the effective
set, or whose disposition is `NoFact`.

`tainted_dimensions` drops the `source` and `parser_version` comparison, keeps
`account`, and replaces period **equality** with period **overlap**. For a
schema-8 gap it unions the dimensions of rows that are not disposed of; for a
legacy gap (empty `rows`) it unions `dimensions`, unchanged.

A gap whose period matches no assertion group must still produce a status. Add
the constraint where statuses are assembled (line 263 onward) rather than only
where groups are iterated, and carry the outstanding counts onto
`ReconciliationStatus` for Task 7.

`with_external_evidence` (278) consults the retained taints before raising.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p iaam-core --test reconciliation_ledger`

```bash
git add crates/iaam-core/src/reconciliation crates/iaam-core/tests
git commit -m "fix(core): an outstanding row withholds the journal, not the channel (iaam-dvki, iaam-ueo1)"
```

---

### Task 6: the owner can dispose of a row

**Files:**
- Modify: `crates/iaam-server/src/routes.rs` — a route listing outstanding rows,
  and a repair that names the row it disposes of
- Modify: `crates/iaam-server/src/dto.rs` — the request and response types
- Modify: `crates/iaam-app/src/scenarios/` — the scenario behind them
- Test: the server's route tests

**Interfaces:**
- Consumes: Tasks 1-5.

**Acceptance Criteria:**
- Outstanding rows for an account and interval are readable.
- A repair appends the manual fact **and** a `Supplied` resolution naming it.
- The manual fact keeps truthful manual provenance. It must **not** claim the
  broker's `SourceId`.
- An owner may record `NoFact { OwnerDetermined }` with a reason.

**Read before writing anything:** `crates/iaam-server/src/routes.rs:1262-1290`
(the journal-events route, and where its `SourceId::new_random()` comes from),
`crates/iaam-server/src/dto.rs:3670` (the journal DTO — it admits only corporate
actions and offers today, and exposes no `Relation`), and one existing route
test in the same file for the assertion style. This task's shape follows those;
do not invent a route convention.

- [ ] **Step 1: Write the failing test — provenance is not forged**

```rust
    #[test]
    fn a_manual_repair_does_not_claim_the_broker_as_its_source() {
        // Repair an outstanding broker row through the public route.
        // Assert the appended fact's provenance.source() is the manual source,
        // not the broker row key's source.
    }
```

This is the important one. `SourceChannel::is_independent_of` compares parser
version and document (`crates/iaam-core/src/reconciliation/evidence.rs:59`); a
manual fact wearing the broker's identity would corrupt both independence and
deduplication.

- [ ] **Step 2-4: Implement, run, commit**

The route appends the fact under manual provenance, then the resolution. Same
ordering rule as Task 4: fact first.

```bash
git add crates/iaam-server/src crates/iaam-app/src
git commit -m "feat(server): the owner disposes of a refused row explicitly (iaam-szl3)"
```

---

### Task 7: the report says what it actually is

**Files:**
- Modify: `crates/iaam-core/src/returns/mod.rs:230` — `MaterialIssue`; `:2234` —
  the provisional arm; `:2215` — the dimension loop
- Modify: `crates/iaam-server/src/dto.rs:1745` — the exhaustive mapping
- Test: `crates/iaam-core/src/returns/mod.rs` tests

**Acceptance Criteria:**
- A provisional dimension caused by an outstanding row reports
  `ImportIncomplete { account, dimension, outstanding }`.
- A provisional dimension with no outstanding row still reports
  `NoIndependentSource`.
- Whether `ImportIncomplete` counts as a defect in `MaterialIssue::is_defect` is
  decided explicitly and its effect on the data-quality status is asserted by a
  test.
- Either the loop at `:2215` widens beyond `Cash` and `Positions`, or the plan
  records why `Income` and `TaxBasis` gaps cannot reach the report.

**Read before writing anything:** `crates/iaam-core/src/returns/mod.rs:2210-2245`
(the loop and the arm you are changing — note it skips zero-valued measurements
and iterates only `Cash` and `Positions`), `MaterialIssue` at `:230` for the
variant style, and `crates/iaam-server/src/dto.rs:1745` for the mapping's shape.
The neighbouring tests in `returns/mod.rs` show how a report is built in a test;
follow one of them rather than constructing a `ReturnsRequest` from scratch.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn an_outstanding_row_is_reported_as_an_incomplete_import() {
        // A journal whose cash is provisional because a row is outstanding.
        // Assert material_issues contains ImportIncomplete for Cash and does
        // NOT contain NoIndependentSource for Cash.
    }

    #[test]
    fn a_provisional_dimension_with_no_gap_still_reports_no_independent_source() {
        // The regression guard: the new arm must not swallow the old case.
    }
```

- [ ] **Step 2-4: Implement, run, commit**

```bash
git add crates/iaam-core/src/returns crates/iaam-server/src/dto.rs
git commit -m "feat(core,server): an incomplete import is reported as one (iaam-szl3)"
```

---

### Task 8: the test that proves the defect is gone

**Files:**
- Modify: `crates/iaam-core/tests/reconciliation_ledger.rs`

**Acceptance Criteria:**
- Source and parser version are held **identical** between the tainted state and
  the recovered state. Only the disposal differs.
- The test fails if `tainted_dimensions` is reverted to correlate on the
  channel.

`row_key`, `refused_row`, `coverage_gap_with_rows` and `row_resolution` are
helpers **local to this test file**, written alongside the existing
`coverage_gap` helper at `crates/iaam-core/tests/reconciliation_ledger.rs:165`.
Task 3 has a helper of the same name in a different crate with a different
signature; they are unrelated, and neither should be moved to share.

- [ ] **Step 1: Write it**

```rust
#[test]
fn a_row_disposed_of_recovers_the_same_channel() {
    let (owner, account, _instrument, _custody, channel, mut events) = seeded_journal();
    let key = row_key(&channel, "OP-1");
    events.push(coverage_gap_with_rows(
        &channel_with_document(&channel, "gap"),
        AssertionScope { owner, account, period: march() },
        vec![refused_row(key.clone(), [Dimension::Cash])],
    ));

    let before = ReconciliationLedger::build(&events).unwrap();
    assert_ne!(
        before.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );

    // The SAME channel — same source, same parser version. The existing
    // `TestChannel::new("later-parser/1", "later")` changes both, because
    // `TestChannel::new` mints a random source (tests/support/mod.rs:31), so
    // it proves recovery by a DIFFERENT channel and not by this one.
    let supplied = deposit(&channel, owner, account, 1);
    let supplied_id = supplied.id;
    events.push(supplied);
    events.push(row_resolution(
        &channel,
        AssertionScope { owner, account, period: march() },
        key,
        RowDisposition::Supplied { events: [supplied_id].into_iter().collect() },
    ));

    let after = ReconciliationLedger::build(&events).unwrap();
    assert_eq!(
        after.status_for(account, date!(2026 - 03 - 15), Dimension::Cash),
        DimensionStatus::AcceptedIndependent
    );
}

#[test]
fn reversing_a_supplied_fact_restores_the_taint() {
    // Same journal as above, plus a reversal of `supplied_id`.
    // Assert Cash is no longer AcceptedIndependent: the resolution lapsed on
    // its own, with nobody touching the gap or the resolution.
}
```

- [ ] **Step 2-3: Run and commit**

Run: `cargo test -p iaam-core --test reconciliation_ledger`

```bash
git add crates/iaam-core/tests
git commit -m "test(core): recovery through the same channel, and its lapse (iaam-dvki)"
```

---

## Orchestrator gate — after Task 8, not before

Only once every task above is accepted:

```bash
make check            # the repo's own guard scripts, not just cargo
cargo test --workspace
scripts/check-mutants.sh   # results land per-module under target/mutants/<module>
```

Then close `iaam-dvki`, `iaam-lg4q`, `iaam-ueo1` and the epic `iaam-evc2`.
