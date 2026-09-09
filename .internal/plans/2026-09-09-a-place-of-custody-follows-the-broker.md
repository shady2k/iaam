# A Place of Custody Follows the Broker — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Restore what the relational journal broke — a security fact can be recorded again — without creating any custody identity that did not exist before, and make an archive restorable into an empty database.

**Architecture:** `custody_places` gains one column, `origin`, splitting the table into places the owner would recognise (`declared`) and bare handles a channel produced (`minted`). Broker synchronisation registers the handles it mints — from both of its append paths — so the foreign keys hold and no value in the journal moves. The owner check moves from the legs to one exhaustive domain traversal, because the events this makes writable again carry custody in detail rows that have no legs. The bundle grows two sections, guarded by an exact schema-coverage test.

**Tech Stack:** Rust workspace, `rusqlite` on SQLite (`STRICT` tables, `PRAGMA foreign_keys` on), `axum` + `utoipa`/`utoipa_axum` for routes and OpenAPI, `ciborium` for the bundle checksum, `cargo nextest`.

**Spec:** `.internal/specs/2026-09-09-a-place-of-custody-follows-the-broker-design.md`
**Beads:** epic `iaam-wwgv`; `iaam-klpv` (P0), `iaam-no3w` (P1), `iaam-pzxl` (P1), `iaam-kks6` (P2).

## Waves

This plan was revised after an adversarial review. The review is not an appendix
— it changed what ships and in what order. See the spec's §8 for the argument.

| Wave | Tasks | Why together |
|---|---|---|
| **1 — P0** | T1, T2, T6, T8, T9 | Restores the broker API path and closes the owner boundary the events it re-enables would otherwise cross. Creates no custody identity that did not exist before. |
| **2 — P2** | T7 | The archive. Touches only `bundle.rs`, the schema section and the store tests; independent of wave 1. |
| **3 — epic** | T3, T4, T5 | Deferred behind `iaam-xep0`. Each mints a custody identity the channels do not share, and that causes silent double counting — not a display split. |

**The cost wave 1 leaves open, stated because it is the owner's to accept:**
securities cannot enter from a broker **report** until wave 3. Only the wired
API channel can record them and the only wired channel is T-Invest
(`adapters/sqlite.rs:1691`). T9 makes that an honest refusal instead of a silent
one; it does not make it work.

## Global Constraints

- **English only** in everything written: identifiers, test names, doc comments, `#[error(...)]` texts, API messages, new documents. Existing Russian text in `0001_schema.sql`, `docs/` and `.internal/` is left alone and not retranslated. Domain terms come from `docs/glossary-ru-en.md`.
- **No owner data anywhere.** Fixtures are invented from scratch (`Main`, `Broker One`, `Shop One`) and never trimmed from a real export. `make privacy` must pass; `make hooks` installs it as `pre-commit`.
- **Schema changes are edited into `crates/iaam-store/migrations/0001_schema.sql` in place.** No new migration file. `SCHEMA_VERSION` stays `1` (`crates/iaam-store/src/schema.rs:18`) — the collapse to one file was done on a greenfield database and that still holds.
- **Production code is never weakened to satisfy a fixture.** If a test wants a check relaxed, stop and report: the check is right and the fixture is wrong.
- **No new dependencies.**
- Quality gate for every task: `make check` (`fmt lint arch privacy skill-doc fixtures deps test doc-test`) exits 0. `lint` is clippy with `-D warnings`.
- Base branch: `wave-ai/relational-journal`. Commit per task, referencing the bead.

## File Structure

| File | Responsibility in this change |
|---|---|
| `crates/iaam-core/src/custody.rs` | **new** — `CustodyOrigin` vocabulary (`code`/`from_code`) |
| `crates/iaam-core/src/lib.rs` | register the module |
| `crates/iaam-store/migrations/0001_schema.sql` | `custody_places.origin` column + index |
| `crates/iaam-store/src/reference.rs` | `CustodyRecord.origin`; `upsert_custody_place`; `list_custody_places`; the `(owner, title)` lookup |
| `crates/iaam-app/src/ports.rs` | `CustodyUpsert`, `CustodyView.origin`, `InstrumentDirectory::record_custody_place` |
| `crates/iaam-app/src/adapters/sqlite.rs` | the sole implementation of the new port method |
| `crates/iaam-core/src/event/mod.rs` | `Event::referenced_custodies()` — the one traversal |
| `crates/iaam-store/src/journal/write.rs` | the ownership check over that traversal |
| `crates/iaam-ingest/src/csv_source.rs` | the reworded custody refusal |
| `crates/iaam-ingest/src/report/tinkoff.rs`, `report/finam.rs` | position rows stop being dropped |
| `crates/iaam-app/src/scenarios/sync.rs` | register a minted place before **each** of the two appends |
| `crates/iaam-store/src/bundle.rs` | two new sections, `BUNDLE_VERSION` 2, import order, closure query |
| `crates/iaam-store/tests/bundle.rs` | restore into a genuinely empty store |
| `crates/iaam-store/tests/schema_coverage.rs` | **new** — exact `(table, column)` guard for both the traversal and the export |

---

### Task 1: `origin` — the column that keeps a handle from posing as a place

**Files:**
- Create: `crates/iaam-core/src/custody.rs`
- Modify: `crates/iaam-core/src/lib.rs` (add `pub mod custody;` beside the other module declarations)
- Modify: `crates/iaam-store/migrations/0001_schema.sql:153-163`
- Modify: `crates/iaam-store/src/reference.rs:288-294` (`CustodyRecord`), `:768-791` (`upsert_custody_place`), `:793-816` (`list_custody_places`)
- Test: `crates/iaam-store/tests/instrument_directory.rs`

**Interfaces:**
- Produces: `iaam_core::custody::CustodyOrigin` with `Declared` and `Minted` variants, `const fn code(self) -> &'static str` and `fn from_code(code: &str) -> Option<Self>`; `iaam_store::reference::CustodyRecord { id, owner, title, institution, origin }`.

**Not produced:** an earlier draft added `custody_place_by_title`. It is dropped. With T3-T5 deferred it would have no production consumer, and its `ORDER BY id LIMIT 1` would hide exactly the ambiguity the unique index below now prevents.

**Acceptance Criteria:**
- A custody place round-trips its origin through `upsert_custody_place` / `list_custody_places`.
- A row whose `origin` is neither `declared` nor `minted` cannot be written: the database refuses it.
- Two declared places of one owner cannot share a title; the database refuses the second. A minted row is not constrained by that index.
- A title is stored trimmed, so `"Broker One"` and `" Broker One "` are one key and not two. Case is left exact.
- The existing refusal of a leg naming another owner's place (`journal/write.rs:787`) still passes unchanged.

- [ ] **Step 1: Write the failing test**

In `crates/iaam-store/tests/instrument_directory.rs`:

```rust
#[test]
fn a_custody_place_keeps_the_origin_it_was_written_with() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let declared = CustodyId::new_random();
    let minted = CustodyId::new_random();

    store
        .upsert_custody_place(&CustodyRecord {
            id: declared,
            owner,
            title: "Broker One".to_owned(),
            institution: Some("Broker One".to_owned()),
            origin: CustodyOrigin::Declared,
        })
        .expect("declared place");
    store
        .upsert_custody_place(&CustodyRecord {
            id: minted,
            owner,
            title: "broker-channel handle".to_owned(),
            institution: None,
            origin: CustodyOrigin::Minted,
        })
        .expect("minted place");

    let places = store.list_custody_places(owner).expect("list");
    let origins: Vec<(CustodyId, CustodyOrigin)> =
        places.iter().map(|place| (place.id, place.origin)).collect();
    assert!(origins.contains(&(declared, CustodyOrigin::Declared)));
    assert!(origins.contains(&(minted, CustodyOrigin::Minted)));
}

#[test]
fn two_declared_places_of_one_owner_cannot_share_a_title() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();

    let declared = |id| CustodyRecord {
        id,
        owner,
        title: "Broker One".to_owned(),
        institution: Some("Broker One".to_owned()),
        origin: CustodyOrigin::Declared,
    };
    store
        .upsert_custody_place(&declared(CustodyId::new_random()))
        .expect("first declared place");
    assert!(
        store
            .upsert_custody_place(&declared(CustodyId::new_random()))
            .is_err(),
        "a second declared place under one title is a state no lookup can interpret"
    );

    // A minted handle carries a generated title and many of them may repeat it:
    // nothing resolves them by title, so nothing is made ambiguous.
    let minted = |id| CustodyRecord {
        id,
        owner,
        title: "broker-channel handle".to_owned(),
        institution: None,
        origin: CustodyOrigin::Minted,
    };
    store.upsert_custody_place(&minted(CustodyId::new_random())).expect("first handle");
    store.upsert_custody_place(&minted(CustodyId::new_random())).expect("second handle");
}

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p iaam-store --test instrument_directory 2>&1 | tail -20`
Expected: compilation failure — `CustodyRecord` has no field `origin`, and `custody_place_by_title` does not exist.

- [ ] **Step 3: Add the vocabulary**

Create `crates/iaam-core/src/custody.rs`:

```rust
//! Where a security is kept, and where the identifier for it came from.

/// Where a place of custody came from.
///
/// The two are not variants of one thing that happen to differ; they answer
/// different questions and only one of them may be offered to a document
/// reader by its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CustodyOrigin {
    /// A place of the owner's: named by him, or derived from the institution
    /// that keeps one of his accounts. It has a title he would recognise, and
    /// a document naming that title reaches it.
    Declared,
    /// A bare handle a channel produced — the broker's `positionUid`, or its
    /// own account identifier. It is not a place. The row exists so the
    /// journal's foreign key holds, and for nothing else: it is never offered
    /// by title, and never shown as one of his places.
    ///
    /// The marker is also what makes `iaam-xep0` addressable later: it names
    /// exactly the rows a repair would have to retract.
    Minted,
}

impl CustodyOrigin {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Minted => "minted",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "declared" => Some(Self::Declared),
            "minted" => Some(Self::Minted),
            _ => None,
        }
    }
}
```

Add `pub mod custody;` to `crates/iaam-core/src/lib.rs` in alphabetical position among the existing `pub mod` lines.

- [ ] **Step 4: Add the column**

In `crates/iaam-store/migrations/0001_schema.sql`, replace the `custody_places` block at lines 153-163 with:

```sql
CREATE TABLE custody_places (
    id          TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    title       TEXT NOT NULL,
    institution TEXT,
    -- Where the identifier came from: 'declared' is a place of the owner's and
    -- may be reached by its title; 'minted' is a bare handle a broker channel
    -- produced and is never offered as a place. The CHECK is here rather than
    -- only in Rust because the column decides what a document reader is shown,
    -- and a third value written by any path would silently widen that.
    origin      TEXT NOT NULL CHECK (origin IN ('declared', 'minted')),
    created_at  TEXT NOT NULL
) STRICT;

-- Владелец в уникальном ключе — как у accounts: иначе чужое место
-- хранения подставится в ногу сделки (§14).
CREATE UNIQUE INDEX custody_places_by_owner ON custody_places (owner, id);

-- A declared place is found by the owner's title for it, and a title names at
-- most one. Without this the schema would admit duplicates while the document
-- reader's own lookup refuses an ambiguous title (`lookup_custody`) - a state
-- no reader can interpret. Minted rows are outside the constraint: they carry
-- generated titles, repeat them freely, and nothing resolves them by title.
CREATE UNIQUE INDEX custody_places_declared_title
    ON custody_places (owner, title) WHERE origin = 'declared';
```

- [ ] **Step 5: Carry it through the store**

In `crates/iaam-store/src/reference.rs`, extend the record (add `use iaam_core::custody::CustodyOrigin;` to the imports):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyRecord {
    pub id: CustodyId,
    pub owner: OwnerId,
    pub title: String,
    pub institution: Option<String>,
    pub origin: CustodyOrigin,
}
```

Replace `upsert_custody_place`:

```rust
/// Creating or updating a custody place.
///
/// The `WHERE custody_places.owner = excluded.owner` condition
/// required for the same reason as accounts: without it, a request
/// with someone else's identifier would overwrite another
/// owner's storage location (§14).
///
/// `origin` is **not** updated on conflict. A handle a channel minted does not
/// become a place of the owner's because a later call said so, and a place of
/// his does not stop being one because a channel met its identifier.
pub fn upsert_custody_place(&self, place: &CustodyRecord) -> Result<(), StoreError> {
    self.conn.execute(
        "INSERT INTO custody_places (id, owner, title, institution, origin, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (id) DO UPDATE SET
             title = excluded.title,
             institution = excluded.institution
         WHERE custody_places.owner = excluded.owner",
        params![
            place.id.inner().to_string(),
            place.owner.inner().to_string(),
            place.title,
            place.institution,
            place.origin.code(),
            now(),
        ],
    )?;
    Ok(())
}
```

Replace `list_custody_places` so it selects and decodes `origin`, and add the lookup beside it:

```rust
pub fn list_custody_places(&self, owner: OwnerId) -> Result<Vec<CustodyRecord>, StoreError> {
    let mut statement = self.conn.prepare(
        "SELECT id, title, institution, origin FROM custody_places
         WHERE owner = ?1 ORDER BY title, id",
    )?;
    let rows = statement.query_map([owner.inner().to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut places = Vec::new();
    for row in rows {
        let (id, title, institution, origin) = row?;
        places.push(CustodyRecord {
            id: CustodyId(parse_uuid(&id, "custody")?),
            owner,
            title,
            institution,
            origin: CustodyOrigin::from_code(&origin).ok_or_else(|| StoreError::NotFound {
                what: "custody origin",
                id: origin.clone(),
            })?,
        });
    }
    Ok(places)
}

```

Titles are trimmed on the way in — in `upsert_custody_place`, so every writer
gets it and not only the one a route would go through.

- [ ] **Step 6: Fix every construction site**

`CustodyRecord` is built in the fixtures listed by `grep -rn "CustodyRecord {" --include=*.rs crates/`. Every one gains `origin: CustodyOrigin::Declared` — they are all standing in for a place of the owner's. Do not change any production check to make one compile.

- [ ] **Step 7: Run the tests**

Run: `cargo nextest run -p iaam-store 2>&1 | tail -20`
Expected: PASS, including `a_leg_naming_a_custody_place_of_another_owner_is_refused`.

- [ ] **Step 8: Full gate and commit**

```bash
make check
git add -A
git commit -m "Give a place of custody an origin, so a handle cannot pose as one (iaam-klpv)"
```

---

---

### Task 2: The port that can create one

**Files:**
- Modify: `crates/iaam-app/src/ports.rs:527-532` (`CustodyView`), `:559-594` (the trait)
- Modify: `crates/iaam-app/src/adapters/sqlite.rs:350-356` (`custody_view`), `:1406-1515` (the impl)
- Test: `crates/iaam-app/tests/custody_places.rs` (new)

**Interfaces:**
- Consumes: `CustodyOrigin`, `CustodyRecord`, `custody_place_by_title` from Task 1.
- Produces:
  ```rust
  pub struct CustodyUpsert {
      pub id: CustodyId,
      pub title: String,
      pub institution: Option<String>,
      pub origin: CustodyOrigin,
  }
  // on InstrumentDirectory:
  async fn record_custody_place(&self, owner: OwnerId, place: CustodyUpsert)
      -> Result<CustodyId, AppError>;
  ```
  and `CustodyView { id, title, institution, origin }`.

**Acceptance Criteria:**
- A custody place can be created through the port and read back through `list_custody_places` with its origin.
- `SqliteAdapter` is the only implementation touched — no test double exists and none is introduced.
- The helpers in `crates/iaam-app/tests/sync.rs:127`, `schedule_metamorphic.rs:132`, `custody_repair.rs:115`, `report_market_input.rs:43`, `money_flow.rs:336` and `owner_balance.rs:120` use the port instead of seeding the raw `SqliteStore`; the comment at `sync.rs:91-101` explaining why they could not is deleted, because it is no longer true.

- [ ] **Step 1: Write the failing test**

Create `crates/iaam-app/tests/custody_places.rs`:

```rust
//! A place of custody can be created through the ports it is read through.

use std::sync::Arc;

use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{CustodyUpsert, InstrumentDirectory};
use iaam_core::custody::CustodyOrigin;
use iaam_core::ids::{CustodyId, OwnerId};
use iaam_store::SqliteStore;

#[tokio::test]
async fn a_place_created_through_the_port_is_listed_by_it() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let directory = Arc::new(SqliteAdapter::new(store));
    let owner = OwnerId::new_random();
    let id = CustodyId::new_random();

    directory
        .record_custody_place(
            owner,
            CustodyUpsert {
                id,
                title: "Broker One".to_owned(),
                institution: Some("Broker One".to_owned()),
                origin: CustodyOrigin::Declared,
            },
        )
        .await
        .expect("record");

    let places = directory.list_custody_places(owner).await.expect("list");
    assert_eq!(places.len(), 1);
    assert_eq!(places[0].id, id);
    assert_eq!(places[0].origin, CustodyOrigin::Declared);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p iaam-app --test custody_places 2>&1 | tail -20`
Expected: compilation failure — `CustodyUpsert` and `record_custody_place` do not exist.

- [ ] **Step 3: Extend the port**

In `crates/iaam-app/src/ports.rs`, add `origin` to the view and the upsert struct beside `InstrumentUpsert`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyView {
    pub id: CustodyId,
    pub title: String,
    pub institution: Option<String>,
    pub origin: iaam_core::custody::CustodyOrigin,
}

/// A place of custody from an authorised writer.
///
/// The caller assigns the identifier, as it does for an instrument: a broker
/// channel already holds the handle it must register, and inventing a second
/// identifier for it would defeat the registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyUpsert {
    pub id: CustodyId,
    pub title: String,
    pub institution: Option<String>,
    pub origin: iaam_core::custody::CustodyOrigin,
}
```

and add to the `InstrumentDirectory` trait, after `list_custody_places`:

```rust
    /// Create or rename a place of custody.
    ///
    /// Upsert by identifier and owner-scoped exactly as an account write is: a
    /// request carrying someone else's identifier changes nothing (§14).
    ///
    /// This is the fallback path. The ordinary one is an account's institution
    /// deriving a place, and a broker channel registering the handle it mints.
    async fn record_custody_place(
        &self,
        owner: OwnerId,
        place: CustodyUpsert,
    ) -> Result<CustodyId, AppError>;

```

- [ ] **Step 4: Implement it once**

In `crates/iaam-app/src/adapters/sqlite.rs`, extend `custody_view` with `origin: record.origin`, and add to `impl InstrumentDirectory for SqliteAdapter`, imitating `record_instrument` exactly:

```rust
    async fn record_custody_place(
        &self,
        owner: OwnerId,
        place: CustodyUpsert,
    ) -> Result<CustodyId, AppError> {
        let CustodyUpsert {
            id,
            title,
            institution,
            origin,
        } = place;
        self.blocking(move |store| {
            store
                .upsert_custody_place(&iaam_store::reference::CustodyRecord {
                    id,
                    owner,
                    title,
                    institution,
                    origin,
                })
                .map_err(store_error)?;
            Ok(id)
        })
        .await
    }
```

- [ ] **Step 5: Run the test**

Run: `cargo nextest run -p iaam-app --test custody_places 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Retire the workarounds**

Rewrite each helper listed in the acceptance criteria to call the port. In `crates/iaam-app/tests/sync.rs`, `seed_custody` becomes an `async fn` taking the directory, and `services_with`'s doc comment loses the paragraph explaining why a port could not be used — replace it with one sentence saying instruments are still seeded on the raw store and custody places no longer are. Do not delete a test to avoid the rewrite.

- [ ] **Step 7: Full gate and commit**

```bash
make check
git add -A
git commit -m "A place of custody can be created through the port it is read through (iaam-klpv)"
```

---

---

### Task 6: Broker synchronisation registers the handles it mints

**Files:**
- Modify: `crates/iaam-app/src/scenarios/sync.rs:139-207` (the operations loop) and `:288-325` (the portfolio-claims loop)
- Test: `crates/iaam-app/tests/sync.rs`

**Interfaces:**
- Consumes: `InstrumentDirectory::record_custody_place`, `CustodyOrigin::Minted` (T2).

**Acceptance Criteria:**
- A store holding no custody place at all synchronises a broker trade **and** a portfolio position claim.
- A position the broker reports that saw no trade in the interval is recorded: it has no operation, so the operations loop never sees its handle.
- The registered rows' origin is `minted`.
- The value in the event is unchanged: a test asserts the custody id on the appended leg, and on the appended assertion, equals the one the channel produced.
- The match over `OperationKind` is exhaustive with no `..` arm that could swallow a new variant's handle.
- `POST /v1/ingest/journal-events` still refuses an event naming a custody place the owner does not hold, and the registration is **not** moved into `ingest::append_checked`.

**A correction that is the whole of this task.** An earlier draft registered
handles from the accepted operations only. There are two append paths in a sync,
not one:

- the operations loop (`sync.rs:139-207`) appends events built from `SubmittedOperation`s, whose custody reaches `Leg::security` unchanged (`ingest/operation.rs:683`);
- the portfolio loop (`sync.rs:288-325`) runs **afterwards**, independently, and appends a legless `ControlAssertion` per claim, carrying `custody = positionUid` from `tinkoff/parse.rs:255`.

A portfolio position with no trade in the requested interval has no operation at
all. Registering only from `parsed.accepted` therefore leaves half the sync
refused, and the half it leaves refused is the half reconciliation depends on.

- [ ] **Step 1: Write the failing tests**

In `crates/iaam-app/tests/sync.rs`, two tests built on `services_with(today, |_store| {})` — seeding **no** custody place:

```rust
#[tokio::test]
async fn a_sync_registers_the_handle_a_trade_names() {
    // No custody place is seeded: the sync must register what the channel mints.
    let services = services_with(TODAY, |_store| {});
    let outcome = sync_one_buy(&services).await.expect("sync");
    assert!(matches!(outcome.recorded[0], Verdict::Recorded { .. }));
}

#[tokio::test]
async fn a_position_with_no_trade_in_the_interval_is_still_asserted() {
    // The portfolio loop is a second append path. A holding the broker reports
    // but did not trade in this interval reaches it with no operation behind
    // it, so a registration driven from the accepted operations never sees its
    // handle.
    let services = services_with(TODAY, |_store| {});
    let outcome = sync_portfolio_only(&services).await.expect("sync");
    assert_eq!(outcome.assertions, 1);
}
```

`sync_one_buy` and `sync_portfolio_only` drive the existing fake channel; read
the neighbouring sync tests and build them from the fixtures already there
rather than inventing a channel.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p iaam-app --test sync 2>&1 | tail -20`
Expected: FAIL — both appends are refused, the custody place is not registered.

- [ ] **Step 3: Register in the operations loop**

Immediately before the `crate::scenarios::ingest::append_checked(...)` call at `sync.rs:189`:

```rust
        // The channel mints this identifier from its own data - `positionUid`
        // for one broker, its own account identifier for another - and neither
        // is a place the owner would recognise. It is registered anyway,
        // because the journal's foreign key requires the row to exist, and it
        // is marked `minted` so nothing offers it to him as one of his places.
        //
        // The value itself is not corrected here. It is part of the
        // deduplication fingerprint, so changing it would stop a re-import
        // recognising rows it already holds and double the positions instead
        // (`iaam-xep0`).
        for custody in minted_custody(&operation) {
            register_minted(services, principal.owner, &channel, custody).await?;
        }
```

with, beside `is_affected_trade`:

```rust
/// The custody handles one operation names.
fn minted_custody(operation: &SubmittedOperation) -> Vec<CustodyId> {
    match &operation.kind {
        OperationKind::Buy { custody, .. }
        | OperationKind::Sell { custody, .. }
        | OperationKind::OpeningPosition { custody, .. } => vec![*custody],
        // Spelled out rather than wildcarded: a new variant carrying a handle
        // must fail to compile here, not silently lose it.
        OperationKind::Deposit { .. }
        | OperationKind::Withdrawal { .. }
        | OperationKind::Transfer { .. }
        | OperationKind::Income { .. }
        | OperationKind::Fee { .. }
        | OperationKind::OpeningCash { .. } => Vec::new(),
    }
}

/// Registering one handle, so both append paths do it the same way.
///
/// Not in the same transaction as the append, and that is accepted: the
/// ordering means the only partial state reachable is an unreferenced `minted`
/// row, which no reader consumes - balances and reconciliation are both
/// event-driven, the document directory filters it out, and the bundle carries
/// only reachable ones. The dangerous ordering, an append committing while its
/// custody row is absent, cannot occur.
async fn register_minted(
    services: &AppServices,
    owner: OwnerId,
    channel: &ChannelContext,
    custody: CustodyId,
) -> Result<(), AppError> {
    services
        .directory
        .record_custody_place(
            owner,
            CustodyUpsert {
                id: custody,
                title: format!("{} handle", channel.source.inner()),
                institution: None,
                origin: CustodyOrigin::Minted,
            },
        )
        .await
        .map(|_| ())
}
```

Spell the remaining `OperationKind` arms from the real enum rather than copying
this list. `channel.source` is the real field — `BrokerChannel` exposes
`identity_scope()` and `channel()`, and there is no `source_label()`; read
`ChannelContext` and use what it has.

- [ ] **Step 4: Register in the portfolio loop**

In the claims loop at `sync.rs:288`, after `assertion_event(...)` is built and
before its `append_checked`, register the handle the claim carries:

```rust
        for custody in event_custodies(&event) {
            register_minted(services, principal.owner, &channel, custody).await?;
        }
```

`event_custodies` is `Event::referenced_custodies()` from T8 — the same
traversal the ownership check uses, so the two cannot disagree about which
custody a fact names. If T8 has not landed yet, take the claim's custody
directly from `ControlClaim::PositionQuantity` and leave a comment saying it is
replaced by the traversal; do not write a second partial traversal that will be
forgotten.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p iaam-app 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Full gate and commit**

```bash
make check
git add -A
git commit -m "Broker synchronisation registers the handles both of its appends name (iaam-klpv)"
```

---

### Task 8: The owner check covers every custody a fact names

**Files:**
- Modify: `crates/iaam-core/src/event/mod.rs` (the traversal)
- Modify: `crates/iaam-store/src/journal/write.rs:88-117`
- Create: `crates/iaam-store/tests/schema_coverage.rs` (shared with T7)
- Test: `crates/iaam-store/src/journal/write.rs` unit tests

**Interfaces:**
- Produces: `Event::referenced_custodies(&self) -> impl Iterator<Item = CustodyId> + '_`, consumed by T6 and by `insert_event_in`.

**Acceptance Criteria:**
- A control assertion, a corporate action and an offer exercise naming another owner's place of custody are each refused, in the vocabulary the existing leg-level refusal uses (`journal/write.rs:787`).
- The traversal collects from security legs, `ControlClaim::PositionQuantity`, every `CorporateAction` variant and `OfferExerciseAction::Settled`, with no `..` pattern that lets a new custody field pass unnoticed.
- The check is about **accounts and custody places only**. Instruments are global and have no owner; there is no ownership check over them.
- `schema_coverage.rs` fails when a `REFERENCES custody_places` column is added to an `event*` table without being added to the traversal's coverage list.

**Why this belongs here and not in a later bead.** `ensure_legs_are_owned`
walks `event.legs`, and its own doc comment says the check exists in code or not
at all, because child tables carry no `owner` column. A `PositionQuantity`
assertion is **legless** (`sync.rs:548`) and a partial redemption produces only a
principal leg, while both carry custody in a detail row whose foreign key is
global — `REFERENCES custody_places (id)`, no owner. Nothing can write those
events today because nothing registers a place. **T6 is what makes them
writable**, so this epic is what turns the hole from latent into live.

- [ ] **Step 1: Write the failing test**

In `crates/iaam-store/src/journal/write.rs`'s test module, beside
`a_leg_naming_a_custody_place_of_another_owner_is_refused`, one test per family
asserting the same refusal for a legless control assertion, a corporate action
and a settled offer exercise whose custody belongs to a stranger.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p iaam-store journal::write 2>&1 | tail -20`
Expected: FAIL — all three are accepted, because the check never looks at them.

- [ ] **Step 3: One traversal in the domain**

```rust
/// Every place of custody this fact names.
///
/// One traversal, in the domain, because two readers need the same answer and
/// must not drift: the store's ownership check, and the synchronisation that
/// registers a handle before appending. The store cannot compute it from the
/// legs - a `PositionQuantity` assertion has none, and a partial redemption's
/// only leg is the cash.
///
/// Every arm is spelled out. A `..` here would let a new custody field ship
/// unchecked, which is exactly the defect this method exists to close.
pub fn referenced_custodies(&self) -> impl Iterator<Item = CustodyId> + '_ {
    // Collect into a Vec rather than chaining lazily: the match arms below
    // borrow different shapes, and a reader who has to hold four iterator
    // adapters in mind to see the coverage cannot check the coverage.
    let mut found: Vec<CustodyId> = self.legs.iter().filter_map(|leg| leg.custody).collect();
    match &self.kind {
        EventKind::ControlAssertion { claim, .. } => { /* PositionQuantity's custody */ }
        EventKind::CorporateAction { action } => { /* every variant carrying one */ }
        EventKind::OfferExercise { action } => { /* Settled's custody */ }
        _ => {}
    }
    found.into_iter()
}
```

Fill each arm from the real enum definitions — `grep -n "custody" crates/iaam-core/src/event/mod.rs` and the `CorporateAction` / `OfferExerciseAction` definitions — and match their variants exhaustively rather than using the `_ => {}` shown here, which is a sketch of the shape and not the code to paste.

- [ ] **Step 4: One check in the store**

Replace `ensure_legs_are_owned` with a check that walks `event.legs` for
accounts and `event.referenced_custodies()` for custody places, keeping the
existing refusal wording and the existing reason for reporting a stranger's row
as absent.

- [ ] **Step 5: The coverage guard**

In `crates/iaam-store/tests/schema_coverage.rs`, parse `0001_schema.sql` into
exact `(table, column)` pairs for `REFERENCES custody_places` and
`REFERENCES instruments` on tables whose name starts with `event`, and assert
each set equals an explicit list the code exports. Exact pairs — substring
matching passes vacuously the moment a table already named gains a second such
column, and `event_corporate_action` already has three instrument columns.

- [ ] **Step 6: Run and commit**

```bash
cargo nextest run -p iaam-store 2>&1 | tail -20
make check
git add -A
git commit -m "The owner check covers every custody a fact names, not only its legs (iaam-no3w)"
```

---

### Task 9: A refusal stops promising a capability that does not exist

**Files:**
- Modify: `crates/iaam-ingest/src/csv_source.rs:697-708` (the refusal), `:783-823` (`lookup_custody`'s wording)
- Modify: `crates/iaam-ingest/src/report/tinkoff.rs:450-464`, `crates/iaam-ingest/src/report/finam.rs:452-458`
- Test: `crates/iaam-ingest/tests/report_tinkoff.rs`, `report_finam.rs`

**Acceptance Criteria:**
- The custody refusal no longer offers "a default one for this account": there has never been a default, and this build has no API that registers a custody title. The wording says what would have allowed the cell to resolve and admits that it cannot currently be remedied through the API.
- **No bead identifier appears in a wire response.** The API description carries the limitation; the refusal tells the caller what failed.
- A report position row whose instrument or custody place cannot be resolved produces a located rejection carried in the parsed report — not a `continue`.
- The refusal is row-level: the document's valid cash rows still import. A document-level refusal would be a worse regression than the one being fixed.
- No `let Ok(..) = .. else { continue; }` remains in either reader's row loops.

**What this is.** `iaam-pzxl`. Both report readers spell the position row's
lookup `let Ok(custody) = lookup_custody(...) else { continue; }`. The error is
dropped: no rejection, no verdict, no assertion, and `upload_report` later
appends only what was collected (`scenarios/documents.rs:310`). With an empty
directory that is **every** opening and closing balance in **every** broker
report, gone with nothing saying it was there. The shape is not about custody —
it swallows an unrecognised instrument identically.

- [ ] **Step 1: Write the failing tests**

In `crates/iaam-ingest/tests/report_tinkoff.rs` and `report_finam.rs`: parse a
report holding one cash row and one position row, with an empty custody
directory, and assert the cash row imports while the position row appears as a
refusal carrying its coordinates. Today the position row appears nowhere at all.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p iaam-ingest 2>&1 | tail -20`
Expected: FAIL — no refusal is produced for the position row.

- [ ] **Step 3: Reword the refusal**

```rust
        _ => Err(Rejection {
            field: "custody".into(),
            expected: "a custody title already registered for this owner; this \
                       build has no API that registers one from a document"
                .into(),
            actual: "not specified".into(),
        }),
```

- [ ] **Step 4: Stop dropping position rows**

Replace each `let Ok(x) = ... else { continue; }` in the two readers' position
loops with a branch that pushes a located rejection into the parsed report and
then continues to the next row. Follow the shape the trade-row path already uses
so the two kinds of refusal read the same to a caller.

- [ ] **Step 5: Run and commit**

```bash
cargo nextest run -p iaam-ingest 2>&1 | tail -20
make check
git add -A
git commit -m "A report's position rows are refused with their coordinates rather than dropped (iaam-pzxl)"
```

---

### Task 7: The bundle carries what a restore into an empty database needs

**Files:**
- Modify: `crates/iaam-store/src/bundle.rs` (whole file)
- Modify: `crates/iaam-store/tests/bundle.rs:320-373, 522-553`
- Create: `crates/iaam-store/tests/schema_coverage.rs` (shared with T8 — write it once, cover both)

**Interfaces:**
- Consumes: `CustodyRecord.origin` (Task 1).
- Produces: `Bundle.custody_places: Vec<CustodyPlaceSection>`, `Bundle.instruments: Vec<InstrumentSection>`, `BUNDLE_VERSION = 2`.

**Acceptance Criteria:**
- A bundle exported from a store holding a security event restores into a genuinely empty store with no pre-seeding.
- An archive serialised before this change still deserialises and still verifies its checksum.
- A bundle carrying a non-empty section detects a tampered section: changing one custody place's title breaks the checksum.
- Custody places import with the owner guard; instruments import `ON CONFLICT DO NOTHING`, and a restore does not overwrite an instrument already in the target.
- The closure guard fails when a `REFERENCES instruments` column is added to an `event*` table without being added to the explicit pair list — including a second such column on a table the query already names.

- [ ] **Step 1: Write the failing tests**

In `crates/iaam-store/tests/bundle.rs`, change `a_bundle_round_trip_keeps_the_new_facts` so `refs.register_in(&restored, owner)` is deleted — the restore target stays empty — and rewrite the `BondReferenceData` doc comment at `:320-331` to state what now holds. Then add:

```rust
#[test]
fn an_archive_written_before_the_reference_sections_still_verifies() {
    // A bundle as the previous build produced it: no custody_places key, no
    // instruments key. Deserialising it must yield empty sections, and its
    // checksum must still match — a section nobody wrote contributes no bytes.
    let bundle: Bundle = serde_json::from_str(ARCHIVE_WITHOUT_REFERENCE_SECTIONS)
        .expect("an old archive still reads");
    assert!(bundle.custody_places.is_empty());
    assert!(bundle.instruments.is_empty());
    assert_eq!(bundle.checksum, bundle.compute_checksum());
}

#[test]
fn a_tampered_custody_place_breaks_the_checksum() {
    let (store, owner, _refs) = a_store_holding_one_security_event();
    let mut bundle = store.export_bundle(owner).expect("export");
    assert!(!bundle.custody_places.is_empty());
    bundle.custody_places[0].title.push_str(" (substituted)");
    assert_ne!(bundle.checksum, bundle.compute_checksum());
}
```

`ARCHIVE_WITHOUT_REFERENCE_SECTIONS` is a `const &str` in the test file, and
**how it is produced is the whole point of the test.** Do not export with the
new code and delete the two keys: that yields `bundle_version: 2` and, if the
deleted sections were non-empty, a checksum that never matched anything. Produce
it with the version-1 serialiser — `git stash` this change, export, paste — or
hand-write an archive that reproduces version-1 content and checksum exactly. A
fixture built the wrong way proves only that a version-2 object with empty
skipped fields works, which is not the claim.

Its events must be invented fixtures, never anything derived from a real export.

The closure guard lives in `crates/iaam-store/tests/schema_coverage.rs`, written
once for both this task and T8. **It compares exact `(table, column)` pairs, and
substring matching is not acceptable here** — the earlier draft's fallback,
`EXPORT_SQL.contains("FROM {table}") && EXPORT_SQL.contains(column)`, passes
vacuously for a second instrument column on a table the query already names, and
`event_corporate_action` already has three. Build `REFERENCE_CLOSURE_SQL` from an
explicit `&[(&str, &str)]` list of table-and-column pairs, and have the test
assert that list equals the set parsed out of `0001_schema.sql`. One list, two
readers: the query generator and the test.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p iaam-store --test bundle --test schema_closure 2>&1 | tail -30`
Expected: FAIL — `Bundle` has no `custody_places`, `REFERENCE_CLOSURE_SQL` does not exist, and the round trip fails on the foreign key.

- [ ] **Step 3: The sections**

In `crates/iaam-store/src/bundle.rs`, bump the version and add the sections to **both** structs:

```rust
/// Bundle format version.
///
/// A bundle with a newer version is not read: silently skipping an unknown
/// section would result in data loss. Version 2 adds the reference data an
/// event's legs point at — without it a bundle holding one security leg could
/// not be restored into an empty database at all.
pub const BUNDLE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustodyPlaceSection {
    pub id: uuid::Uuid,
    pub title: String,
    pub institution: Option<String>,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentSection {
    pub id: uuid::Uuid,
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
    pub lineage_parent: Option<uuid::Uuid>,
    pub lineage_reason: Option<String>,
}
```

On `Bundle` and on `BundleContent<'a>` alike — **this is the trap and it is why the attribute appears twice.** `BundleContent` is a separate type assembled by hand in `compute_checksum`; the attribute on `Bundle` alone fixes reading an old archive and leaves the checksum wrong, and on `BundleContent` alone fixes the checksum and leaves old archives unreadable:

```rust
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custody_places: Vec<CustodyPlaceSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruments: Vec<InstrumentSection>,
```

Extend `compute_checksum`'s `BundleContent { .. }` construction with both, and add a doc paragraph on `BundleContent` recording why the two structs must be edited together.

- [ ] **Step 4: Export them**

Add the closure query as a public constant so the guard test can read it:

```rust
/// The instruments an owner's events reach.
///
/// Public so `tests/schema_closure.rs` can check it against the schema: the
/// query is a list of tables, and a list is what goes stale when a ninth table
/// gains an instrument column.
pub const REFERENCE_CLOSURE_SQL: &str = "
    WITH RECURSIVE reached(id) AS (
        SELECT l.instrument FROM event_legs l
          JOIN events e ON e.id = l.event
         WHERE e.owner = ?1 AND l.instrument IS NOT NULL
        UNION SELECT t.instrument FROM event_trade t
          JOIN events e ON e.id = t.event WHERE e.owner = ?1
        UNION SELECT i.instrument FROM event_income i
          JOIN events e ON e.id = i.event WHERE e.owner = ?1 AND i.instrument IS NOT NULL
        UNION SELECT p.instrument FROM event_opening_position p
          JOIN events e ON e.id = p.event WHERE e.owner = ?1
        UNION SELECT v.instrument FROM event_valuation v
          JOIN events e ON e.id = v.event WHERE e.owner = ?1
        UNION SELECT c.instrument FROM event_control_assertion c
          JOIN events e ON e.id = c.event WHERE e.owner = ?1 AND c.instrument IS NOT NULL
        UNION SELECT a.instrument FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.instrument IS NOT NULL
        UNION SELECT a.predecessor FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.predecessor IS NOT NULL
        UNION SELECT a.successor FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.successor IS NOT NULL
        UNION SELECT o.instrument FROM event_offer_exercise o
          JOIN events e ON e.id = o.event WHERE e.owner = ?1 AND o.instrument IS NOT NULL
        UNION SELECT n.lineage_parent FROM instruments n
          JOIN reached r ON n.id = r.id WHERE n.lineage_parent IS NOT NULL
    )
    SELECT n.id, n.kind, n.symbol, n.title,
           n.denomination_currency, n.settlement_currency, n.quote_currency,
           n.lineage_parent, n.lineage_reason
      FROM instruments n JOIN reached r ON n.id = r.id
     ORDER BY n.symbol, n.id
";
```

Verify each table and column name against `0001_schema.sql` before running — the guard test will catch a missing table, not a misspelled one.

In `export_bundle`, fill `instruments` from `REFERENCE_CLOSURE_SQL` and
`custody_places` with **every `declared` row, and only the `minted` rows
reachable from the exported events**. A minted row is as load bearing for the
foreign key as a declared one, so reachable ones must travel; scoping to
reachability also keeps out the unreferenced handles a failed synchronisation
can leave behind (T6 registers before it appends, so an append that fails leaves
one). Both sections are filled before `bundle.checksum = bundle.compute_checksum()`.

- [ ] **Step 5: Import them, before the events**

In `import_bundle`, between the accounts loop and the contours loop:

```rust
        // Before the events loop: `PRAGMA foreign_keys` is on and no constraint
        // in this schema is deferred, so SQLite checks at the statement that
        // violates it, not at COMMIT. Order here is a requirement, not a habit.
        for place in &bundle.custody_places {
            transaction.execute(
                "INSERT INTO custody_places (id, owner, title, institution, origin, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET
                     title = excluded.title,
                     institution = excluded.institution
                 WHERE custody_places.owner = excluded.owner",
                params![
                    place.id.to_string(),
                    owner.inner().to_string(),
                    place.title,
                    place.institution,
                    place.origin,
                    created_at,
                ],
            )?;
        }

        // An archive supplies what is missing and never overwrites what is
        // there. Instruments are global reference data shared with whatever
        // else this database holds, so a restore must not silently rewrite a
        // live directory with a snapshot of an older one. Accounts and custody
        // places are the owner's own and do update, above.
        for instrument in &bundle.instruments {
            transaction.execute(
                "INSERT INTO instruments
                     (id, kind, symbol, title,
                      denomination_currency, settlement_currency, quote_currency,
                      lineage_parent, lineage_reason, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    instrument.id.to_string(),
                    instrument.kind,
                    instrument.symbol,
                    instrument.title,
                    instrument.denomination_currency,
                    instrument.settlement_currency,
                    instrument.quote_currency,
                    instrument.lineage_parent.map(|id| id.to_string()),
                    instrument.lineage_reason,
                    created_at,
                ],
            )?;
        }
```

An instrument whose `lineage_parent` is itself in the section may be inserted before its parent; `instruments.lineage_parent` is a self-referencing foreign key, so sort the section parents-first on export, or insert in two passes. Whichever you choose, add a test with a lineage chain two deep.

Update the module doc comment at the top of `bundle.rs`: the list of what is not yet included loses reference data, and gains one sentence saying what the sections are for.

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p iaam-store 2>&1 | tail -30`
Expected: PASS.

- [ ] **Step 7: Full gate and commit**

```bash
make check
git add -A
git commit -m "Carry the reference data a restore into an empty database needs (iaam-kks6)"
```

---


---

## Deferred to wave 3, behind `iaam-xep0`

The three tasks below were written for the first draft of this plan and are
**not to be implemented** until `iaam-xep0` settles what `CustodyId` means. They
are kept here rather than deleted because the work is real and the reasoning
should not have to be rediscovered — but each of them mints a custody identity
the channels do not share, and the spec's §8 shows why that is silent double
counting rather than a display split.

Read the spec's §8 before reviving any of them. In particular, the two
mitigations that look obvious have both been examined and rejected: a
history-dependent guard (order-dependent, wrong for a legitimate transfer
between depositories, racy outside the append transaction) and a narrower route
attaching the owner's title to an existing minted handle (`positionUid` is per
instrument while a report's title names a section, so one title would name a
dozen handles and be refused as ambiguous).

---

### Deferred: The routes

**Files:**
- Modify: `crates/iaam-server/src/dto.rs`
- Modify: `crates/iaam-server/src/routes.rs` (new handlers beside `create_instrument` at `:574-641`; the deliberately-not-an-`OperationKey` list at `:7216`)
- Modify: `crates/iaam-server/src/openapi.rs` (import list `:17-63`, `components(schemas(...))` around `:445`)
- Modify: `crates/iaam-server/src/lib.rs:76-101` (route registration)
- Test: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `InstrumentDirectory::record_custody_place`, `list_custody_places`, `CustodyView.origin` (Task 2).
- Produces: `POST /v1/custody-places` → 201 `CustodyPlaceDto`; `GET /v1/custody-places` → 200 `CustodyPlaceListDto`.

**Acceptance Criteria:**
- Both routes exist, are registered, and appear in the OpenAPI document.
- Both are gated by `require_admin`, and the new write route is added to the list of write routes deliberately not carrying an `OperationKey`, so `every_offered_route_is_gated_by_the_floor_it_publishes` passes.
- An agent-scoped token receives 403 from the write route.
- The list route returns declared places only: a minted row is absent.

- [ ] **Step 1: Write the failing test**

In `crates/iaam-server/tests/contract.rs`:

```rust
#[tokio::test]
async fn a_custody_place_is_created_and_listed_and_a_minted_one_is_not() {
    let harness = harness();

    let created = harness
        .post_json(
            "/v1/custody-places",
            serde_json::json!({"title": "Broker One", "institution": "Broker One"}),
        )
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    // A handle a channel minted is registered through the port, not the route,
    // and the list does not offer it as one of his places.
    harness
        .services
        .directory
        .record_custody_place(
            harness.owner,
            iaam_app::ports::CustodyUpsert {
                id: CustodyId::new_random(),
                title: "broker-channel handle".to_owned(),
                institution: None,
                origin: CustodyOrigin::Minted,
            },
        )
        .await
        .expect("minted place");

    let listed = harness.get_json("/v1/custody-places").await;
    let places = listed["custody_places"].as_array().expect("array");
    assert_eq!(places.len(), 1);
    assert_eq!(places[0]["title"], "Broker One");
}

#[tokio::test]
async fn an_agent_token_may_not_create_a_custody_place() {
    let harness = harness();
    let response = harness
        .post_json_as_agent(
            "/v1/custody-places",
            serde_json::json!({"title": "Broker One"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
```

Use whatever the harness's actual request helpers are named — read the neighbouring `create_instrument` contract tests and copy their shape rather than inventing one.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p iaam-server --test contract custody_place 2>&1 | tail -20`
Expected: FAIL — 404, the route does not exist.

- [ ] **Step 3: The DTOs**

In `crates/iaam-server/src/dto.rs`, beside the instrument DTOs:

```rust
#[derive(Debug, Serialize, ToSchema)]
pub struct CustodyPlaceDto {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CustodyPlaceListDto {
    pub custody_places: Vec<CustodyPlaceDto>,
}

/// Creating a place of custody.
///
/// There is no `origin` field, and there will not be one: a place created
/// through this route is the owner's by construction. A handle a channel mints
/// is registered by the channel, and is not something a caller may ask for.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCustodyPlaceRequest {
    #[serde(default)]
    pub id: Option<Uuid>,
    pub title: String,
    #[serde(default)]
    pub institution: Option<String>,
}
```

- [ ] **Step 4: The handlers**

In `crates/iaam-server/src/routes.rs`, beside `create_instrument`:

```rust
/// Register a place of custody.
///
/// Permission is checked before the body is parsed, as it is for an instrument:
/// an agent token receives 403 even for a body that is itself invalid (§7, §14).
///
/// The ordinary way a place appears is an account's institution deriving one.
/// This route exists for the case a document names a custodian the owner keeps
/// under a title of his own.
#[utoipa::path(
    post,
    path = "/v1/custody-places",
    request_body = CreateCustodyPlaceRequest,
    responses(
        (status = 201, description = "Place of custody recorded", body = CustodyPlaceDto),
        (status = 403, description = "Insufficient permissions", body = ApiError),
        (status = 422, description = "Invalid place of custody", body = ApiError),
        (status = 400, description = "Request body could not be read", body = ApiError),
        (status = 413, description = "Request body exceeds the limit", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_custody_place(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    ApiBytes(body): ApiBytes,
) -> Result<(StatusCode, Json<CustodyPlaceDto>), ApiFailure> {
    require_admin(&principal)?;
    let request: CreateCustodyPlaceRequest = serde_json::from_slice(&body)
        .map_err(|error| invalid_field("body", "custody place JSON object", error.to_string()))?;
    if request.title.trim().is_empty() {
        return Err(invalid_field("title", "the title the owner gives this place", String::new()).into());
    }
    let id = CustodyId(request.id.unwrap_or_else(Uuid::new_v4));
    state
        .services
        .directory
        .record_custody_place(
            principal.owner,
            iaam_app::ports::CustodyUpsert {
                id,
                title: request.title.clone(),
                institution: request.institution.clone(),
                origin: iaam_core::custody::CustodyOrigin::Declared,
            },
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CustodyPlaceDto {
            id: id.inner(),
            title: request.title,
            institution: request.institution,
        }),
    ))
}

/// The owner's places of custody.
///
/// Declared places only. A handle a broker channel minted is in the table so
/// the journal's foreign key holds; it is not one of his places and is not
/// offered as one.
#[utoipa::path(
    get,
    path = "/v1/custody-places",
    responses(
        (status = 200, description = "Places of custody", body = CustodyPlaceListDto),
        (status = 403, description = "Insufficient permissions", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_custody_places(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<CustodyPlaceListDto>, ApiFailure> {
    require_admin(&principal)?;
    let custody_places = state
        .services
        .directory
        .list_custody_places(principal.owner)
        .await?
        .into_iter()
        .filter(|place| place.origin == iaam_core::custody::CustodyOrigin::Declared)
        .map(|place| CustodyPlaceDto {
            id: place.id.inner(),
            title: place.title,
            institution: place.institution,
        })
        .collect();
    Ok(Json(CustodyPlaceListDto { custody_places }))
}
```

- [ ] **Step 5: Register**

In `crates/iaam-server/src/lib.rs`, beside the instrument line:

```rust
    .routes(routes!(routes::list_custody_places, routes::create_custody_place))
```

In `crates/iaam-server/src/openapi.rs`, add `CustodyPlaceDto, CustodyPlaceListDto, CreateCustodyPlaceRequest` to the `use crate::dto::{...}` list and to `components(schemas(...))`.

In `crates/iaam-server/src/routes.rs:7216`, add `create_custody_place` to the list of write routes deliberately not carrying an `OperationKey`, with the same one-line reason the instrument routes carry: it writes reference data an agent token never reaches.

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p iaam-server 2>&1 | tail -20`
Expected: PASS, including `every_offered_route_is_gated_by_the_floor_it_publishes`.

- [ ] **Step 7: Full gate and commit**

```bash
make check
git add -A
git commit -m "Publish the routes that register and list places of custody (iaam-klpv)"
```

---

---

### Deferred: An account's institution derives a place

**Files:**
- Modify: the `SqliteStore` account write (`create_account` and the account upsert — find with `grep -rn "fn create_account\|fn upsert_account" crates/iaam-store/src/`)
- Test: `crates/iaam-store/tests/instrument_directory.rs` and `crates/iaam-app/tests/custody_places.rs`

**Interfaces:**
- Consumes: `custody_place_by_title`, `upsert_custody_place`, `CustodyOrigin::Declared` (Task 1).
- Produces: after any account write carrying `institution = Some(name)`, the owner has exactly one declared place titled `name`.

**Acceptance Criteria:**
- Creating an account with an institution creates the owner's declared place for it, titled with the institution's name, in the same transaction as the account.
- Creating a second account at the same institution creates no second place.
- An account with no institution derives no place.
- The derivation is idempotent: writing the same account twice leaves one place.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn an_accounts_institution_derives_the_owners_place() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();

    create_account_in(&store, owner, "Brokerage", Some("Broker One"));
    create_account_in(&store, owner, "Second brokerage", Some("Broker One"));
    create_account_in(&store, owner, "Cash", None);

    let places = store.list_custody_places(owner).expect("list");
    assert_eq!(places.len(), 1, "one institution, one place");
    assert_eq!(places[0].title, "Broker One");
    assert_eq!(places[0].origin, CustodyOrigin::Declared);
}
```

`create_account_in` is a helper in the test file calling whichever store function the account routes call; read it before writing this and match its real signature.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p iaam-store --test instrument_directory 2>&1 | tail -20`
Expected: FAIL — `places.len()` is 0.

- [ ] **Step 3: Derive inside the account write**

In the account write, after the account row is inserted and inside the same transaction:

```rust
/// An account names the institution that keeps it, and a place of custody is
/// what a security leg on that account will have to point at. Deriving it here
/// rather than at a route means every path that creates an account — the route,
/// the batch route, a restore — leaves the owner able to record a security
/// without ever being asked for a depository he has no information about.
///
/// Keyed by title rather than by a derived identifier: the owner may rename the
/// place, and a rename must not orphan the legs already pointing at it.
fn ensure_institution_place(
    transaction: &rusqlite::Transaction<'_>,
    owner: OwnerId,
    institution: Option<&str>,
    created_at: &str,
) -> Result<(), StoreError> {
    let Some(institution) = institution.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(());
    };
    let known: Option<String> = transaction
        .query_row(
            "SELECT id FROM custody_places
             WHERE owner = ?1 AND origin = 'declared' AND title = ?2",
            params![owner.inner().to_string(), institution],
            |row| row.get(0),
        )
        .optional()?;
    if known.is_some() {
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO custody_places (id, owner, title, institution, origin, created_at)
         VALUES (?1, ?2, ?3, ?4, 'declared', ?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            owner.inner().to_string(),
            institution,
            institution,
            created_at,
        ],
    )?;
    Ok(())
}
```

Call it from `create_account` and from the account upsert. If the account write is not currently transactional, wrap it — an account whose place failed to derive is a state that leaves a security unrecordable with nothing saying why.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p iaam-store 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

```bash
make check
git add -A
git commit -m "Derive the owner's place of custody from his account's institution (iaam-klpv)"
```

---

---

### Deferred: A report row without a named custodian uses the account's place

**Files:**
- Modify: `crates/iaam-ingest/src/csv_source.rs:50-62` (`Directory`), `:602-626` (`row_to_operation`), `:628-652` (`build_kind`), `:688-708` (`resolve_custody`), `:737-781` (`build_trade`)
- Modify: `crates/iaam-server/src/routes.rs:7388-7423` (`build_directory`)
- Test: `crates/iaam-ingest/src/csv_source.rs` unit tests (`:1320-1340`), `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `CustodyView.origin` (Task 2), derived places (Task 4).
- Produces: `Directory.default_custody: BTreeMap<AccountId, CustodyId>`; `resolve_custody(name, account, directory)`; `build_kind(row, account, directory, currency, date)`; `build_trade(row, account, directory, currency, date)`.

**Acceptance Criteria:**
- A CSV buy row that names no custody column resolves to the place derived from its account's institution.
- A row on an account whose institution derives no place is refused, and the refusal names what is missing rather than saying "not specified".
- A row naming a custodian by title still resolves by title, and a title belonging to a minted row does not resolve.
- The contract harness can now drive a CSV buy row all the way to the custody column — the coverage `iaam-zhej` records as unreachable.

- [ ] **Step 1: Write the failing tests**

In `crates/iaam-ingest/src/csv_source.rs`'s unit tests:

```rust
#[test]
fn an_unnamed_custody_takes_the_place_of_its_own_account() {
    let brokerage = AccountId::new_random();
    let other = AccountId::new_random();
    let place = CustodyId::new_random();
    let elsewhere = CustodyId::new_random();
    let mut directory = Directory::default();
    directory.default_custody.insert(brokerage, place);
    directory.default_custody.insert(other, elsewhere);

    assert_eq!(resolve_custody(None, brokerage, &directory).unwrap(), place);
    assert_eq!(resolve_custody(Some(""), other, &directory).unwrap(), elsewhere);
}

#[test]
fn an_account_with_no_place_is_refused_by_what_is_missing() {
    let account = AccountId::new_random();
    let directory = Directory::default();
    let rejection = resolve_custody(None, account, &directory).unwrap_err();
    assert_eq!(rejection.field, "custody");
    assert!(
        rejection.expected.contains("institution"),
        "the refusal must say what would produce a place: {}",
        rejection.expected
    );
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p iaam-ingest 2>&1 | tail -20`
Expected: compilation failure — `default_custody` is an `Option`, and `resolve_custody` takes two arguments.

- [ ] **Step 3: Make the default per account**

```rust
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub accounts: AccountNames,
    pub custodies: CustodyNames,
    pub instruments: InstrumentAliases,
    /// The place a row on this account falls back to when it names none.
    ///
    /// Per account rather than one for the whole owner: he holds securities
    /// through more than one broker, and one default across all of them would
    /// file a holding at the wrong broker without anything refusing it.
    pub default_custody: BTreeMap<AccountId, CustodyId>,
}
```

```rust
fn resolve_custody(
    name: Option<&str>,
    account: AccountId,
    directory: &Directory,
) -> Result<CustodyId, Rejection> {
    match name {
        Some(name) if !name.is_empty() => resolve_named_custody(name, directory, "custody"),
        _ => directory
            .default_custody
            .get(&account)
            .copied()
            .ok_or_else(|| Rejection {
                field: "custody".into(),
                expected: "a place of custody of the owner's, or an institution \
                           declared on this account for one to be derived from"
                    .into(),
                actual: "not specified".into(),
            }),
    }
}
```

Thread `account: AccountId` through `build_kind` and `build_trade`; `row_to_operation` already has it at `csv_source.rs:605`, one line before the `build_kind` call, so this is threading and not reordering. Update every call site the compiler names.

- [ ] **Step 4: Fill the map**

In `crates/iaam-server/src/routes.rs`'s `build_directory`, build the title map from declared places only, and the per-account default by matching each account's `institution` to the declared place carrying that title:

```rust
    let mut by_title: BTreeMap<String, CustodyId> = BTreeMap::new();
    for place in places {
        if place.origin != iaam_core::custody::CustodyOrigin::Declared {
            // A handle a channel minted has no title anybody chose. Offering it
            // here would let a hundred of them make a real title ambiguous, and
            // `lookup_custody` refuses an ambiguous title.
            continue;
        }
        by_title.insert(place.title.clone(), place.id);
        directory
            .custodies
            .entry(place.title)
            .or_default()
            .push(place.id);
    }
    for account in accounts.details() {
        if let Some(place) = account
            .institution
            .as_deref()
            .and_then(|name| by_title.get(name))
        {
            directory.default_custody.insert(account.id, *place);
        }
    }
```

`accounts.details()` stands for whatever `AccountDirectory` already exposes that carries each account's id and institution; read `AccountDirectory::load` and use its real accessor, adding one if it exposes only names.

- [ ] **Step 5: Add the contract coverage `iaam-zhej` asks for**

In `crates/iaam-server/tests/contract.rs`, a test that: creates an account with an institution, registers an instrument and its alias, posts a CSV buy row naming no custody column, and asserts the row is accepted; and a second that posts a row naming an unknown custodian and asserts the refusal names the custody field.

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p iaam-ingest -p iaam-server 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Full gate and commit**

```bash
make check
git add -A
git commit -m "A report row with no custodian named takes its account's place (iaam-klpv)"
```

---

---

## After the waves

- Update `iaam-xep0` with what this work established: `positionUid` is a property of the instrument rather than a place; `custody` is part of the deduplication fingerprint (`ingest/dedup.rs:280`) so correcting it requires reversal and re-import; `Balances::quantity_of` sums across custody (`projection/balances.rs:110`) so two namespaces double a holding; and `ground_three` awards agreement per dimension rather than per subject (`reconciliation/mod.rs:654, 689`) so two channels can each reconcile against their own silo and jointly produce a false `BrokerApiAgreesWithStatement`. It is raised to P1.
- `iaam-zhej` stays open: the contract coverage it wants needs a registered custody place, and that path is in wave 3.
- The spec's §8 cost — securities cannot enter from a broker report until wave 3 — is stated in the API description where a caller meets it, not only in the spec.
