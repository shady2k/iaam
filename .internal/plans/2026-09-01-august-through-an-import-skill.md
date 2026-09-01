# August, entered through an import skill — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use beads-superpowers:subagent-driven-development (recommended) or beads-superpowers:executing-plans to implement this plan task-by-task. Each Task becomes a bead (`bd create -t task --parent <epic-id>`). Steps within tasks use checkbox (`- [ ]`) syntax for human readability.

**Goal:** The owner's August is in the journal, entered by a reusable import
skill rather than by hand, and the flow report answers "сколько пришло, сколько
ушло и куда" with a named discrepancy.

**Architecture:** Three bounded gaps in `crates/` are closed first, because
without them the category list cannot be created, description rules can never
match, and a per-row decision is unreachable. Then the institution's knowledge
goes into an **import skill** outside the crates, developed against a synthetic
fixture and run against the owner's real export. No crate learns the word
"T-Bank"; no skill learns the owner's account names.

**Tech Stack:** Rust (axum, utoipa, rusqlite, serde), SQLite, `bd` for
tracking, an agent skill in `.claude/skills/`.

Spec: `.internal/specs/2026-09-01-bank-import-skill-design.md`.
Companion spec: `.internal/specs/2026-09-01-money-flow-design.md`.

## Global Constraints

- **English only** in code, comments, tests, doc comments and new documents
  (`CLAUDE.md`). Values that come from a source stay verbatim — the T-Bank
  column name `"Категория по-умолчанию"` and the value `"Супермаркеты"` are
  data, not our text. Take domain terms from `docs/glossary-ru-en.md`; the
  import vocabulary was added there on 2026-09-01.
- **The gate is `make check`** — `fmt`, `clippy --workspace --all-targets
  --all-features -D warnings`, `./scripts/check-architecture.sh`,
  `./scripts/check-fixtures.sh`, `deps`, `test`, `doc-test`. `cargo check -p
  <crate>` builds neither test targets nor other crates' binaries and has let
  five breakages through before; never use it as the gate.
- **`cargo` is not on `PATH`.** Every command runs as
  `direnv exec /home/dev/repos/iaam <command>`, or inside `nix develop`. In a
  fresh worktree, `direnv allow` first.
- **`match` on `EventKind` stays exhaustive**, no `_` arm. A new event kind must
  break the build rather than silently become a discrepancy.
- **Money is summed in the core**, never in a route or an adapter. The
  architecture guard enforces it.
- **`#[must_use]` on a function returning `Result` is an error**
  (`double_must_use`). More than six arguments is an error; fix with a parameter
  object, never `#[allow]`.
- **No file may be added under `tests/fixtures/`** unless it is both listed in
  `tests/fixtures/MANIFEST.sha256` and mentioned by name in a `.rs` file under
  `crates/` — `scripts/check-fixtures.sh` rejects both halves. The skill's
  fixture therefore lives beside the skill, not there.
- **The owner's data never enters the repository.** Not the export, not account
  names, not balances, not counterparty names. `*.db` is already gitignored;
  keep it that way.

---

### Task 1: A category group can be created over HTTP

Today `POST /v1/categories` demands a group identifier and there is no way to
obtain one: `create_group` exists at
`crates/iaam-app/src/scenarios/categories.rs:49` and is called from no route.
The existing contract test reaches around the gap by calling
`store.insert_category_group` directly
(`crates/iaam-server/tests/contract.rs:4866`), which is the proof that the hole
is real. Without this task the owner's category list cannot be started and
money-flow §3 is unreachable.

**Files:**
- Modify: `crates/iaam-store/src/categories.rs` — add `list_groups`; the table
  is written at line 66 and retired at line 238, and never read back
- Modify: `crates/iaam-app/src/ports.rs:375` — add `list_groups` to
  `CategoryStore`, and its refusal in `UnavailableCategoryStore` (line 748)
- Modify: `crates/iaam-app/src/adapters/sqlite.rs` — implement it, mapping rows
  through the existing `category_group_view` helper at line 821
- Modify: `crates/iaam-app/src/scenarios/categories.rs` — add `list_groups`
  beside `list_categories` (line 71)
- Modify: `crates/iaam-server/src/dto.rs` — add `CategoryGroupRequest` and
  `CategoryGroupDto`
- Modify: `crates/iaam-server/src/routes.rs` — add `list_category_groups` and
  `create_category_group_route`
- Modify: `crates/iaam-server/src/lib.rs:141` — register the routes next to the
  category routes
- Test: `crates/iaam-server/tests/contract.rs`

**Note on the size of this task.** `CategoryStore` has `create_group` and
`retire_group` but **no** `list_groups` (`crates/iaam-app/src/ports.rs:375`),
and `crates/iaam-store/src/categories.rs` writes the `category_groups` table
without ever reading it. The listing therefore has to be added through all five
layers. Adding only `POST` would leave a group the owner can create and never
find again.

**Interfaces:**
- Consumes: `iaam_app::scenarios::categories::create_group(services,
  principal, title) -> Result<CategoryGroupId, AppError>`; the group listing
  port `services.categories` (see `crates/iaam-app/src/ports.rs:338` for
  `CategoryGroupView { id, title, retired_at }`).
- Produces: `POST /v1/category-groups` returning `201` with
  `{"id": Uuid, "title": String, "retired_at": Option<String>}`, and
  `GET /v1/category-groups` returning that shape as an array. Task 7 uses the
  returned `id` as the `group` field of `POST /v1/categories`.

**Acceptance Criteria:**
- `POST /v1/category-groups` with `{"title": "Usual Expenses"}` returns `201`
  and an `id` usable as the `group` of `POST /v1/categories`.
- `GET /v1/category-groups` lists it.
- A non-owner token receives `403`.
- An empty title receives `422` and names the field.
- The whole flow — create group, create category — works over HTTP with no
  direct store access.

- [ ] **Step 1: Write the failing test**

In `crates/iaam-server/tests/contract.rs`, beside the existing category tests:

```rust
#[tokio::test]
async fn a_category_group_can_be_created_and_then_holds_a_category() {
    let (harness, _path) = harness_on_disk();

    let (status, group) = call(
        &harness.router,
        post(
            "/v1/category-groups",
            &harness.owner_token,
            &json!({"title": "Usual Expenses"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{group}");
    let group_id = group["id"].as_str().expect("group id").to_owned();

    let (status, listed) = call(
        &harness.router,
        get("/v1/category-groups", Some(&harness.owner_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed.as_array().expect("group list").len(), 1);

    // The point of the route: the group it returns is usable straight away.
    let (status, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group_id, "title": "Food"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{category}");
}

#[tokio::test]
async fn a_category_group_without_a_title_is_refused_by_field() {
    let (harness, _path) = harness_on_disk();

    let (status, body) = call(
        &harness.router,
        post(
            "/v1/category-groups",
            &harness.owner_token,
            &json!({"title": "   "}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["field"], "title", "{body}");
}
```

- [ ] **Step 2: Run the test and watch it fail**

```
direnv exec /home/dev/repos/iaam cargo test -p iaam-server --test contract a_category_group
```

Expected: FAIL — the response is `404`, because the route does not exist.

- [ ] **Step 3: Add the DTOs**

In `crates/iaam-server/src/dto.rs`, beside `CategoryRequest`:

```rust
/// A new category group.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CategoryGroupRequest {
    pub title: String,
}

/// A category group, active or retired.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryGroupDto {
    pub id: Uuid,
    pub title: String,
    pub retired_at: Option<String>,
}
```

- [ ] **Step 4: Add the routes**

In `crates/iaam-server/src/routes.rs`, beside `create_category_route`. Reject a
blank title here rather than letting an unnamed group into the reference data:
a group with no name cannot be chosen from a list later.

```rust
#[utoipa::path(
    get,
    path = "/v1/category-groups",
    responses(
        (status = 200, description = "Owner category groups", body = Vec<CategoryGroupDto>),
        (status = 403, description = "Owner only", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn list_category_groups(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<CategoryGroupDto>>, ApiFailure> {
    require_admin(&principal)?;
    let groups = list_groups(&state.services, &principal).await?;
    Ok(Json(
        groups
            .into_iter()
            .map(|group| CategoryGroupDto {
                id: group.id.inner(),
                title: group.title,
                retired_at: group.retired_at,
            })
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/category-groups",
    request_body = CategoryGroupRequest,
    responses(
        (status = 201, description = "Category group added", body = CategoryGroupDto),
        (status = 403, description = "Owner only", body = ApiError),
        (status = 422, description = "Invalid category group", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn create_category_group_route(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CategoryGroupRequest>,
) -> Result<(StatusCode, Json<CategoryGroupDto>), ApiFailure> {
    require_admin(&principal)?;
    let title = request.title.trim();
    if title.is_empty() {
        return Err(invalid_field("title", "a non-empty title", request.title));
    }
    let id = create_group(&state.services, &principal, title).await?;
    Ok((
        StatusCode::CREATED,
        Json(CategoryGroupDto {
            id: id.inner(),
            title: title.to_owned(),
            retired_at: None,
        }),
    ))
}
```

`list_groups` does not exist yet at any layer. Add it in this order, so each
layer compiles against the one below:

```rust
// crates/iaam-store/src/categories.rs — beside the insert at line 66.
pub fn list_groups(
    conn: &Connection,
    owner: Uuid,
) -> Result<Vec<CategoryGroupRow>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT id, title, retired_at FROM category_groups
         WHERE owner = ?1 ORDER BY created_at",
    )?;
    let rows = statement
        .query_map(params![owner.to_string()], |row| {
            Ok(CategoryGroupRow {
                id: parse_uuid(&row.get::<_, String>(0)?)?,
                title: row.get(1)?,
                retired_at: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
```

Follow the file's existing row-struct and uuid-parsing conventions rather than
these names if they differ; `CategoryRow` right beside it is the model.

```rust
// crates/iaam-app/src/ports.rs — in trait CategoryStore, after create_group.
    async fn list_groups(&self, owner: OwnerId) -> Result<Vec<CategoryGroupView>, AppError>;
```

```rust
// crates/iaam-app/src/ports.rs — in UnavailableCategoryStore, matching its
// neighbours: a build without a category store refuses rather than pretends.
    async fn list_groups(&self, _owner: OwnerId) -> Result<Vec<CategoryGroupView>, AppError> {
        Err(AppError::NotConfigured { what: "categories" })
    }
```

```rust
// crates/iaam-app/src/adapters/sqlite.rs — in impl CategoryStore.
    async fn list_groups(&self, owner: OwnerId) -> Result<Vec<CategoryGroupView>, AppError> {
        let rows = self
            .with_connection(move |conn| iaam_store::categories::list_groups(conn, owner.inner()))
            .await
            .map_err(category_error)?;
        Ok(rows
            .into_iter()
            .map(|row| category_group_view(row.id, row.title, row.retired_at))
            .collect())
    }
```

Use whatever connection helper the neighbouring `list_categories` uses in that
file; do not invent a second way to reach the database.

```rust
// crates/iaam-app/src/scenarios/categories.rs — beside list_categories.
pub async fn list_groups(
    services: &AppServices,
    principal: &Principal,
) -> Result<Vec<CategoryGroupView>, AppError> {
    services.categories.list_groups(principal.owner).await
}
```

- [ ] **Step 5: Register the routes**

In `crates/iaam-server/src/lib.rs`, immediately before the category routes at
line 141:

```rust
        .routes(routes!(
            routes::list_category_groups,
            routes::create_category_group_route
        ))
```

- [ ] **Step 6: Run the tests and the gate**

```
direnv exec /home/dev/repos/iaam cargo test -p iaam-server --test contract a_category_group
direnv exec /home/dev/repos/iaam make check
```

Expected: both new tests PASS, `make check` green. If an OpenAPI snapshot in
`crates/iaam-server/tests/snapshots/` disagrees, read the diff before accepting
it — a changed *report* shape would be a real failure, a new *path* is expected.

- [ ] **Step 7: Commit**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/src/routes.rs \
        crates/iaam-server/src/lib.rs crates/iaam-server/tests/contract.rs \
        crates/iaam-app/src/scenarios/categories.rs
git commit -m "feat(server): a category group can be created over HTTP (<bead-id>)"
```

---

### Task 2: An operation carries the description the source gave it

`CategoryMatcher::DescriptionContains` is implemented and tested in the core
(`crates/iaam-core/src/category.rs:68`) and can never match, because the subject
is built with `counterparty: None, description: None`
(`crates/iaam-app/src/scenarios/categories.rs:274`) — the event has no such
field. August needs it: the export puts 74 rows under the single source category
`Переводы`, and those rows are money to the children, money to the spouse, a
utility payment and a transfer to the owner's own account at another bank. Only
the description separates them.

**Files:**
- Modify: `crates/iaam-core/src/event/provenance.rs:52` — add
  `description: Option<String>` and its builder and accessor
- Modify: `crates/iaam-core/src/event/mod.rs:168-185` — schema version to 10,
  and document version 9, which the tax event added without a changelog line
- Modify: `crates/iaam-ingest/src/operation.rs:115` — `SubmittedOperation.
  description`, and `crates/iaam-ingest/src/operation.rs:208` — carry it into
  provenance
- Modify: `crates/iaam-server/src/dto.rs` — `OperationDto.description`
- Modify: `crates/iaam-app/src/scenarios/categories.rs:274` — populate the
  subject
- Test: `crates/iaam-core/src/event/provenance.rs` (unit),
  `crates/iaam-server/tests/contract.rs` (end to end)

**Interfaces:**
- Consumes: `Provenance::new(source, raw_hash, parser_version)` and its
  `with_*` builders (`crates/iaam-core/src/event/provenance.rs:78`).
- Produces: `Provenance::with_description(impl Into<String>) -> Self` and
  `Provenance::description(&self) -> Option<&str>`; `OperationDto.description:
  Option<String>`; `SubmittedOperation.description: Option<String>`. Task 4's
  skill sends `description`; Task 7's description rules match on it.

**Acceptance Criteria:**
- An operation submitted with `"description": "Corner Shop"` stores it, and a
  `description_contains` category rule with the text `corner shop` decomposes that
  row (case-insensitive, as `category.rs:78` already specifies).
- The description does **not** enter the deduplication fingerprint: two
  submissions differing only in description are still the same fact.
- Events already in the journal, which have no such field, still deserialise.
- `SCHEMA_VERSION` is 10 and the doc comment explains versions 9 and 10.

- [ ] **Step 1: Write the failing core test**

In the `tests` module of `crates/iaam-core/src/event/provenance.rs`:

```rust
#[test]
fn a_description_is_kept_and_read_back() {
    let provenance = Provenance::new(
        SourceId::new_random(),
        hash("a"),
        ParserVersion("test".to_owned()),
    )
    .with_description("Corner Shop");

    assert_eq!(provenance.description(), Some("Corner Shop"));
}

#[test]
fn provenance_recorded_before_the_description_existed_still_reads() {
    // The journal is append-only: a fact written under an older schema must
    // stay readable, or the field cannot be added at all.
    let stored = r#"{"source":"00000000-0000-0000-0000-000000000000",
        "raw_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "parser_version":"test"}"#;
    let provenance: Provenance = serde_json::from_str(stored).expect("older provenance");

    assert_eq!(provenance.description(), None);
}
```

- [ ] **Step 2: Run it and watch it fail**

```
direnv exec /home/dev/repos/iaam cargo test -p iaam-core --lib provenance
```

Expected: FAIL — `no method named with_description`.

- [ ] **Step 3: Add the field**

In `crates/iaam-core/src/event/provenance.rs`, inside `struct Provenance`, after
`source_category`:

```rust
    /// The description or counterparty the source printed on the row.
    ///
    /// Evidence about what the source said, exactly like `source_category`
    /// beside it, and never rewritten. It is what a description rule matches
    /// when the source's own category is too coarse to separate two different
    /// meanings — a bank filing both a transfer to one's own account and a
    /// utility payment under one word.
    ///
    /// `#[serde(default)]` is required: the journal is append-only and events
    /// already recorded do not carry this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
```

Initialise it to `None` in `Provenance::new`, and add:

```rust
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
```

- [ ] **Step 4: Bump the schema version and fix the changelog**

In `crates/iaam-core/src/event/mod.rs`, extend the doc comment above
`SCHEMA_VERSION` and raise the constant. Version 9 shipped without a line of its
own in commit `86c1b3f`; add it while here rather than leaving the changelog one
version behind.

```rust
/// Version 9 adds the variant [`EventKind::Tax`]: a self-paid tax is a fact of
/// its own rather than an unnamed outflow.
/// Version 10 adds the optional source description inside [`Provenance`]. It
/// defaults to absent, so facts already in the journal stay readable, while
/// the number still distinguishes software that understands the new field.
pub const SCHEMA_VERSION: u32 = 10;
```

- [ ] **Step 5: Carry it through ingest**

In `crates/iaam-ingest/src/operation.rs`, in `struct SubmittedOperation` beside
`source_category`:

```rust
    /// Description or counterparty printed by the source, retained verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
```

and in the provenance block at line 208, after the `source_category` arm:

```rust
                let base = match operation.source_category.as_deref() {
                    Some(category) => base.with_source_category(category),
                    None => base,
                };
                match operation.description.as_deref() {
                    Some(description) => base.with_description(description),
                    None => base,
                }
```

**Do not touch `canonical_form`** in `crates/iaam-ingest/src/dedup.rs:289`. The
canonical form is `{v, account, kind, dates}` on purpose: it describes what the
fact *is*. A description is what the source *called* it, and folding it in would
make one purchase, re-exported with a tidied merchant name, look like two.

Every construction site of `SubmittedOperation` in tests must gain
`description: None`. Compile errors will list them; do not add `..Default::
default()` to hide them.

- [ ] **Step 6: Accept it over HTTP**

In `crates/iaam-server/src/dto.rs`, in `OperationDto` beside `source_category`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
```

and in `OperationDto::to_domain`, beside `source_category`:

```rust
            description: self.description.clone(),
```

- [ ] **Step 7: Populate the subject**

In `crates/iaam-app/src/scenarios/categories.rs:274`:

```rust
        let subject = CategorySubject {
            row_key: event.provenance.source_operation_id(),
            source_category: event.provenance.source_category(),
            // The two were hard-wired to None, which made every
            // DescriptionContains rule dead on arrival. The source states one
            // string; it fills both roles until a source that separates them
            // arrives.
            counterparty: event.provenance.description(),
            description: event.provenance.description(),
            on: event.order.date(),
        };
```

- [ ] **Step 8: Write the end-to-end test**

In `crates/iaam-server/tests/contract.rs`:

```rust
#[tokio::test]
async fn a_description_rule_decomposes_a_row_the_source_category_cannot_separate() {
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");
    let (_, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Groceries"}),
        ),
    )
    .await;
    let category_id = category["id"].as_str().expect("category id").to_owned();

    let account = create_account(&harness, "Card").await;
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "test",
                "source": {"account": account, "channel": "paste"},
                "operations": [{
                    "account": account,
                    "type": "withdrawal",
                    "amount": "123.45",
                    "currency": "RUB",
                    "dates": {"cash_posted": "2026-08-31"},
                    "idempotency_key": "row-1",
                    "source_category": "Супермаркеты",
                    "description": "Corner Shop"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    // Not "accepted": an account with no independent confirmation yields
    // "provisional", and that is this system working as designed. This test is
    // about the description rule, so it pins only that the row was taken.
    assert_ne!(verdicts[0]["verdict"], "rejected", "{verdicts}");

    // Case-insensitive substring, per category.rs:78.
    let (status, impact) = call(
        &harness.router,
        post(
            "/v1/category-rules/preview",
            &harness.owner_token,
            &json!({
                "matcher": {"kind": "description_contains", "value": {"text": "corner shop"}},
                "category": category_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["rows"], 1, "{impact}");
}
```

Reuse whatever account-creation helper `contract.rs` already has instead of
`create_account` if the name differs; grep the file for `"/v1/accounts"`.

- [ ] **Step 9: Run everything**

```
direnv exec /home/dev/repos/iaam cargo test -p iaam-core --lib provenance
direnv exec /home/dev/repos/iaam cargo test -p iaam-server --test contract a_description_rule
direnv exec /home/dev/repos/iaam make check
```

Expected: all PASS. Adding a variant or a field to core has broken seven
exhaustive matches before — `make check` is what finds them, not `cargo check`.

- [ ] **Step 10: Commit**

```bash
git add crates/iaam-core crates/iaam-ingest crates/iaam-server crates/iaam-app
git commit -m "feat(core): an operation carries the description its source printed (<bead-id>)"
```

---

### Task 3: A row key falls back to the key the client supplied

The `Row` matcher — the owner's hand-made decision about one specific row, and
the strongest precedence level of money-flow §3 — keys off
`subject.row_key`, populated from `event.provenance.source_operation_id()`
(`crates/iaam-app/src/scenarios/categories.rs:275`). A source that states no
identifier of its own leaves that field absent by design and carries its
identity in `idempotency_key` instead. The T-Bank export is such a source: its
17 columns contain no operation identifier. So for exactly the rows this epic is
about, the strongest rule level cannot be used at all.

**Files:**
- Modify: `crates/iaam-app/src/scenarios/categories.rs:274`
- Test: `crates/iaam-server/tests/contract.rs`

**Interfaces:**
- Consumes: `Event.idempotency_key: Option<String>`
  (`crates/iaam-core/src/event/mod.rs:165`), `Provenance::
  source_operation_id()`.
- Produces: no new signature. The behaviour changes: `CategorySubject.row_key`
  is `source_operation_id` when the source named one, otherwise the client's
  `idempotency_key`.

**Acceptance Criteria:**
- A row submitted with only an `idempotency_key` can be pinned by a `row`
  category rule using that key.
- A row that has a `source_operation_id` still matches on it — the fallback
  does not displace the source's own identifier.
- The provenance is unchanged: it still records that the source named nothing.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_row_rule_pins_a_row_whose_source_named_no_identifier() {
    let (harness, path) = harness_on_disk();
    let mut store = SqliteStore::open(&path).expect("second connection");
    let group = store
        .insert_category_group(harness.owner, "Usual Expenses")
        .expect("category group");
    let (_, category) = call(
        &harness.router,
        post(
            "/v1/categories",
            &harness.owner_token,
            &json!({"group": group, "title": "Gifts"}),
        ),
    )
    .await;
    let category_id = category["id"].as_str().expect("category id").to_owned();

    let account = create_account(&harness, "Card").await;
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &json!({
                "source_label": "test",
                "source": {"account": account, "channel": "paste"},
                "operations": [{
                    "account": account,
                    "type": "withdrawal",
                    "amount": "999.00",
                    "currency": "RUB",
                    "dates": {"cash_posted": "2026-08-28"},
                    "idempotency_key": "tbank/paste/deadbeef/1"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");

    let (status, impact) = call(
        &harness.router,
        post(
            "/v1/category-rules/preview",
            &harness.owner_token,
            &json!({
                "matcher": {"kind": "row", "value": {"key": "tbank/paste/deadbeef/1"}},
                "category": category_id,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["rows"], 1, "{impact}");
}
```

- [ ] **Step 2: Run it and watch it fail**

```
direnv exec /home/dev/repos/iaam cargo test -p iaam-server --test contract a_row_rule_pins
```

Expected: FAIL — `rows` is `0`, because `row_key` is `None`.

- [ ] **Step 3: Implement the fallback**

In `crates/iaam-app/src/scenarios/categories.rs`, in the `assignment` method:

```rust
        // The source's own identifier when it stated one; otherwise the key the
        // client supplied. Without the fallback, a source that names no
        // identifier — a card statement, typically — cannot be corrected row by
        // row at all, and money-flow §3's strongest precedence level is dead
        // for exactly the imports that need it most. Provenance is untouched:
        // it still records that the source named nothing.
        let row_key = event
            .provenance
            .source_operation_id()
            .or(event.idempotency_key.as_deref());
        let subject = CategorySubject {
            row_key,
            source_category: event.provenance.source_category(),
            counterparty: event.provenance.description(),
            description: event.provenance.description(),
            on: event.order.date(),
        };
```

- [ ] **Step 4: Run the tests**

```
direnv exec /home/dev/repos/iaam cargo test -p iaam-server --test contract a_row_rule_pins
direnv exec /home/dev/repos/iaam make check
```

Expected: PASS, gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/iaam-app crates/iaam-server/tests/contract.rs
git commit -m "fix(app): a row rule reaches rows whose source named no identifier (<bead-id>)"
```

---

### Task 4: The T-Bank import skill, proven on a synthetic export

The skill is where the institution lives. It must run end to end against a
**synthetic** export before it ever sees the owner's file, so that a failure is
a bug in the skill rather than a mess in the journal.

**Files:**
- Create: `.claude/skills/tbank-csv-import/SKILL.md`
- Create: `.claude/skills/tbank-csv-import/import.py`
- Create: `.claude/skills/tbank-csv-import/fixtures/synthetic-export.csv`
- Create: `.claude/skills/tbank-csv-import/fixtures/expected-summary.json`

Not under `tests/fixtures/`: `scripts/check-fixtures.sh` requires every file
there to be both in `MANIFEST.sha256` and named by a `.rs` test, and this
fixture is read by neither.

**Interfaces:**
- Consumes: `GET /v1/openapi.json` (unauthenticated), `GET /v1/accounts`,
  `POST /v1/ingest/operations` with `description` from Task 2.
- Produces: `import.py --export <file> --base-url <url> --token-env <var>
  --account-map <file> --channel file --dry-run|--submit`, printing a summary
  of `{submitted, deduplicated, skipped_outside_contour, dropped_second_leg,
  rejected}`.

**Acceptance Criteria:**
- The synthetic export produces exactly the counts in
  `expected-summary.json` on `--dry-run`.
- A transfer pair between two in-contour accounts becomes **one** `transfer`
  operation, never two.
- A transfer pair whose counterparty is outside the contour becomes one
  `withdrawal` (or `deposit`) on the in-contour side, and the out-of-contour
  leg is not submitted.
- Rows belonging entirely to out-of-contour accounts are counted and reported,
  never silently dropped.
- `idempotency_key` matches the rule of spec §3 exactly, including the account
  and channel prefix.
- `source_category` and `description` are sent verbatim.
- No account name, UUID, amount or counterparty from the owner's data appears in
  any file created by this task.

- [ ] **Step 1: Write the synthetic export**

`.claude/skills/tbank-csv-import/fixtures/synthetic-export.csv` — same 17
columns and separator as the real export, with invented values. Column names are
data from the source and stay in Russian; the amounts, names and merchants are
made up.

```csv
"Имя счёта";"Номер карты";"Дата операции";"Сумма операции";"Валюта операции";"Сумма в валюте счёта";"Валюта счёта";"Статус";"Категория по-умолчанию";"Ваша категория";"MCC";"Описание";"Сообщение";"Округление";"Сумма операции с округлением";"Бонусы (включая кэшбэк)";"Учёт в аналитике"
"Main";"*1111";"05.08.2026 10:00:00";"-100,00";"RUB";"-100,00";"RUB";"Ок";"Супермаркеты";"";"5411";"Shop One";"";"0,00";"-100,00";"1,00";"Да"
"Main";"*1111";"05.08.2026 11:00:00";"-100,00";"RUB";"-100,00";"RUB";"Ок";"Супермаркеты";"";"5411";"Shop One";"";"0,00";"-100,00";"1,00";"Да"
"Main";"*1111";"06.08.2026 09:00:00";"-5000,00";"RUB";"-5000,00";"RUB";"Ок";"Переводы";"";"";"Между своими счетами";"";"0,00";"-5000,00";"0,00";"Нет"
"Savings";"*2222";"06.08.2026 09:00:01";"5000,00";"RUB";"5000,00";"RUB";"Ок";"Переводы";"";"";"Между своими счетами";"";"0,00";"5000,00";"0,00";"Нет"
"Main";"*1111";"07.08.2026 09:00:00";"-700,00";"RUB";"-700,00";"RUB";"Ок";"Переводы";"";"";"Между своими счетами";"";"0,00";"-700,00";"0,00";"Нет"
"Outside";"*3333";"07.08.2026 09:00:01";"700,00";"RUB";"700,00";"RUB";"Ок";"Переводы";"";"";"Между своими счетами";"";"0,00";"700,00";"0,00";"Нет"
"Outside";"*3333";"08.08.2026 12:00:00";"-250,00";"RUB";"-250,00";"RUB";"Ок";"Супермаркеты";"";"5411";"Shop Two";"";"0,00";"-250,00";"0,00";"Да"
"Main";"*1111";"09.08.2026 08:00:00";"12000,00";"RUB";"12000,00";"RUB";"Ок";"Пополнения";"";"";"Someone Outside";"";"0,00";"12000,00";"0,00";"Да"
```

Two things in this fixture are deliberate, and removing either makes it easier
than reality:

- **The two identical `Shop One` rows on one day.** The store refuses to treat
  them as one fact, and they are what the ordinal in the row key exists for.
- **Each transfer pair is one second apart.** The real export posts the two legs
  separately, and matching on the exact timestamp finds 1 pair out of 53. A
  fixture with equal timestamps would pass while the importer was broken.

- [ ] **Step 2: Write the expected summary — the test's assertion**

`.claude/skills/tbank-csv-import/fixtures/expected-summary.json`:

```json
{
  "submitted": 5,
  "dropped_second_leg": 1,
  "skipped_outside_contour": 2,
  "rejected": 0,
  "operations": [
    {"kind": "withdrawal", "amount": "100.00", "ordinal": 1},
    {"kind": "withdrawal", "amount": "100.00", "ordinal": 2},
    {"kind": "transfer",   "amount": "5000.00"},
    {"kind": "withdrawal", "amount": "700.00"},
    {"kind": "deposit",    "amount": "12000.00"}
  ]
}
```

Read it against the fixture before writing code: `Main`/`Savings` are in the
contour and `Outside` is not, so the `Main→Savings` pair collapses to one
`transfer` (one leg dropped), the `Main→Outside` pair yields a `withdrawal` on
`Main` while the `Outside` leg is skipped, and `Outside`'s own purchase is
skipped. That is 2 skipped and 1 dropped.

- [ ] **Step 3: Write the importer**

`.claude/skills/tbank-csv-import/import.py`, standard library only. The parts
that matter, in full:

```python
"""Submit a T-Bank operations export to iaam.

Knows the bank's export and nothing about any owner: accounts arrive by name
through --account-map and are resolved against GET /v1/accounts at run time.
"""

import argparse, csv, hashlib, json, os, sys, urllib.request
from collections import defaultdict
from datetime import datetime

CHANNEL_DEFAULT = "file"


def amount_of(row):
    """The account-currency amount, as a Decimal-shaped string."""
    return row["Сумма в валюте счёта"].replace("\xa0", "").replace(" ", "").replace(",", ".")


def date_of(row):
    """The export prints 'DD.MM.YYYY HH:MM:SS'; the API wants ISO."""
    return datetime.strptime(row["Дата операции"], "%d.%m.%Y %H:%M:%S")


def row_key(account_id, channel, raw_line, ordinal):
    """Spec §3. The account and channel are in the key because the store
    matches idempotency_key globally per owner, so two institutions producing
    an identical line would otherwise collide and the second would vanish."""
    digest = hashlib.sha256(raw_line.encode("utf-8")).hexdigest()
    return f"{account_id}/{channel}/{digest}/{ordinal}"


def is_internal_transfer(row):
    return row["Описание"].strip() == "Между своими счетами"


PAIR_TOLERANCE_SECONDS = 5


def pair_legs(rows):
    """Both legs of an internal transfer are in the export; submitting both
    would double-count the movement.

    The two legs do NOT share a timestamp — they differ by about a second,
    because the bank posts them separately. Pairing on the exact time was tried
    against the real August export and matched 1 pair out of 53. Match instead
    on the equal absolute amount and the nearest time within a tight tolerance:
    27 of 27 pairs, and no false pair, because a wider window would start
    joining two genuinely separate transfers of the same round amount — August
    has nine of 10 000 alone."""
    pairs, singles = [], []
    legs, others = [], []
    for row in rows:
        (legs if is_internal_transfer(row) else others).append(row)

    outgoing = sorted([r for r in legs if float(amount_of(r)) < 0], key=date_of)
    incoming = sorted([r for r in legs if float(amount_of(r)) > 0], key=date_of)
    taken = set()
    for out_row in outgoing:
        best, best_gap = None, None
        for index, in_row in enumerate(incoming):
            if index in taken:
                continue
            if abs(float(amount_of(in_row))) != abs(float(amount_of(out_row))):
                continue
            gap = abs((date_of(in_row) - date_of(out_row)).total_seconds())
            if gap <= PAIR_TOLERANCE_SECONDS and (best_gap is None or gap < best_gap):
                best, best_gap = index, gap
        if best is None:
            # Reported by the caller, never guessed at: a transfer inferred
            # from one side is an invention.
            singles.append(out_row)
        else:
            taken.add(best)
            pairs.append((out_row, incoming[best]))

    singles.extend(row for index, row in enumerate(incoming) if index not in taken)
    singles.extend(others)
    return pairs, singles
```

The rest, in full:

```python
def get(base_url, path, token=None):
    request = urllib.request.Request(base_url.rstrip("/") + path)
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request) as response:
        return json.load(response)


def post(base_url, path, token, payload):
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(base_url.rstrip("/") + path, data=body)
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request) as response:
        return json.load(response)


def resolve_accounts(base_url, token, account_map):
    """Account names come from --account-map; identifiers come from the live
    system. Nothing about the owner is stored in this file."""
    by_title = {a["title"]: a["id"] for a in get(base_url, "/v1/accounts", token)}
    missing = [name for name in account_map.values() if name not in by_title]
    if missing:
        raise SystemExit(f"no such account in the system: {', '.join(sorted(missing))}")
    return {export_name: by_title[title] for export_name, title in account_map.items()}


def operation_of(row, account_id, currency="rub"):
    value = amount_of(row)
    kind = "deposit" if float(value) > 0 else "withdrawal"
    return {
        "account": account_id,
        "type": kind,
        "amount": value.lstrip("-"),
        "currency": currency,
        "dates": {"cash_posted": date_of(row).date().isoformat()},
        "source_category": row["Категория по-умолчанию"],
        "description": row["Описание"],
    }


def build(rows, accounts, channel, raw_lines):
    """Returns (operations, summary). An account absent from `accounts` is
    outside the contour: its own rows are skipped and counted, and a transfer
    to it is a real movement across the boundary rather than a paired leg."""
    pairs, singles = pair_legs(rows)
    ordinals = defaultdict(int)
    operations, summary = [], {
        "submitted": 0, "dropped_second_leg": 0,
        "skipped_outside_contour": 0, "unmatched_legs": 0,
    }

    def key_for(row, account_id):
        day = date_of(row).date().isoformat()
        ordinals[(account_id, day)] += 1
        return row_key(account_id, channel, raw_lines[id(row)], ordinals[(account_id, day)])

    for out_row, in_row in pairs:
        out_id = accounts.get(out_row["Имя счёта"])
        in_id = accounts.get(in_row["Имя счёта"])
        if out_id and in_id:
            operation = operation_of(out_row, out_id)
            operation["type"] = "transfer"
            operation["to_account"] = in_id
            operation["idempotency_key"] = key_for(out_row, out_id)
            operations.append(operation)
            summary["dropped_second_leg"] += 1
        elif out_id or in_id:
            # One side only: money genuinely crossed the boundary.
            inside, outside = (out_row, in_row) if out_id else (in_row, out_row)
            account_id = out_id or in_id
            operation = operation_of(inside, account_id)
            operation["idempotency_key"] = key_for(inside, account_id)
            operations.append(operation)
            summary["skipped_outside_contour"] += 1
        else:
            summary["skipped_outside_contour"] += 2

    for row in singles:
        account_id = accounts.get(row["Имя счёта"])
        if not account_id:
            summary["skipped_outside_contour"] += 1
            continue
        if is_internal_transfer(row):
            # A leg whose partner is not in the file. Never guessed at: a
            # transfer inferred from one side is an invention.
            summary["unmatched_legs"] += 1
            print(f"unmatched transfer leg: {row['Дата операции']} {amount_of(row)}", file=sys.stderr)
            continue
        operation = operation_of(row, account_id)
        operation["idempotency_key"] = key_for(row, account_id)
        operations.append(operation)

    summary["submitted"] = len(operations)
    return operations, summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--export", required=True)
    parser.add_argument("--account-map", required=True,
                        help='JSON: {"<name in the export>": "<account title in iaam>"}')
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--token-env", default="IAAM_TOKEN")
    parser.add_argument("--channel", default=CHANNEL_DEFAULT)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    with open(args.export, encoding="utf-8-sig", newline="") as handle:
        text = handle.read()
    rows = list(csv.DictReader(text.splitlines(), delimiter=";"))
    raw_lines = {id(row): line for row, line in zip(rows, text.splitlines()[1:])}
    account_map = json.load(open(args.account_map, encoding="utf-8"))

    token = os.environ.get(args.token_env, "")
    if args.dry_run:
        # No server needed to check the arithmetic: the export's own account
        # name stands in for the identifier.
        accounts = {export_name: export_name for export_name in account_map}
    else:
        accounts = resolve_accounts(args.base_url, token, account_map)

    operations, summary = build(rows, accounts, args.channel, raw_lines)
    summary["rows_in_file"] = len(rows)
    accounted = (summary["submitted"] + summary["dropped_second_leg"]
                 + summary["skipped_outside_contour"] + summary["unmatched_legs"])
    summary["unaccounted"] = len(rows) - accounted

    if args.dry_run:
        print(json.dumps(summary, ensure_ascii=False, indent=2))
        return

    by_account = defaultdict(list)
    for operation in operations:
        by_account[operation["account"]].append(operation)
    for account_id, batch in by_account.items():
        verdicts = post(args.base_url, "/v1/ingest/operations", token, {
            "source_label": f"tbank-export {os.path.basename(args.export)}",
            "source": {"account": account_id, "channel": args.channel},
            "operations": batch,
        })
        summary.setdefault("verdicts", defaultdict(int))
        for verdict in verdicts:
            summary["verdicts"][verdict["verdict"]] += 1
            if verdict["verdict"] == "rejected":
                print(json.dumps(verdict, ensure_ascii=False), file=sys.stderr)
    print(json.dumps(summary, ensure_ascii=False, indent=2, default=dict))


if __name__ == "__main__":
    main()
```

**`unaccounted` must be zero.** It is the arithmetic that makes "the report is
missing something" answerable: every row of the file is submitted, dropped as a
paired leg, skipped as out-of-contour, or reported as an unmatched leg. A
non-zero value means the skill met a case it does not handle, and the fix is in
the skill.

The source is declared **per account**, so the loop batches by account: a single
`source` for the whole file would make one account's rows deduplicate against
another's.

- [ ] **Step 4: Run it against the fixture and compare**

```
direnv exec /home/dev/repos/iaam python3 .claude/skills/tbank-csv-import/import.py \
  --export .claude/skills/tbank-csv-import/fixtures/synthetic-export.csv \
  --account-map <(echo '{"Main": "Main", "Savings": "Savings"}') \
  --dry-run
```

Expected: a summary equal to `expected-summary.json`. If it differs, the
importer is wrong — the fixture was derived from the spec, not from the code.

- [ ] **Step 5: Write SKILL.md**

`.claude/skills/tbank-csv-import/SKILL.md` — frontmatter `name` and
`description`, then: what the export looks like, the column mapping, how to run
the importer, the row-key rule and **why** it carries the account and channel,
the leg-pairing rule, and an explicit section "What this skill must never
contain: the owner's account names, UUIDs, balances or counterparties — they
arrive through `--account-map` and `GET /v1/accounts`."

- [ ] **Step 6: Commit**

```bash
git add .claude/skills/tbank-csv-import
git commit -m "feat(skill): a T-Bank export becomes operations, proven on a synthetic file (<bead-id>)"
```

---

### Task 5: August in the journal

**Files:** none in the repository. This task writes to the owner's database
only.

**Interfaces:**
- Consumes: Task 4's importer, `POST /v1/accounts`, `POST /v1/contours`.
- Produces: accounts, one contour version, and August's operations in the
  journal. Task 6 reads them.

**Acceptance Criteria:**
- Every account of the spec's perimeter table exists, and the contour version
  lists exactly the in-contour ones.
- The import summary accounts for all 221 rows: submitted + dropped second leg
  + skipped outside contour + rejected = 221, with no unexplained remainder.
- Running the same import a second time submits the same rows and results in
  **zero** new events.
- Nothing from the export is written into the repository.

- [ ] **Step 1: Start the service and get a token**

```
IAAM_DATABASE=$HOME/iaam-owner.db direnv exec /home/dev/repos/iaam make owner-token LABEL=laptop
IAAM_DATABASE=$HOME/iaam-owner.db direnv exec /home/dev/repos/iaam make run
```

The database lives outside the repository. Keep the token in an environment
variable; never paste it into a file.

- [ ] **Step 2: Create the accounts**

One `POST /v1/accounts` per row of the perimeter table, in-contour and
out-of-contour alike — an out-of-contour account still needs an identity for the
skill's `--account-map` to name a counterparty. Record the returned ids in a
local map file outside the repository.

- [ ] **Step 3: Create the contour version**

`POST /v1/contours` with exactly the in-contour account ids. Note the returned
`contour` and `version` — the report names them. The account list itself is the
owner's data and is never written into this plan.

- [ ] **Step 4: Dry-run the import and read the summary**

```
python3 .claude/skills/tbank-csv-import/import.py \
  --export "$HOME/Operations Sat Aug 01 2026-Mon Aug 31 2026.csv" \
  --account-map "$HOME/iaam-accounts.json" --dry-run
```

Check the arithmetic against 221 before submitting anything. A row that is
neither submitted, dropped as a second leg, skipped as out-of-contour, nor
rejected means the skill has a case it does not handle — fix the skill, do not
hand-edit the data.

- [ ] **Step 5: Submit**

Re-run with `--submit`. Every verdict must be `accepted`; a `rejected` verdict
names the field, and the fix belongs in the skill.

- [ ] **Step 6: Prove idempotency**

Re-run `--submit` unchanged. Expected: every verdict reports the row as already
known and the journal grows by zero events. This is requirement I4 and it is
cheap to check; skipping it means discovering duplicates a month later.

- [ ] **Step 7: Confirm the repository is clean**

```bash
git status --porcelain
```

Expected: empty, or only `.beads/` churn. Any file containing the owner's
operations here is a defect of this task.

---

### Task 6: Balances stated, the report read, the discrepancy believed

**Files:** none in the repository.

**Interfaces:**
- Consumes: `POST /v1/reconciliation/balance`
  (`OwnerBalanceRequest { account, from, to, at, cash, positions,
  source_hash }`), `GET /v1/reports/flow?contour&from&to`,
  `GET /v1/reports/balances?contour&as_of`.
- Produces: the August figures, and a written note of the discrepancy and its
  account.

**Acceptance Criteria:**
- A closing control balance on 2026-08-31 exists for every in-contour account
  whose balance the owner can state, including the MTS account, which has a
  balance and no operations.
- `GET /v1/reports/flow` for 2026-08-01..2026-08-31 returns the six quantities,
  the internal-transfer reference block, and the discrepancy.
- The discrepancy is **reported as found**, with the account it belongs to. If
  it is not zero, the finding is recorded and investigated on that account —
  the report is not adjusted to make it zero.
- `not_decomposed` is reported with its row count and amount.

- [ ] **Step 1: State each closing balance**

```
POST /v1/reconciliation/balance
{"account": "<id>", "from": "2026-08-01", "to": "2026-08-31",
 "at": "closing", "cash": {"amount": "<stated>", "currency": "RUB"}}
```

- [ ] **Step 2: Read the flow report**

```
GET /v1/reports/flow?contour=<id>&from=2026-08-01&to=2026-08-31
```

- [ ] **Step 3: Write down what it said**

Record the six quantities, the discrepancy with its account, and the
`not_decomposed` count and amount. Expect the MTS account to carry an
undecomposed delta: its operations are not loaded, and spec §5 says that surfaces
as a named delta rather than as invented earnings or spending.

- [ ] **Step 4: Report to the owner before touching anything**

The acceptance of this epic is the discrepancy being *named*, not being zero. If
it is not zero, the next step is to look at the account the report named — not
to change the report.

---

### Task 7: The owner's categories, and the 74 rows one word hides

**Files:** none in the repository. Reference data only.

**Interfaces:**
- Consumes: `POST /v1/category-groups` (Task 1), `POST /v1/categories`,
  `POST /v1/category-rules/preview`, `POST /v1/category-rules`, the description
  from Task 2 and the row-key fallback from Task 3.
- Produces: the owner's category list and the rule set that decomposes August.

**Acceptance Criteria:**
- The owner's own two-level list exists — his list, not the bank's.
- One `source_category` rule per T-Bank category value that maps cleanly.
- `Переводы` is **not** mapped by a source-category rule. Its 74 rows are
  separated by description rules: the children's cards, the spouse's card, the
  utility company, and the owner's own account at another bank.
- Every rule carries a validity interval (R11) — none is open-ended by
  accident.
- Each rule is previewed before it is created, and the preview's row count and
  monthly movements are read (R12).
- Whatever remains undecomposed is reported as a count and an amount, and is
  discussed with the owner rather than swept into a catch-all. There is no
  "Прочее".

- [ ] **Step 1: Create the groups and categories the owner names**

Ask him for the list; do not invent one. His Actual Budget groups are the
obvious starting point, and they are his to confirm.

- [ ] **Step 2: Map the source categories that are unambiguous**

For each T-Bank value — `Супермаркеты`, `Такси`, `Фастфуд`, `Аптеки`,
`Мобильная связь`, `ЖКХ` and the rest — preview, then create:

```
POST /v1/category-rules/preview
{"matcher": {"kind": "source_category", "value": {"value": "Супермаркеты"}},
 "category": "<id>", "valid_from": "2026-08-01"}
```

- [ ] **Step 3: Separate the transfers by description**

```
POST /v1/category-rules/preview
{"matcher": {"kind": "description_contains", "value": {"text": "<counterparty>"}},
 "category": "<id>", "valid_from": "2026-08-01"}
```

One rule per meaning, previewed first. A description rule is level 3 and is
overridden by any source-category rule, so `Переводы` must be left unmapped at
level 2 for these to be reachable — that is the whole reason step 2 skips it.

- [ ] **Step 4: Re-read the flow report and compare**

The outflow decomposition should now name where August's money went, and
`not_decomposed` should be a small, named remainder. Report both numbers to the
owner with the rule versions the report used.

---

## Risks and known traps

- **Adding a field to core breaks distant code.** `EventKind` changes have
  broken seven exhaustive matches at once before. `make check` is the gate;
  `cargo check -p <crate>` builds neither test targets nor other crates'
  binaries and has let five breakages through.
- **`cargo test` takes one filter**, and the target is named with `--test` or
  `--lib`.
- **Migrations do nothing until registered** in
  `crates/iaam-store/src/schema.rs`. Task 2 changes no table, so it needs none —
  but if one is added, its `SCHEMA_VERSION` is the store's, which is not the
  event schema version of `crates/iaam-core/src/event/mod.rs:185`.
- **The description must not enter the deduplication fingerprint.**
  `crates/iaam-ingest/src/dedup.rs:289` deliberately hashes `{v, account, kind,
  dates}`.
- **`Учёт в аналитике = Нет` is not a rule for skipping a row.** In the August
  file it marks internal transfers, and also four ±1,00 authorisation pairs from
  one merchant. The skill pairs and drops legs on its own evidence; using the
  bank's analytics flag as the criterion would delete real operations the day a
  bank changes its meaning.
- **The two legs of a transfer do not share a timestamp.** They differ by about
  a second. Measured on the real August export: pairing on the exact time
  matched 1 pair of 53; nearest-time within 5 seconds matched 27 of 27. Widening
  the window is the wrong repair — August contains nine separate transfers of
  10 000, and a loose window would join two of them into one.
- **Disk fills fast**: each worktree's `target/` is roughly 20 GB. Delete merged
  trees immediately.
