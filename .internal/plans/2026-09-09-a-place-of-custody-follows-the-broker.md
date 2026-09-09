# A Place of Custody Follows the Broker — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** Make it possible to record a security fact in production again — by giving `custody_places` a write path, deriving a place from the account's institution, marking the handles a broker channel mints so they never pose as places, and making an archive restorable into an empty database.

**Architecture:** `custody_places` gains one column, `origin`, splitting the table into places the owner would recognise (`declared`) and bare handles a channel produced (`minted`). Declared places are derived from an account's institution and offered to a document reader by title; minted ones exist only so the foreign key holds. The bundle grows two sections — custody places and the instrument closure its events reach — guarded by a test that reads the schema.

**Tech Stack:** Rust workspace, `rusqlite` on SQLite (`STRICT` tables, `PRAGMA foreign_keys` on), `axum` + `utoipa`/`utoipa_axum` for routes and OpenAPI, `ciborium` for the bundle checksum, `cargo nextest`.

**Spec:** `.internal/specs/2026-09-09-a-place-of-custody-follows-the-broker-design.md`
**Beads:** `iaam-klpv` (P0, T1–T6), `iaam-kks6` (P2, T7).

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
| `crates/iaam-store/src/accounts.rs` (or wherever `create_account` lives) | derive the institution's place inside the account write |
| `crates/iaam-app/src/ports.rs` | `CustodyUpsert`, `CustodyView.origin`, `InstrumentDirectory::record_custody_place` |
| `crates/iaam-app/src/adapters/sqlite.rs` | the sole implementation of the new port method |
| `crates/iaam-server/src/dto.rs` | `CustodyPlaceDto`, `CustodyPlaceListDto`, `CreateCustodyPlaceRequest` |
| `crates/iaam-server/src/routes.rs` | `POST`/`GET /v1/custody-places`; `build_directory` filters and fills |
| `crates/iaam-server/src/openapi.rs` | schema registration |
| `crates/iaam-server/src/lib.rs` | route registration |
| `crates/iaam-ingest/src/csv_source.rs` | per-account custody default; thread the account through `build_kind`/`build_trade` |
| `crates/iaam-app/src/scenarios/sync.rs` | register a minted place before appending |
| `crates/iaam-store/src/bundle.rs` | two new sections, `BUNDLE_VERSION` 2, import order, closure query |
| `crates/iaam-store/tests/bundle.rs` | restore into a genuinely empty store |
| `crates/iaam-store/tests/schema_closure.rs` | **new** — the guard that reads the schema |

---

### Task 1: `origin` — the column that keeps a handle from posing as a place

**Files:**
- Create: `crates/iaam-core/src/custody.rs`
- Modify: `crates/iaam-core/src/lib.rs` (add `pub mod custody;` beside the other module declarations)
- Modify: `crates/iaam-store/migrations/0001_schema.sql:153-163`
- Modify: `crates/iaam-store/src/reference.rs:288-294` (`CustodyRecord`), `:768-791` (`upsert_custody_place`), `:793-816` (`list_custody_places`)
- Test: `crates/iaam-store/tests/instrument_directory.rs`

**Interfaces:**
- Produces: `iaam_core::custody::CustodyOrigin` with `Declared` and `Minted` variants, `const fn code(self) -> &'static str` and `fn from_code(code: &str) -> Option<Self>`; `iaam_store::reference::CustodyRecord { id, owner, title, institution, origin }`; `SqliteStore::custody_place_by_title(owner, title) -> Result<Option<CustodyRecord>, StoreError>`.

**Acceptance Criteria:**
- A custody place round-trips its origin through `upsert_custody_place` / `list_custody_places`.
- A row whose `origin` is neither `declared` nor `minted` cannot be written: the database refuses it.
- `custody_place_by_title` finds a declared place by the owner's title and does not find another owner's.
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
fn a_title_reaches_only_its_own_owners_place() {
    let store = SqliteStore::open_in_memory().expect("memory store");
    let owner = OwnerId::new_random();
    let stranger = OwnerId::new_random();
    let place = CustodyId::new_random();

    store
        .upsert_custody_place(&CustodyRecord {
            id: place,
            owner,
            title: "Broker One".to_owned(),
            institution: Some("Broker One".to_owned()),
            origin: CustodyOrigin::Declared,
        })
        .expect("declared place");

    assert_eq!(
        store
            .custody_place_by_title(owner, "Broker One")
            .expect("lookup")
            .map(|record| record.id),
        Some(place)
    );
    assert!(
        store
            .custody_place_by_title(stranger, "Broker One")
            .expect("lookup")
            .is_none()
    );
}
```

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

-- A declared place is found by the owner's title for it, both when a document
-- names one and when an account's institution derives one.
CREATE INDEX custody_places_by_title ON custody_places (owner, origin, title);
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

/// The owner's declared place carrying this title, if he has one.
///
/// Minted rows are excluded rather than filtered by the caller: a handle has no
/// title anybody chose, and letting one answer a lookup by title is the whole
/// failure the column exists to prevent.
pub fn custody_place_by_title(
    &self,
    owner: OwnerId,
    title: &str,
) -> Result<Option<CustodyRecord>, StoreError> {
    let mut statement = self.conn.prepare(
        "SELECT id, title, institution FROM custody_places
         WHERE owner = ?1 AND origin = 'declared' AND title = ?2
         ORDER BY id LIMIT 1",
    )?;
    let found = statement
        .query_row(params![owner.inner().to_string(), title], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .optional()?;
    found
        .map(|(id, title, institution)| {
            Ok(CustodyRecord {
                id: CustodyId(parse_uuid(&id, "custody")?),
                owner,
                title,
                institution,
                origin: CustodyOrigin::Declared,
            })
        })
        .transpose()
}
```

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

### Task 3: The routes

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

### Task 4: An account's institution derives a place

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

### Task 5: A report row without a named custodian uses the account's place

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

### Task 6: Broker synchronisation registers the handle it mints

**Files:**
- Modify: `crates/iaam-app/src/scenarios/sync.rs:139-207`
- Test: `crates/iaam-app/tests/sync.rs`

**Interfaces:**
- Consumes: `InstrumentDirectory::record_custody_place`, `CustodyOrigin::Minted` (Task 2).

**Acceptance Criteria:**
- A store holding no custody place at all synchronises a broker trade successfully: the sync registers the handle before appending.
- The registered row's origin is `minted`, and it is absent from `GET /v1/custody-places`.
- The value in the leg is unchanged — no fingerprint and no reconciliation key moves. A test asserts the custody id on the appended leg equals the one the channel produced.
- `crates/iaam-app/tests/sync.rs` stops calling `seed_custody` for the ids the channel mints; where it still seeds one, that is a declared place and says so.

- [ ] **Step 1: Write the failing test**

In `crates/iaam-app/tests/sync.rs`, a test that builds `services_with(today, |_store| {})` — seeding **no** custody place — drives a fake channel that returns one buy operation, and asserts the sync records it rather than refusing.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p iaam-app --test sync 2>&1 | tail -20`
Expected: FAIL — the append is refused because the custody place is not registered.

- [ ] **Step 3: Register before appending**

In `sync.rs`'s per-operation loop, immediately before the `crate::scenarios::ingest::append_checked(...)` call:

```rust
        // The channel mints this identifier from its own data — `positionUid`
        // for one broker, its own account identifier for another — and neither
        // is a place the owner would recognise. It is registered anyway,
        // because the journal's foreign key requires the row to exist, and it
        // is marked `minted` so nothing offers it to him as one of his places.
        //
        // The value itself is not corrected here. It is part of the
        // deduplication fingerprint, so changing it would stop a re-import
        // recognising rows it already holds and double the positions instead
        // (`iaam-xep0`).
        for custody in minted_custody(&operation) {
            services
                .directory
                .record_custody_place(
                    principal.owner,
                    CustodyUpsert {
                        id: custody,
                        title: format!("{} handle", channel.source_label()),
                        institution: None,
                        origin: CustodyOrigin::Minted,
                    },
                )
                .await?;
        }
```

with a small helper beside `is_affected_trade`:

```rust
/// The custody handles one operation names.
///
/// A function rather than a field read, because the handle lives inside
/// `OperationKind` and only some variants carry one.
fn minted_custody(operation: &SubmittedOperation) -> Vec<CustodyId> {
    match &operation.kind {
        OperationKind::Buy { custody, .. } | OperationKind::Sell { custody, .. } => vec![*custody],
        _ => Vec::new(),
    }
}
```

Extend the match arms to every `OperationKind` variant carrying a `custody` field — `grep -n "custody" crates/iaam-ingest/src/operation.rs` lists them at `:187`, `:210`, `:275`. A wildcard arm that silently drops a new variant's handle is not acceptable here: match the variants exhaustively so the compiler names a new one.

`channel.source_label()` stands for whatever the channel already exposes as its name; if it exposes none, use the `SourceId` rendered as text rather than inventing a label.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p iaam-app 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

```bash
make check
git add -A
git commit -m "Broker synchronisation registers the handle it mints (iaam-klpv)"
```

---

### Task 7: The bundle carries what a restore into an empty database needs

**Files:**
- Modify: `crates/iaam-store/src/bundle.rs` (whole file)
- Modify: `crates/iaam-store/tests/bundle.rs:320-373, 522-553`
- Create: `crates/iaam-store/tests/schema_closure.rs`

**Interfaces:**
- Consumes: `CustodyRecord.origin` (Task 1).
- Produces: `Bundle.custody_places: Vec<CustodyPlaceSection>`, `Bundle.instruments: Vec<InstrumentSection>`, `BUNDLE_VERSION = 2`.

**Acceptance Criteria:**
- A bundle exported from a store holding a security event restores into a genuinely empty store with no pre-seeding.
- An archive serialised before this change still deserialises and still verifies its checksum.
- A bundle carrying a non-empty section detects a tampered section: changing one custody place's title breaks the checksum.
- Custody places import with the owner guard; instruments import `ON CONFLICT DO NOTHING`, and a restore does not overwrite an instrument already in the target.
- The closure guard fails when a `REFERENCES instruments` or `REFERENCES custody_places` column is added to an `event*` table without being added to the export query.

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

`ARCHIVE_WITHOUT_REFERENCE_SECTIONS` is a `const &str` in the test file: produce it by exporting a bundle with the new code, deleting the two keys from the JSON, and pasting the result — its events must be invented fixtures, never anything derived from a real export.

Create `crates/iaam-store/tests/schema_closure.rs`:

```rust
//! The export's reference-data closure is a list, and a list goes stale.
//!
//! Eight event tables carry a column referencing `instruments` today, and all
//! eight arrived in one change. A ninth added later would export an incomplete
//! closure and produce an archive that fails on restore — the exact failure the
//! sections exist to remove. So the schema is read, not remembered.

const SCHEMA: &str = include_str!("../migrations/0001_schema.sql");
const EXPORT_SQL: &str = iaam_store::bundle::REFERENCE_CLOSURE_SQL;

fn referencing_columns(target: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut table = String::new();
    for line in SCHEMA.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("CREATE TABLE ") {
            table = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches('(')
                .to_owned();
        }
        if trimmed.starts_with("--") || !trimmed.contains(&format!("REFERENCES {target}")) {
            continue;
        }
        if !table.starts_with("event") {
            continue;
        }
        if let Some(column) = trimmed.split_whitespace().next() {
            found.push((table.clone(), column.to_owned()));
        }
    }
    found
}

#[test]
fn every_event_column_referencing_an_instrument_is_in_the_export_closure() {
    let columns = referencing_columns("instruments");
    assert!(!columns.is_empty(), "the parser found nothing: fix the parser");
    for (table, column) in columns {
        assert!(
            EXPORT_SQL.contains(&format!("{table}.{column}"))
                || EXPORT_SQL.contains(&format!("FROM {table}")) && EXPORT_SQL.contains(&column),
            "{table}.{column} references instruments and is not in the export closure"
        );
    }
}

#[test]
fn every_event_column_referencing_a_custody_place_is_covered() {
    // Custody places export whole per owner rather than by closure, so the
    // requirement is weaker: this test exists to fail loudly if that ever
    // changes to a closure without the guard changing with it.
    let columns = referencing_columns("custody_places");
    assert!(!columns.is_empty(), "the parser found nothing: fix the parser");
}
```

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

In `export_bundle`, fill `custody_places` from `self.list_custody_places(owner)?` (**both** origins: a minted row is as load bearing for the foreign key as a declared one) and `instruments` from `REFERENCE_CLOSURE_SQL`. Both before `bundle.checksum = bundle.compute_checksum()`.

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

## After the tasks

- Update `iaam-xep0` with what this work established: `positionUid` is a property of the instrument rather than a place; `custody` is part of the deduplication fingerprint (`ingest/src/dedup.rs:280`) so correcting it requires reversal and re-import; and the `minted` marker names exactly the rows a repair would retract. Raise it from P3.
- Close `iaam-zhej` if Task 5's contract coverage satisfies it; if it does not, say which part remains.
- The spec's §8 consequence — a holding imported from a document and the same holding synchronised from the API can now carry different custody values and reconcile as two positions — is written where a reader of a position meets it, not only in the spec.
