# Money flow P1 — August becomes visible: implementation plan

> **For agentic workers:** each Task below is one bead and one worker. Steps use
> `- [ ]` for human readability; tracking is in beads. Do not run repo-wide
> gates or the full test suite — see Global Constraints.

**Goal:** the owner enters August through the structured route and reads one
report that says what came in from outside, what went out, what the capital
earned, what moved into assets, what went to fees and taxes, what moved between
his own accounts, and — when the identity does not close — by how much and on
which account.

**Architecture:** a new core projection, `MoneyFlow`, reads the existing journal
against the existing versioned contour and sums seven quantities by leg kind and
event kind, plus the cash delta computed the same way `Balances` computes it.
The identity is checked, not assumed, and its residual is reported. `EventKind`
gains `Tax`, modelled exactly on the existing `Fee`, so a self-paid tax stops
being indistinguishable from spending. `SourceId` gains a deterministic
constructor so a re-submitted batch replaces rows instead of duplicating them.
Two read-only routes expose the result. Nothing in the returns path is touched.

**Tech Stack:** Rust 2024, workspace of ten crates. `serde` for the journal
payload (JSON in a single SQLite column, so new event variants need no SQL
migration). `uuid` with the `v5` feature, already enabled in `iaam-core`. Tests
are `#[test]` unit tests beside the code plus integration tests under
`crates/<crate>/tests/`.

**Spec:** `.internal/specs/2026-09-01-money-flow-design.md` — read it before
starting any task. R9, R10 and the §2 identity are load-bearing; do not
"simplify" them away.

**Not in this plan:** categories and the decomposition of the outflow (plan P2),
the deterministic file importer and its column mappings (plan P3), the Actual
Budget migration, the web UI.

## Global Constraints

- **English only** in everything new: identifiers, test names, doc comments,
  inline comments, `#[error(...)]` text. Existing Russian comments are left
  alone, not retranslated. Domain terms come from `docs/glossary-ru-en.md`.
- `unsafe_code = "forbid"`, `clippy::all` denied at workspace level.
- **Validation logic never goes in a function named `new`** — `cargo-mutants`
  skips those, so the mutation gate would not see it (§15.7; see the comment at
  `crates/iaam-core/src/event/provenance.rs:17`).
- **Workers run targeted tests only.** `cargo test -p <crate> <filter>` and
  `cargo check -p <crate>`. **`cargo test` takes one filter, not two** — a
  second positional argument fails with `unexpected argument found`. Run each
  filter in its own invocation. Do **not** run `make check`, the full suite,
  `cargo-mutants`, or any formatter — the orchestrator runs those once at the
  end of the epic.
- **`cargo check -p <crate>` is mandatory before claiming a task done.** It is
  how you learn that an added `EventKind` variant broke an exhaustive match you
  did not know about.
- **No money arithmetic with `f64`.** `scripts/` enforces this in the core.
  Amounts are `PostedMinor` / `Money`; addition is `checked_add`.
- **Currencies are never silently added.** Every quantity in this plan is a map
  keyed by `CurrencyCode`. A function that collapses currencies into one number
  is a plan violation.
- Do not touch the issue tracker. Do not commit, push, or branch unless the
  task says so.

## Where the exhaustive matches are

Adding the `EventKind::Tax` variant (Task 3) fails to compile at every
exhaustive match. These are the ones present today — treat the list as a
starting point and let `cargo check -p` find the rest:

| File | Line | What it decides |
|---|---|---|
| `crates/iaam-core/src/event/kind.rs` | ~276 | `discriminant()` — the stored kind string |
| `crates/iaam-core/src/event/kind.rs` | ~300 | `flow_endpoints()` — money movement endpoints |
| `crates/iaam-core/src/event/mod.rs` | ~250 | `validate_structure()` — the event's leg shape |
| `crates/iaam-ingest/src/operation.rs` | ~290 | `build()` — operation kind to event kind |

## File Structure

| File | Responsibility |
|---|---|
| `crates/iaam-core/src/ids.rs` | gains `SourceId::declared` — deterministic source identity |
| `crates/iaam-core/src/event/kind.rs` | gains `EventKind::Tax` |
| `crates/iaam-core/src/event/mod.rs` | gains `validate_tax` |
| `crates/iaam-core/src/projection/money_flow.rs` | **new** — the seven quantities, the cash delta, the residual |
| `crates/iaam-core/src/projection/mod.rs` | re-exports the new module |
| `crates/iaam-ingest/src/operation.rs` | gains `OperationKind::Tax` |
| `crates/iaam-app/src/scenarios/reports.rs` | gains `money_flow` and `account_balances` |
| `crates/iaam-server/src/dto.rs` | gains the request and response DTOs |
| `crates/iaam-server/src/routes.rs` | gains two routes; declares the source on ingest |
| `crates/iaam-server/src/lib.rs` | registers the two routes |

---

### Task 1: A source can be declared, so a re-submitted batch does not duplicate

**Files:**
- Modify: `crates/iaam-core/src/ids.rs`
- Test: `crates/iaam-core/src/ids.rs` (unit tests in the same file, existing `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `SourceId::declared(owner: OwnerId, account: AccountId, channel: &str) -> SourceId`.

**Acceptance Criteria:**
- The same `(owner, account, channel)` triple always yields the same `SourceId`.
- Changing any one of the three yields a different `SourceId`.
- The result is a UUIDv5, so it can never collide with a `new_random()` v4 id.

- [ ] **Step 1: Write the failing test**

Append to the existing `mod tests` in `crates/iaam-core/src/ids.rs`:

```rust
    #[test]
    fn a_declared_source_is_stable_across_calls() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        assert_eq!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(owner, account, "file")
        );
    }

    #[test]
    fn a_declared_source_separates_channels_of_one_account() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        // Two channels of the same account must stay distinct source identities,
        // or a pasted row would deduplicate against an exported one instead of
        // confirming it (spec §6).
        assert_ne!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(owner, account, "paste")
        );
    }

    #[test]
    fn a_declared_source_separates_accounts_and_owners() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let other_owner = OwnerId(uuid::Uuid::from_u128(9));
        let account = AccountId(uuid::Uuid::from_u128(2));
        let other_account = AccountId(uuid::Uuid::from_u128(3));
        assert_ne!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(owner, other_account, "file")
        );
        assert_ne!(
            SourceId::declared(owner, account, "file"),
            SourceId::declared(other_owner, account, "file")
        );
    }

    #[test]
    fn a_declared_source_is_never_a_random_one() {
        let owner = OwnerId(uuid::Uuid::from_u128(1));
        let account = AccountId(uuid::Uuid::from_u128(2));
        // Version 5, not version 4: a declared source and a random one occupy
        // disjoint spaces, so they cannot be confused by accident.
        assert_eq!(
            SourceId::declared(owner, account, "file").inner().get_version_num(),
            5
        );
        assert_eq!(SourceId::new_random().inner().get_version_num(), 4);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p iaam-core ids::tests::a_declared_source`
Expected: FAIL, `no function or associated item named `declared` found`.

- [ ] **Step 3: Write the implementation**

Add to `crates/iaam-core/src/ids.rs`, after the `typed_id!` invocations:

```rust
/// Namespace for declared sources. A fixed UUID, so the derivation is stable
/// across builds and machines.
const DECLARED_SOURCE_NAMESPACE: uuid::Uuid =
    uuid::uuid!("6f2b1c4e-6f8a-5a1d-9d0e-2c7f4a3b8e11");

impl SourceId {
    /// A source identity the caller declares rather than one we mint.
    ///
    /// Minting a random source per request means nothing deduplicates across
    /// requests: re-sending a corrected batch creates a second set of rows
    /// instead of replacing the first. The identity is therefore derived from
    /// the triple that actually names the source — the owner, the account, and
    /// the channel the rows arrived through.
    ///
    /// The channel is part of the key on purpose. A file export and a page
    /// paste of the same account are two channels; collapsing them into one
    /// source would make a pasted row deduplicate against an exported one
    /// instead of confirming it, and the two could never be told apart.
    #[must_use]
    pub fn declared(owner: OwnerId, account: AccountId, channel: &str) -> Self {
        let name = format!("{}/{}/{}", owner.inner(), account.inner(), channel);
        Self(uuid::Uuid::new_v5(
            &DECLARED_SOURCE_NAMESPACE,
            name.as_bytes(),
        ))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p iaam-core ids::tests::a_declared_source`
Expected: PASS, 4 tests.

- [ ] **Step 5: Check the crate compiles**

Run: `cargo check -p iaam-core`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-core/src/ids.rs
git commit -m "feat(core): a source can be declared, so a resubmitted batch does not duplicate (iaam-5jhq)"
```

---

### Task 2: The ingest route accepts a declared source

**Files:**
- Modify: `crates/iaam-server/src/dto.rs` (`SubmitOperationsRequest`)
- Modify: `crates/iaam-server/src/routes.rs:1219` (`ingest_operations`)
- Test: `crates/iaam-server/tests/contract.rs` — append to the existing file. There is no `common` module and no second test file; the crate keeps one contract test with its helpers at the top.

**Interfaces:**
- Consumes: `SourceId::declared` from Task 1.
- Produces: `SubmitOperationsRequest { operations, source: Option<DeclaredSourceDto> }` where
  `DeclaredSourceDto { account: Uuid, channel: String }`.

**Acceptance Criteria:**
- Posting the same batch twice with the same declared source produces the same
  `SourceId` on both submissions.
- Omitting `source` keeps today's behaviour (`SourceId::new_random()`), so no
  existing caller breaks.
- A `channel` that is empty or longer than 32 characters is rejected with 422
  and a named field, not silently accepted.

- [ ] **Step 1: Write the failing test**

Append to `crates/iaam-server/tests/contract.rs`, using the helpers already at
the top of that file: `unclaimed_harness()` (returns `(Router, String)` — the
router and an owner token), `post(path, token, &body)`, `get(path, token)` and
`call(&router, request)`. Read
`the_stage_one_question_is_answered_end_to_end` (around line 1018) first: it is
the closest existing shape, and it shows how an account is created and an
operation submitted through the API.

```rust
#[tokio::test]
async fn the_same_declared_source_yields_the_same_source_id() {
    let (router, token) = unclaimed_harness().await;
    let (status, account) = call(
        &router,
        post(
            "/v1/accounts",
            &token,
            &json!({ "title": "Card" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let account = account["id"].as_str().expect("account id").to_owned();

    let body = json!({
        "source": { "account": account, "channel": "paste" },
        "operations": [{
            "account": account,
            "dates": { "trade_date": "2026-08-05" },
            "kind": { "Withdrawal": { "amount_minor": 12_300, "currency": "RUB" } }
        }]
    });

    let (first, _) = call(&router, post("/v1/ingest/operations", &token, &body)).await;
    assert_eq!(first, StatusCode::OK);
    let (second, _) = call(&router, post("/v1/ingest/operations", &token, &body)).await;
    assert_eq!(second, StatusCode::OK);

    // The second submission must land on the same source, so the store sees a
    // repeat rather than a new origin. A random source per request is what
    // makes a corrected re-submission duplicate instead of replace.
    let sources = distinct_source_ids(&router, &token).await;
    assert_eq!(sources.len(), 1, "expected one source, got {sources:?}");
}

#[tokio::test]
async fn an_empty_channel_is_rejected() {
    let (router, token) = unclaimed_harness().await;
    let (status, account) = call(
        &router,
        post("/v1/accounts", &token, &json!({ "title": "Card" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let account = account["id"].as_str().expect("account id").to_owned();

    let (status, body) = call(
        &router,
        post(
            "/v1/ingest/operations",
            &token,
            &json!({
                "source": { "account": account, "channel": "" },
                "operations": []
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["field"], "source.channel");
}
```

The exact request bodies for creating an account and submitting an operation
must match what the DTOs actually accept — read `crates/iaam-server/src/dto.rs`
and the existing test at line 1018 rather than trusting the shapes above
verbatim; they show intent, and the field names are yours to confirm.

`distinct_source_ids` does not exist yet. Add it as a small helper beside the
other helpers in `contract.rs`, reading the events the harness store holds for
the owner and collecting their `source` values into a `BTreeSet`. Follow how
`add_reconciliation_assertion` (line 201) reaches the store. One helper, not a
new abstraction.

- [ ] **Step 2: Run the test to verify it fails**

Run: `direnv exec $WORKTREE cargo test -p iaam-server --test contract the_same_declared_source` then `... an_empty_channel`
Expected: FAIL — `unknown field "source"` from the request deserializer, or a
compile error on the missing helper.

- [ ] **Step 3: Write the implementation**

In `crates/iaam-server/src/dto.rs`, add beside `SubmitOperationsRequest`:

```rust
/// The source the caller declares for this batch.
///
/// Without it the server mints a random source per request, and nothing
/// deduplicates across requests: a corrected re-submission would add a second
/// set of rows rather than replace the first (spec §6).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DeclaredSourceDto {
    /// Account the rows belong to.
    pub account: Uuid,
    /// How the rows arrived: `file`, `paste`, `manual`.
    pub channel: String,
}
```

and add the field to `SubmitOperationsRequest`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DeclaredSourceDto>,
```

In `crates/iaam-server/src/routes.rs`, replace the line
`let source = SourceId::new_random();` inside `ingest_operations` with:

```rust
    let source = match &request.source {
        Some(declared) => {
            let channel = declared.channel.trim();
            if channel.is_empty() || channel.len() > 32 {
                return Err(ApiFailure::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    ApiError {
                        code: "invalid_request".into(),
                        message: "channel must be 1..=32 characters".into(),
                        field: Some("source.channel".into()),
                        expected: Some("a short channel name such as file, paste or manual".into()),
                        actual: Some(declared.channel.clone()),
                        correlation_id: None,
                    },
                ));
            }
            SourceId::declared(principal.owner, AccountId(declared.account), channel)
        }
        // No declaration: today's behaviour, so existing callers keep working.
        None => SourceId::new_random(),
    };
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `direnv exec $WORKTREE cargo test -p iaam-server --test contract the_same_declared_source` then `... an_empty_channel`
Expected: PASS, 2 tests.

- [ ] **Step 5: Check the crate compiles**

Run: `cargo check -p iaam-server`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/src/routes.rs crates/iaam-server/tests/contract.rs
git commit -m "feat(server): the ingest route accepts a declared source (iaam-5jhq)"
```

---

### Task 3: A tax is a fact of its own, not an unnamed outflow

**Files:**
- Modify: `crates/iaam-core/src/event/kind.rs`
- Modify: `crates/iaam-core/src/event/mod.rs`
- Test: `crates/iaam-core/src/event/mod.rs` (existing `mod tests`)

**Interfaces:**
- Consumes: `LegKind::Tax` and `Leg::tax`, which already exist
  (`crates/iaam-core/src/event/leg.rs:24`, `:84`).
- Produces: `EventKind::Tax { amount: Money, origin: TaxOrigin }` with
  `pub enum TaxOrigin { WithheldAtSource, SelfPaid }`; discriminant `"tax"`;
  `flow_endpoints()` returns `FlowEndpoints::WithinAccount`.

**Acceptance Criteria:**
- A tax event with exactly one negative tax leg matching the declared amount
  validates.
- A tax event whose leg is positive is rejected with `WrongSign`.
- A tax event with zero or two tax legs is rejected with `LegCount`.
- `discriminant()` returns `"tax"`.
- `flow_endpoints()` returns `WithinAccount`, so a tax never counts as money
  leaving the contour for the returns path.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/iaam-core/src/event/mod.rs`, next to the
existing `// --- Fee ---` block:

```rust
    // --- Tax ---

    fn tax_event(amount: Money, leg: Money) -> Event {
        let account = AccountId::new_random();
        event_with_legs(
            EventKind::Tax {
                amount,
                origin: TaxOrigin::SelfPaid,
            },
            vec![Leg::tax(account, leg)],
        )
    }

    #[test]
    fn a_tax_matches_its_single_negative_leg() {
        let event = tax_event(rub(-130_000), rub(-130_000));
        assert!(event.validate_structure().is_ok());
    }

    #[test]
    fn a_positive_tax_leg_is_rejected() {
        // A tax that increases the balance is not a tax. Taking the absolute
        // value here is how a refund silently becomes a charge.
        let event = tax_event(rub(130_000), rub(130_000));
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::WrongSign { .. })
        ));
    }

    #[test]
    fn a_tax_without_a_tax_leg_is_rejected() {
        let account = AccountId::new_random();
        let event = event_with_legs(
            EventKind::Tax {
                amount: rub(-130_000),
                origin: TaxOrigin::WithheldAtSource,
            },
            vec![Leg::cash(account, rub(-130_000))],
        );
        assert!(matches!(
            event.validate_structure(),
            Err(EventValidationError::LegCount { .. })
        ));
    }

    #[test]
    fn a_tax_names_itself_and_stays_within_the_account() {
        let kind = EventKind::Tax {
            amount: rub(-1),
            origin: TaxOrigin::SelfPaid,
        };
        assert_eq!(kind.discriminant(), "tax");
        // WithinAccount, exactly like Fee: a tax is a cost borne by the
        // contour, not money crossing its boundary. Calling it an external
        // outflow would understate contributions in the returns path.
        assert_eq!(kind.flow_endpoints(), FlowEndpoints::WithinAccount);
    }
```

`event_with_legs`, `rub` and the imports for `Leg`, `EventKind`,
`EventValidationError` and `FlowEndpoints` already exist in that test module —
reuse them; if `event_with_legs` is named differently there, use the existing
helper rather than adding another.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p iaam-core event::tests::a_tax`
Expected: FAIL, `no variant named `Tax` found for enum `EventKind``.

- [ ] **Step 3: Write the implementation**

In `crates/iaam-core/src/event/kind.rs`, add beside `FeeOrigin`:

```rust
/// Where a tax came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaxOrigin {
    /// Withheld by the payer before the money arrived.
    WithheldAtSource,
    /// Paid by the owner: property, transport, a filed return.
    SelfPaid,
}
```

add the variant to `EventKind`, next to `Fee`:

```rust
    /// Tax, whether withheld at source or paid by the owner.
    ///
    /// Modelled on `Fee`: a cost borne by the contour rather than money
    /// crossing its boundary. Without a fact of its own, a self-paid tax is
    /// indistinguishable from ordinary spending, and discretionary spending is
    /// overstated by exactly the tax bill (spec §2).
    Tax { amount: Money, origin: TaxOrigin },
```

add to `discriminant()`:

```rust
            Self::Tax { .. } => "tax",
```

and add `Self::Tax { .. }` to the `WithinAccount` arm of `flow_endpoints()`,
beside `Self::Fee { .. }`.

In `crates/iaam-core/src/event/mod.rs`, add the dispatch arm beside the `Fee`
one:

```rust
            EventKind::Tax { amount, .. } => self.validate_tax(name, *amount),
```

and the validator beside `validate_fee`:

```rust
    /// Tax: exactly one negative tax leg, equal to the declared amount.
    ///
    /// Deliberately a separate function from `validate_fee` rather than a
    /// shared one parameterised by leg kind: the two shapes are equal today by
    /// coincidence, and a shared body would silently impose one's future
    /// conditions on the other.
    fn validate_tax(
        &self,
        name: &'static str,
        declared: Money,
    ) -> Result<(), EventValidationError> {
        let tax_legs = self.legs_of_kind(LegKind::Tax);
        let money = single_leg_money(name, &tax_legs, "exactly one tax leg")?;
        if money.amount().raw() >= 0 {
            return Err(EventValidationError::WrongSign {
                kind: name,
                amount: money.amount().raw(),
                currency: money.currency(),
            });
        }
        require_equal(name, money, declared)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p iaam-core event::tests::a_tax`
Expected: PASS, 4 tests.

- [ ] **Step 5: Find every exhaustive match the variant broke**

Run: `cargo check -p iaam-core && cargo check -p iaam-ingest && cargo check -p iaam-app && cargo check -p iaam-server && cargo check -p iaam-store`
Expected: errors listing each non-exhaustive match. Handle each one explicitly —
never with a `_` arm. A tax is `WithinAccount` for flows, contributes its leg to
balances automatically, and is not a trade, an income or a fee.

- [ ] **Step 6: Run the core event tests**

Run: `cargo test -p iaam-core event::`
Expected: PASS, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/iaam-core/src/event/
git commit -m "feat(core): a tax is a fact of its own, not an unnamed outflow"
```

---

### Task 4: A tax can be submitted as an operation

**Files:**
- Modify: `crates/iaam-ingest/src/operation.rs`
- Test: `crates/iaam-ingest/src/operation.rs` (existing `mod tests`)

**Interfaces:**
- Consumes: `EventKind::Tax` and `TaxOrigin` from Task 3.
- Produces: `OperationKind::Tax { amount_minor: i64, currency: CurrencyCode, origin: TaxOrigin }`,
  built into `EventKind::Tax` with a single `Leg::tax` carrying the negated amount.

**Acceptance Criteria:**
- A submitted tax of `130_000` minor units produces an event whose tax leg is
  `-130_000` and whose declared amount matches.
- A submitted tax of `0` or a negative amount is rejected with a named field,
  consistent with how `Fee` and `Withdrawal` already behave.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/iaam-ingest/src/operation.rs`:

```rust
    #[test]
    fn a_submitted_tax_becomes_one_negative_tax_leg() {
        let account = AccountId::new_random();
        let operation = SubmittedOperation {
            account,
            kind: OperationKind::Tax {
                amount_minor: 130_000,
                currency: CurrencyCode::Rub,
                origin: TaxOrigin::SelfPaid,
            },
            ..sample_operation(account)
        };
        let (kind, legs) = build(&operation, &operation.kind).expect("tax builds");
        assert!(matches!(kind, EventKind::Tax { .. }));
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].kind, LegKind::Tax);
        assert_eq!(legs[0].money.expect("money").amount().raw(), -130_000);
    }

    #[test]
    fn a_non_positive_tax_is_rejected() {
        // The client sends a magnitude; the sign is ours to set. A client that
        // sends -130_000 believing it helps must be told, not silently obeyed.
        let account = AccountId::new_random();
        for amount in [0_i64, -1] {
            let operation = SubmittedOperation {
                account,
                kind: OperationKind::Tax {
                    amount_minor: amount,
                    currency: CurrencyCode::Rub,
                    origin: TaxOrigin::SelfPaid,
                },
                ..sample_operation(account)
            };
            let rejection = build(&operation, &operation.kind).expect_err("rejected");
            assert_eq!(rejection.field, "amount");
        }
    }
```

Use the module's existing helper for building a `SubmittedOperation` skeleton
instead of `sample_operation` if it is named differently there.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p iaam-ingest operation::tests::a_submitted_tax`
Expected: FAIL, `no variant named `Tax``.

- [ ] **Step 3: Write the implementation**

Add the variant to `OperationKind` in `crates/iaam-ingest/src/operation.rs`,
beside `Fee`:

```rust
    Tax {
        amount_minor: i64,
        currency: CurrencyCode,
        origin: TaxOrigin,
    },
```

and the arm to `build`, beside the `Fee` arm:

```rust
        OperationKind::Tax {
            amount_minor,
            currency,
            origin,
        } => {
            let amount = money(-positive(*amount_minor, "amount", *currency)?, *currency);
            Ok((
                EventKind::Tax {
                    amount,
                    origin: *origin,
                },
                vec![Leg::tax(account, amount)],
            ))
        }
```

Import `TaxOrigin` from `iaam_core::event::kind` at the top of the file.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p iaam-ingest operation::tests::a_submitted_tax && cargo test -p iaam-ingest operation::tests::a_non_positive_tax`
Expected: PASS, 2 tests.

- [ ] **Step 5: Check the crates compile**

Run: `cargo check -p iaam-ingest && cargo check -p iaam-server`
Expected: no errors. `OperationKind` is serialized straight into the request
DTO, so the route needs no change; confirm this by reading the DTO before
assuming it.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-ingest/src/operation.rs
git commit -m "feat(ingest): a tax can be submitted as an operation"
```

---

### Task 5: The money flow projection, and the identity it must hold

**Files:**
- Create: `crates/iaam-core/src/projection/money_flow.rs`
- Modify: `crates/iaam-core/src/projection/mod.rs` (add `pub mod money_flow;`)
- Test: `crates/iaam-core/src/projection/money_flow.rs` (unit tests in the file)

**Interfaces:**
- Consumes: `ContourDefinition`, `classify`, `FlowClass` from
  `crates/iaam-core/src/contour.rs`; `Event`, `EventKind`, `LegKind`;
  `CurrencyCode`, `Money`, `PostedMinor`.
- Produces:

```rust
pub struct MoneyFlow { /* private */ }

impl MoneyFlow {
    pub fn new() -> Self;
    pub fn apply(&mut self, event: &Event, contour: &ContourDefinition, window: DateWindow)
        -> Result<(), MoneyFlowError>;
    pub fn came_in(&self, currency: CurrencyCode) -> Money;
    pub fn went_out(&self, currency: CurrencyCode) -> Money;
    pub fn earned_by_capital(&self, currency: CurrencyCode) -> Money;
    pub fn moved_into_assets(&self, currency: CurrencyCode) -> Money;
    pub fn fees(&self, currency: CurrencyCode) -> Money;
    pub fn taxes(&self, currency: CurrencyCode) -> Money;
    pub fn internal_transfers(&self, currency: CurrencyCode) -> Money;
    pub fn cash_delta(&self, currency: CurrencyCode) -> Money;
    pub fn residual(&self, currency: CurrencyCode) -> Money;
    pub fn currencies(&self) -> impl Iterator<Item = CurrencyCode> + '_;
    /// Every account whose residual is non-zero, with the amount it owes.
    pub fn residuals_by_account(&self) -> Vec<(AccountId, Money)>;
}

pub struct DateWindow { pub from: Date, pub to: Date }
```

**Acceptance Criteria:**
- `residual` is zero for a journal built only from `CashIn`, `CashOut`,
  `CashTransfer`, `Income`, `Fee`, `Tax` and `Trade` events inside the contour.
- An internal transfer contributes to `internal_transfers` and to neither
  `came_in` nor `went_out`, and leaves `cash_delta` unchanged.
- An `Income` event contributes to `earned_by_capital` and to neither
  `came_in` nor `went_out`.
- A purchase contributes its gross cash leg to `moved_into_assets` and its fee
  leg to `fees`, with no double counting.
- Events outside `window` are ignored entirely.
- Amounts in different currencies never mix; `currencies()` lists each one seen.
- `residuals_by_account` names every account whose own cash change the six
  quantities fail to explain, and is empty when the identity closes everywhere.
  A contour-wide residual of zero built from two accounts that are wrong in
  opposite directions must still be reported.

- [ ] **Step 1: Write the failing test**

Create `crates/iaam-core/src/projection/money_flow.rs` with the test module
first (the implementation follows in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::{FeeOrigin, TaxOrigin, TradeSide};
    use crate::event::leg::Leg;
    use crate::ids::{AccountId, EventId, InstrumentId, TransferId};
    use crate::money::{CurrencyCode, PostedMinor, Quantity};
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn august() -> DateWindow {
        DateWindow {
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        }
    }

    /// Builds an event dated inside August with the given kind and legs. Reuse
    /// the crate's existing test constructor if one is already exported; this
    /// helper exists only so the assertions below are readable.
    fn event(kind: EventKind, legs: Vec<Leg>, on: Date) -> Event { /* see Step 3 note */ }

    #[test]
    fn an_internal_transfer_is_neither_income_nor_expense() {
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![card, deposit],
        );
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashTransfer {
                    transfer_id: TransferId::new_random(),
                    from: card,
                    to: deposit,
                    amount: rub(480_000),
                },
                vec![Leg::cash(card, rub(-480_000)), Leg::cash(deposit, rub(480_000))],
                date!(2026 - 08 - 10),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.went_out(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.internal_transfers(CurrencyCode::Rub), rub(480_000));
        // Both halves are inside the contour, so the contour's cash is unchanged.
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn a_coupon_is_earned_by_the_capital_and_not_an_inflow() {
        let broker = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Income {
                    instrument: Some(InstrumentId::new_random()),
                    gross: rub(31_000),
                    kind: None,
                },
                vec![Leg::cash(broker, rub(31_000))],
                date!(2026 - 08 - 15),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        // The distinction Snowball Income loses: a coupon is an earning, not a
        // contribution. It must appear, and it must not appear as an inflow.
        assert_eq!(flow.earned_by_capital(CurrencyCode::Rub), rub(31_000));
        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(31_000));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn a_purchase_moves_money_into_assets_and_its_fee_is_counted_once() {
        let broker = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![broker]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Trade {
                    side: TradeSide::Buy,
                    instrument,
                    quantity: Quantity::from_i64(10),
                    gross: rub(-100_000),
                    fee: Some(rub(-350)),
                    basis_fee: None,
                    basis_fee_exact: None,
                    accrued_interest: None,
                },
                vec![
                    Leg::cash(broker, rub(-100_000)),
                    Leg::fee(broker, rub(-350)),
                ],
                date!(2026 - 08 - 20),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.moved_into_assets(CurrencyCode::Rub), rub(100_000));
        assert_eq!(flow.fees(CurrencyCode::Rub), rub(350));
        // 100_350 left the cash balance; the identity accounts for all of it.
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(-100_350));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn a_self_paid_tax_is_not_ordinary_spending() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::Tax {
                    amount: rub(-13_000),
                    origin: TaxOrigin::SelfPaid,
                },
                vec![Leg::tax(card, rub(-13_000))],
                date!(2026 - 08 - 25),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.taxes(CurrencyCode::Rub), rub(13_000));
        assert_eq!(flow.went_out(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn salary_in_and_spending_out_close_the_identity() {
        let card = AccountId::new_random();
        let outside = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        for (kind, legs, on) in [
            (
                EventKind::CashIn { amount: rub(300_000) },
                vec![Leg::cash(card, rub(300_000))],
                date!(2026 - 08 - 05),
            ),
            (
                EventKind::CashOut { amount: rub(-120_000) },
                vec![Leg::cash(card, rub(-120_000))],
                date!(2026 - 08 - 12),
            ),
        ] {
            flow.apply(&event(kind, legs, on), &contour, august())
                .expect("applies");
        }
        let _ = outside;

        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(300_000));
        assert_eq!(flow.went_out(CurrencyCode::Rub), rub(120_000));
        assert_eq!(flow.cash_delta(CurrencyCode::Rub), rub(180_000));
        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
    }

    #[test]
    fn an_event_outside_the_window_is_ignored() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashIn { amount: rub(1_000) },
                vec![Leg::cash(card, rub(1_000))],
                date!(2026 - 07 - 31),
            ),
            &contour,
            august(),
        )
        .expect("applies");
        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(0));
        assert_eq!(flow.currencies().count(), 0);
    }

    #[test]
    fn two_accounts_wrong_in_opposite_directions_are_both_named() {
        // The contour-wide residual is zero here. Reporting only that total
        // would call a doubly-broken month correct.
        let card = AccountId::new_random();
        let deposit = AccountId::new_random();
        let contour = ContourDefinition::new(
            ContourId::new_random(),
            ContourVersion(1),
            vec![card, deposit],
        );
        let mut flow = MoneyFlow::new();
        // `OpeningCash` is a starting point, not a flow, so none of the six
        // quantities claims it. Landing inside the window it is exactly what an
        // unexplained jump looks like — and here two of them cancel.
        for (account, amount) in [(card, rub(-5_000)), (deposit, rub(5_000))] {
            flow.apply(
                &event(
                    EventKind::OpeningCash { amount },
                    vec![Leg::cash(account, amount)],
                    date!(2026 - 08 - 18),
                ),
                &contour,
                august(),
            )
            .expect("applies");
        }

        assert_eq!(flow.residual(CurrencyCode::Rub), rub(0));
        let named = flow.residuals_by_account();
        assert_eq!(named.len(), 2, "both accounts must be named: {named:?}");
    }

    #[test]
    fn two_currencies_never_mix() {
        let card = AccountId::new_random();
        let contour =
            ContourDefinition::new(ContourId::new_random(), ContourVersion(1), vec![card]);
        let usd = Money::new(PostedMinor::new(50_000), CurrencyCode::Usd);
        let mut flow = MoneyFlow::new();
        flow.apply(
            &event(
                EventKind::CashIn { amount: rub(300_000) },
                vec![Leg::cash(card, rub(300_000))],
                date!(2026 - 08 - 05),
            ),
            &contour,
            august(),
        )
        .expect("applies");
        flow.apply(
            &event(
                EventKind::CashIn { amount: usd },
                vec![Leg::cash(card, usd)],
                date!(2026 - 08 - 06),
            ),
            &contour,
            august(),
        )
        .expect("applies");

        assert_eq!(flow.came_in(CurrencyCode::Rub), rub(300_000));
        assert_eq!(flow.came_in(CurrencyCode::Usd), usd);
        assert_eq!(flow.currencies().count(), 2);
    }
}
```

Read `crates/iaam-core/src/projection/flows.rs`'s test module first: it already
constructs `Event` values for exactly this kind of test. Reuse its constructor
for `event` rather than writing a second one.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p iaam-core projection::money_flow`
Expected: FAIL — the module does not compile, `MoneyFlow` is not defined.

- [ ] **Step 3: Write the implementation**

Put this above the test module in the same file:

```rust
//! The flow of money across and inside the contour (spec §2).
//!
//! This module answers a household question — what came in, what went out,
//! what the capital earned, what moved into assets — and it deliberately does
//! **not** reclassify any event to do so. `flows.rs` answers a different
//! question, about contributions and withdrawals for the returns path, and
//! `EventKind::Income` is `WithinAccount` there for a correct reason: a coupon
//! is not a new contribution of capital. Moving it would corrupt XIRR.
//!
//! So the two projections read the same journal from two angles. The quantity
//! that makes this honest is `residual`: the difference between the cash the
//! contour actually gained and the six quantities that claim to explain it. A
//! non-zero residual is reported, never absorbed.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Date;

use crate::contour::{ContourDefinition, FlowClass, classify};
use crate::event::Event;
use crate::event::kind::EventKind;
use crate::event::leg::LegKind;
use crate::ids::{AccountId, EventId};
use crate::money::{CurrencyCode, Money, PostedMinor};

/// The interval a report covers, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateWindow {
    pub from: Date,
    pub to: Date,
}

impl DateWindow {
    /// Inclusive at both ends: a report for August includes the 1st and the 31st.
    #[must_use]
    pub fn covers(&self, on: Date) -> bool {
        self.from <= on && on <= self.to
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MoneyFlowError {
    #[error("event {event:?} moves money inside the window but has no date")]
    MovementWithoutDate { event: EventId },
    #[error("overflow while summing {quantity} for event {event:?}")]
    Overflow {
        quantity: &'static str,
        event: EventId,
    },
}

/// Seven quantities and the cash they claim to explain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneyFlow {
    came_in: Ledger,
    went_out: Ledger,
    earned_by_capital: Ledger,
    moved_into_assets: Ledger,
    fees: Ledger,
    taxes: Ledger,
    internal_transfers: Ledger,
    cash_delta: Ledger,
}

/// Amounts kept per account **and** per currency.
///
/// Per currency, because currencies are never silently added. Per account,
/// because §2 requires the residual to name the account it belongs to: a
/// contour-wide zero built from one account short and another long is the
/// worst possible report — it looks correct and is wrong twice.
type Ledger = BTreeMap<(AccountId, CurrencyCode), PostedMinor>;
```

Then the body. Write `apply` so that each event contributes to **exactly one**
explanatory quantity and, independently, to `cash_delta`:

```rust
impl MoneyFlow {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one event.
    ///
    /// `cash_delta` is accumulated from the legs, the same way `Balances` does
    /// it, so the two can never drift apart. The explanatory quantities are
    /// accumulated from the event kind and the leg kind. Their disagreement is
    /// what `residual` reports.
    pub fn apply(
        &mut self,
        event: &Event,
        contour: &ContourDefinition,
        window: DateWindow,
    ) -> Result<(), MoneyFlowError> {
        if !self.event_belongs(event, contour, window)? {
            return Ok(());
        }

        for leg in &event.legs {
            if !contour.contains(leg.account) {
                continue;
            }
            let Some(money) = leg.cash_effect() else {
                continue;
            };
            add(&mut self.cash_delta, leg.account, money, "cash_delta", event.id)?;

            match (&event.kind, leg.kind) {
                (_, LegKind::Fee) => {
                    add(&mut self.fees, leg.account, negated(money), "fees", event.id)?;
                }
                (_, LegKind::Tax) => {
                    add(&mut self.taxes, leg.account, negated(money), "taxes", event.id)?;
                }
                (EventKind::Trade { .. }, LegKind::Cash) => {
                    add(
                        &mut self.moved_into_assets,
                        leg.account,
                        negated(money),
                        "moved_into_assets",
                        event.id,
                    )?;
                }
                (EventKind::Income { .. }, LegKind::Cash) => {
                    add(
                        &mut self.earned_by_capital,
                        leg.account,
                        money,
                        "earned_by_capital",
                        event.id,
                    )?;
                }
                (EventKind::CashIn { .. }, LegKind::Cash) => {
                    add(&mut self.came_in, leg.account, money, "came_in", event.id)?;
                }
                (EventKind::CashOut { .. }, LegKind::Cash) => {
                    add(&mut self.went_out, leg.account, negated(money), "went_out", event.id)?;
                }
                // A transfer's two halves cancel in `cash_delta` when both
                // accounts are inside the contour; only the incoming half is
                // counted for the reference block, so the figure reads as
                // "this much moved", not "this much moved twice".
                (EventKind::CashTransfer { .. }, LegKind::Cash) => {
                    if money.amount().raw() > 0 {
                        add(
                            &mut self.internal_transfers,
                            leg.account,
                            money,
                            "internal_transfers",
                            event.id,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
```

`event_belongs` returns `false` for an event outside the window or outside the
contour, and errors with `MovementWithoutDate` for an event that moves contour
cash but carries no effective date — the same defence `flows.rs` already
mounts. Use `event.dates.effective_date()` and `classify(contour, event)`,
treating `FlowClass::Irrelevant` as "not ours".

Add the accessors, each returning `Money` and defaulting to zero in the
requested currency, `currencies()` returning the union of all keys as a
`BTreeSet`, and:

```rust
    /// The cash the six quantities fail to explain, for one account.
    ///
    /// Zero means the report closes there. Non-zero is a defect and is shown as
    /// one: a report that quietly absorbs its residual is how
    /// `Saved <redacted>` came to mean nothing.
    #[must_use]
    fn residual_of(&self, account: AccountId, currency: CurrencyCode) -> i64 {
        let at = |ledger: &Ledger| {
            ledger
                .get(&(account, currency))
                .map_or(0, |amount| amount.raw())
        };
        let explained = at(&self.came_in) - at(&self.went_out) + at(&self.earned_by_capital)
            - at(&self.moved_into_assets)
            - at(&self.fees)
            - at(&self.taxes);
        at(&self.cash_delta) - explained
    }

    /// The contour-wide residual in a currency.
    #[must_use]
    pub fn residual(&self, currency: CurrencyCode) -> Money {
        let total: i64 = self
            .accounts()
            .map(|account| self.residual_of(account, currency))
            .sum();
        Money::new(PostedMinor::new(total), currency)
    }

    /// Every account that does not close, with what it owes.
    ///
    /// Reported separately from `residual` on purpose. Two accounts wrong in
    /// opposite directions sum to zero, and a report that showed only the total
    /// would call that success while being wrong twice.
    #[must_use]
    pub fn residuals_by_account(&self) -> Vec<(AccountId, Money)> {
        let mut rows = Vec::new();
        for account in self.accounts() {
            for currency in self.currencies() {
                let residual = self.residual_of(account, currency);
                if residual != 0 {
                    rows.push((account, Money::new(PostedMinor::new(residual), currency)));
                }
            }
        }
        rows
    }
```

`accounts()` is the union of the account keys across all eight ledgers, as a
`BTreeSet`; `currencies()` is the union of the currency keys, likewise. Each
public accessor (`came_in`, `went_out`, …) sums its ledger across accounts for
the requested currency, so the totals the report prints and the per-account
residuals come from one set of numbers rather than two.

Register the module in `crates/iaam-core/src/projection/mod.rs`:

```rust
pub mod money_flow;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p iaam-core projection::money_flow`
Expected: PASS, 7 tests.

- [ ] **Step 5: Check the crate compiles**

Run: `cargo check -p iaam-core`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-core/src/projection/
git commit -m "feat(core): the money flow projection, with the residual it refuses to hide"
```

---

### Task 6: The scenarios that load a month and a set of balances

**Files:**
- Modify: `crates/iaam-app/src/scenarios/reports.rs`
- Test: `crates/iaam-app/tests/` — add `crates/iaam-app/tests/money_flow.rs`, or
  extend the neighbouring scenario test file if one already covers reports

**Interfaces:**
- Consumes: `MoneyFlow`, `DateWindow` from Task 5; `ReturnsQuery`'s contour
  resolution pattern from the same file; `statuses` from
  `crates/iaam-app/src/scenarios/reconciliation.rs`.
- Produces:

```rust
pub struct MoneyFlowQuery {
    pub contour: ContourId,
    pub contour_version: Option<ContourVersion>,
    pub from: Date,
    pub to: Date,
}

pub struct MoneyFlowReport {
    pub contour: ContourId,
    pub version: ContourVersion,
    pub from: Date,
    pub to: Date,
    pub flow: MoneyFlow,
}

pub struct AccountBalanceRow {
    pub account: AccountId,
    pub cash: Vec<Money>,
    pub reconciliation: Vec<ReconciliationStatus>,
    pub positions: Vec<(PositionKey, Quantity)>,
}

pub async fn money_flow(services: &AppServices, principal: &Principal, query: &MoneyFlowQuery)
    -> Result<MoneyFlowReport, AppError>;

pub async fn account_balances(services: &AppServices, principal: &Principal,
    contour: ContourId, contour_version: Option<ContourVersion>, as_of: Date)
    -> Result<Vec<AccountBalanceRow>, AppError>;
```

**Acceptance Criteria:**
- `money_flow` resolves the contour version the same way `returns` does, and
  reports the version it used.
- `from` later than `to` is rejected with `AppError::Invalid` naming the field,
  the same way `reconciliation::statuses` already rejects it.
- `account_balances` returns one row per account in the contour version,
  including accounts with no movements at all — an account absent from the
  report and an account with a zero balance are different facts (§10.7).
- `account_balances` never sums cash and position value into one number.

- [ ] **Step 1: Write the failing test**

Create `crates/iaam-app/tests/money_flow.rs`. Read a neighbouring test under
`crates/iaam-app/tests/` first and reuse its in-memory service harness verbatim.

```rust
//! The money flow scenario over a real store.

#[tokio::test]
async fn a_month_of_a_card_reports_what_came_in_and_what_went_out() {
    let ctx = harness().await; // the existing helper
    let card = ctx.account("Card").await;
    let contour = ctx.contour(&[card]).await;

    ctx.submit_deposit(card, 300_000, "2026-08-05").await;
    ctx.submit_withdrawal(card, 120_000, "2026-08-12").await;
    // July must not leak into August.
    ctx.submit_withdrawal(card, 999_999, "2026-07-30").await;

    let report = money_flow(
        &ctx.services,
        &ctx.principal,
        &MoneyFlowQuery {
            contour,
            contour_version: None,
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
        },
    )
    .await
    .expect("report");

    assert_eq!(report.version, ContourVersion(1));
    assert_eq!(report.flow.came_in(CurrencyCode::Rub).amount().raw(), 300_000);
    assert_eq!(report.flow.went_out(CurrencyCode::Rub).amount().raw(), 120_000);
    assert_eq!(report.flow.residual(CurrencyCode::Rub).amount().raw(), 0);
}

#[tokio::test]
async fn a_reversed_interval_is_rejected() {
    let ctx = harness().await;
    let card = ctx.account("Card").await;
    let contour = ctx.contour(&[card]).await;
    let error = money_flow(
        &ctx.services,
        &ctx.principal,
        &MoneyFlowQuery {
            contour,
            contour_version: None,
            from: date!(2026 - 08 - 31),
            to: date!(2026 - 08 - 01),
        },
    )
    .await
    .expect_err("rejected");
    assert!(matches!(error, AppError::Invalid { .. }));
}

#[tokio::test]
async fn an_account_with_no_movements_still_appears() {
    let ctx = harness().await;
    let card = ctx.account("Card").await;
    let cash = ctx.account("Cash").await; // never touched
    let contour = ctx.contour(&[card, cash]).await;
    ctx.submit_deposit(card, 300_000, "2026-08-05").await;

    let rows = account_balances(
        &ctx.services,
        &ctx.principal,
        contour,
        None,
        date!(2026 - 08 - 31),
    )
    .await
    .expect("balances");

    // "No movements" and "zero" are different facts, and an account that
    // silently vanishes from the report is how a forgotten wallet stays
    // forgotten (§10.7).
    assert_eq!(rows.len(), 2);
    let untouched = rows.iter().find(|row| row.account == cash).expect("present");
    assert!(untouched.cash.is_empty());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p iaam-app --test money_flow`
Expected: FAIL — `money_flow` is not defined.

- [ ] **Step 3: Write the implementation**

In `crates/iaam-app/src/scenarios/reports.rs`, add the query and report types
from the Interfaces block above, then:

```rust
/// The flow of money over an interval.
///
/// The contour version is resolved exactly as `returns` resolves it, and is
/// reported back: a report that does not name the contour it used cannot be
/// compared with one printed after an account was added.
pub async fn money_flow(
    services: &AppServices,
    principal: &Principal,
    query: &MoneyFlowQuery,
) -> Result<MoneyFlowReport, AppError> {
    if query.to < query.from {
        return Err(AppError::Invalid {
            field: "period".into(),
            expected: "from no later than to".into(),
            actual: format!("{}..{}", query.from, query.to),
        });
    }
    let version = match query.contour_version {
        Some(version) => version,
        None => services
            .store
            .latest_contour_version(principal.owner, query.contour)
            .await?
            .ok_or_else(|| AppError::NotFound {
                what: "contour",
                id: query.contour.0.to_string(),
            })?,
    };
    // Loaded together with the owner: someone else's contour is not found,
    // rather than found and rejected later (§14).
    let definition = services
        .store
        .load_contour(principal.owner, query.contour, version)
        .await?
        .ok_or_else(|| AppError::NotFound {
            what: "contour_version",
            id: format!("{}/{}", query.contour.0, version.0),
        })?;

    let events = services
        .store
        .load_events_through(principal.owner, query.to)
        .await?;

    let window = DateWindow {
        from: query.from,
        to: query.to,
    };
    let mut flow = MoneyFlow::new();
    for event in &events {
        flow.apply(event, &definition, window)?;
    }

    Ok(MoneyFlowReport {
        contour: query.contour,
        version,
        from: query.from,
        to: query.to,
        flow,
    })
}
```

`account_balances` follows the same contour resolution, then builds `Balances`
by folding `load_events_through(owner, as_of)`, and calls
`reconciliation::statuses` per account for the interval `[as_of, as_of]`. Start
the result from the **contour's account list**, not from the balance map, so an
untouched account is present with an empty `cash` vector.

Add `MoneyFlowError` to `AppError` via `#[from]` beside the existing projection
errors in `crates/iaam-app/src/error.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p iaam-app --test money_flow`
Expected: PASS, 3 tests.

- [ ] **Step 5: Check the crate compiles**

Run: `cargo check -p iaam-app`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-app/src/scenarios/reports.rs crates/iaam-app/src/error.rs crates/iaam-app/tests/money_flow.rs
git commit -m "feat(app): the money flow and account balance scenarios"
```

---

### Task 7: Two routes, and a report that names its residual

**Files:**
- Modify: `crates/iaam-server/src/dto.rs`
- Modify: `crates/iaam-server/src/routes.rs`
- Modify: `crates/iaam-server/src/lib.rs` (route registration)
- Test: `crates/iaam-server/tests/contract.rs` — append to the existing file, using its `unclaimed_harness()` / `post` / `get` / `call` helpers. There is no `common` module and no second test file.

**Interfaces:**
- Consumes: `money_flow`, `account_balances`, `MoneyFlowQuery` from Task 6.
- Produces:
  - `GET /v1/reports/flow?contour&contour_version&from&to` → `MoneyFlowReportDto`
  - `GET /v1/reports/balances?contour&contour_version&as_of` → `Vec<AccountBalanceDto>`

```rust
pub struct MoneyFlowReportDto {
    pub contour: Uuid,
    pub contour_version: u32,
    pub from: Date,
    pub to: Date,
    pub currencies: Vec<MoneyFlowCurrencyDto>,
    /// Accounts whose own cash change the six quantities do not explain.
    /// Empty when the report closes everywhere.
    pub unexplained: Vec<AccountResidualDto>,
}

pub struct AccountResidualDto {
    pub account: Uuid,
    pub currency: CurrencyDto,
    pub amount: String,
}

pub struct MoneyFlowCurrencyDto {
    pub currency: CurrencyDto,
    pub came_in: String,
    pub went_out: String,
    pub earned_by_capital: String,
    pub moved_into_assets: String,
    pub fees: String,
    pub taxes: String,
    pub internal_transfers: String,
    pub cash_delta: String,
    pub residual: String,
}

pub struct AccountBalanceDto {
    pub account: Uuid,
    pub cash: Vec<MoneyDto>,
    pub reconciliation: Vec<ReconciliationStatusDto>,
    pub positions: Vec<PositionQuantityDto>,
}
```

**Acceptance Criteria:**
- `GET /v1/reports/flow` returns one entry per currency seen, each carrying all
  nine figures including `residual`.
- `unexplained` names every account that does not close, so a contour-wide zero
  built from two accounts wrong in opposite directions is still visible.
- `from` later than `to` returns 422 with `field: "period"`.
- Both routes require authentication and are registered inside `protected`.
- `AccountBalanceDto` keeps `cash` and `positions` as separate fields; no field
  in the response holds their sum.
- The OpenAPI document contains both paths.

- [ ] **Step 1: Write the failing test**

Append to `crates/iaam-server/tests/contract.rs`. The sketch below uses a
`ctx` object for brevity; translate it to the file's real helpers —
`unclaimed_harness()`, `post`, `get`, `call` — the way the other tests there do.
Note that `contract.rs` already has
`every_documented_path_answers_something_other_than_404`, which will start
covering the two new paths automatically once they are registered.

```rust
#[tokio::test]
async fn the_flow_report_names_its_residual() {
    let ctx = common::owner_server().await;
    let card = ctx.create_account("Card").await;
    let contour = ctx.create_contour(&[card]).await;
    ctx.ingest_deposit(card, 300_000, "2026-08-05").await;
    ctx.ingest_withdrawal(card, 120_000, "2026-08-12").await;

    let response = ctx
        .get_json(&format!(
            "/v1/reports/flow?contour={contour}&from=2026-08-01&to=2026-08-31"
        ))
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await;
    let rub = &body["currencies"][0];
    assert_eq!(rub["came_in"], "3000.00");
    assert_eq!(rub["went_out"], "1200.00");
    assert_eq!(rub["residual"], "0.00");
    assert_eq!(body["unexplained"].as_array().expect("array").len(), 0);
}

#[tokio::test]
async fn a_reversed_interval_is_unprocessable() {
    let ctx = common::owner_server().await;
    let card = ctx.create_account("Card").await;
    let contour = ctx.create_contour(&[card]).await;
    let response = ctx
        .get_json(&format!(
            "/v1/reports/flow?contour={contour}&from=2026-08-31&to=2026-08-01"
        ))
        .await;
    assert_eq!(response.status(), 422);
    let body: serde_json::Value = response.json().await;
    assert_eq!(body["field"], "period");
}

#[tokio::test]
async fn balances_keep_cash_and_positions_apart() {
    let ctx = common::owner_server().await;
    let card = ctx.create_account("Card").await;
    let contour = ctx.create_contour(&[card]).await;
    ctx.ingest_deposit(card, 300_000, "2026-08-05").await;

    let response = ctx
        .get_json(&format!(
            "/v1/reports/balances?contour={contour}&as_of=2026-08-31"
        ))
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await;
    let row = &body[0];
    assert!(row["cash"].is_array());
    assert!(row["positions"].is_array());
    // A single "total" would be exactly the number nobody checks.
    assert!(row.get("total").is_none());
}
```

Match the amount formatting to whatever `MoneyDto` already produces in this
crate — read it before asserting `"3000.00"`, and use its format.

- [ ] **Step 2: Run the test to verify it fails**

Run: `direnv exec $WORKTREE cargo test -p iaam-server --test contract flow_report` then `... balances`
Expected: FAIL — 404 on both paths.

- [ ] **Step 3: Write the implementation**

Add the DTOs from the Interfaces block to `crates/iaam-server/src/dto.rs`, each
with `#[derive(Debug, Clone, Serialize, ToSchema)]` and a `from_domain`
constructor, following `ReturnsReportDto`'s existing shape at
`crates/iaam-server/src/dto.rs:1847`.

Add the query structs and handlers to `crates/iaam-server/src/routes.rs`,
following the `returns_report` handler at line 1372:

```rust
/// Money flow report parameters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct MoneyFlowParams {
    pub contour: Uuid,
    pub contour_version: Option<u32>,
    /// Inclusive start, ISO-8601.
    pub from: String,
    /// Inclusive end, ISO-8601.
    pub to: String,
}

/// The flow of money over an interval.
#[utoipa::path(
    get,
    path = "/v1/reports/flow",
    params(MoneyFlowParams),
    responses(
        (status = 200, description = "Flow of money over the interval", body = MoneyFlowReportDto),
        (status = 422, description = "Invalid interval", body = ApiError)
    ),
    security(("bearer" = []))
)]
pub async fn flow_report(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<MoneyFlowParams>,
) -> Result<Json<MoneyFlowReportDto>, ApiFailure> {
    let query = MoneyFlowQuery {
        contour: ContourId(params.contour),
        contour_version: params.contour_version.map(ContourVersion),
        from: parse_date(&params.from, "from")?,
        to: parse_date(&params.to, "to")?,
    };
    let report = money_flow(&state.services, &principal, &query).await?;
    Ok(Json(MoneyFlowReportDto::from_domain(&report)))
}
```

Reuse the crate's existing date parsing helper — `parse_as_of` is right next to
`returns_report`; if it does not fit a required (non-optional) date, add
`parse_date(value, field)` beside it rather than parsing inline in the handler.

Write `balances_report` the same way, taking `contour`, `contour_version` and
`as_of`.

Register both in `crates/iaam-server/src/lib.rs`, inside `protected`, beside the
returns routes:

```rust
        .routes(routes!(routes::flow_report))
        .routes(routes!(routes::balances_report))
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `direnv exec $WORKTREE cargo test -p iaam-server --test contract flow_report` then `... balances`
Expected: PASS, 3 tests.

- [ ] **Step 5: Check the crate compiles and the spec lists both paths**

Run: `direnv exec $WORKTREE cargo check -p iaam-server && direnv exec $WORKTREE cargo test -p iaam-server --test contract every_documented_path`
Expected: no errors, and `every_documented_path_answers_something_other_than_404`
passes with the two new paths included.

`crates/iaam-server/tests/snapshots/contract__the_report_shape_is_frozen_by_a_snapshot.snap`
freezes the **returns** report shape. This task must not change it: the new
routes are additions, and a diff in that snapshot means something in the returns
DTOs moved. If it does change, stop and escalate rather than accepting the new
snapshot.

- [ ] **Step 6: Commit**

```bash
git add crates/iaam-server/src/dto.rs crates/iaam-server/src/routes.rs crates/iaam-server/src/lib.rs crates/iaam-server/tests/contract.rs
git commit -m "feat(server): the flow and balances reports"
```

---

## Closing the epic

After Task 7, the orchestrator — not the task workers — runs the repo-wide
gates once:

```bash
make check
make diff-coverage BASE=main
```

Then enter August: submit the two banks' operations through
`POST /v1/ingest/operations` with a declared source per `(account, channel)`,
record a control balance per account for 31.08 through the existing
reconciliation route, and read `GET /v1/reports/flow?from=2026-08-01&to=2026-08-31`.

**The residual is the acceptance test for the whole plan.** If it is not zero,
the report has done its job — it found something — and the next step is to look
at the account it names, not to adjust the report.
