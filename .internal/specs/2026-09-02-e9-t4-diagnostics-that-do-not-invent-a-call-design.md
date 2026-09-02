# E9.T4 — Diagnostics that do not invent a call

Bead: `iaam-3y2o` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

The reconciliation ledger and the flow report already compute more than they
say. A status that failed to reconcile answers `"outcome": "discrepant"` and
throws away the field, the two sides and the delta. A dimension that a refused
import cannot confirm is silently downgraded, and nothing names the gap or the
rows. The evidence for independence is rendered as two parser-version strings —
which is not enough to check independence, as §3 explains.

At the same time, several of the problems the system can detect have **no repair
call at all**. This task handles both halves, and it handles the second half by
refusing to invent an address.

## The rule this task exists to enforce

**A URL that might help is not an action.** Where nothing this API offers
resolves the problem, the item says so and carries no operation. An item that
points at a plausible-looking route the agent will call and get nowhere with is
worse than silence, because the agent will believe it acted.

## 1. What is already computable, and what is not

Established by reading the code. This corrects the bead in one place, and an
earlier draft of this spec in two.

**Available today and discarded before it reaches a client:**

| Fact | Where it exists | Where it is lost |
|---|---|---|
| `Discrepancy { field, claimed, observed, delta }` | `check.rs:29` | only `outcome.code()` survives |
| `NotComparable::{NoJournalCoverage, TaxFactsNotRecorded}` | `check.rs:44` | same |
| `ReconciliationException::{UnsupportedRepoEncumbrance, UnsupportedFinancingPresent}` | `check.rs:66` | same |
| The asserted claim's currency, balance point, instrument, custody, values | `ControlClaim` (`claim.rs:84`) | only `discriminant()` survives |
| Which dimensions a coverage gap taints | `tainted_dimensions` (`reconciliation/mod.rs:415`) | computed at `mod.rs:227`, used, then dropped — never stored |
| The gap: dimensions, refused count, refused rows; source and parser version from provenance | `EventKind::ImportCoverageGap` (`kind.rs:256`) | `CoverageGap` is private and **discards `refused` and `rows` at construction** (`mod.rs:385`) |
| The document identity behind an evidence claim | `SourceChannel.document` (`evidence.rs:23`) | never rendered |
| Which account an undecomposed outflow belongs to | `MoneyFlow.not_decomposed.1` is keyed `(AccountId, CurrencyCode)` | `not_decomposed()` folds the account away (`money_flow.rs:464`) |

**The lossy conversion exists twice.** `ClaimOutcomeDto` and `EvidenceDto` are
built independently at `dto.rs:2165-2187` and again at `routes.rs:1801-1819`.
Both must change. Fixing one and leaving the other is the likeliest way for this
task to ship half-done and look finished.

**Correction to the bead.** The bead says refused row identities are not
exposed. That is true of **undecomposed** rows in the flow report and false of
**refused** rows in a coverage gap: schema 8 added
`ImportCoverageGap.rows: Vec<RefusedRow>`, carrying a
`SourceRowKey { source, row: Given | Fingerprint }` and the dimensions that row
alone would have moved. Schema ≥8 rejects an empty `rows` and a count or
dimension set that disagrees with it (`event/mod.rs:824`); a schema-7 record
stays readable with an empty list (`event/mod.rs:2824`). So refused rows can be
named, and only a legacy gap cannot — which the report must say rather than
render as a gap that refused nothing.

**Only one channel writes a gap at all.** `EventKind::ImportCoverageGap` is
constructed in exactly one place, `scenarios/sync.rs:436` — the broker sync
path. A document upload that refuses rows produces per-row verdicts and no gap
event, so it taints nothing. That is a hole in **ingest**, not in this task's
reporting: filed as `iaam-hj1o`. `coverage_gap_unrepaired` speaks only for
synced channels today and must not be worded as though it covered every import.

**Genuinely not available, and not made available here:**

- **Undecomposed row identity.** `MoneyFlow` accumulates into ledgers and keeps
  no event identifiers. Naming the rows would mean carrying identities through
  the projection — a change to what a projection is for, and not this task.
- **Outstanding possible duplicates.** `Verdict::PossibleDuplicate` is
  constructed in production (`scenarios/sync.rs:170`, `scenarios/ingest.rs`) but
  it is an **import-time verdict**, not stored state. No query answers
  "duplicates still undecided", so no detector over stored state can produce
  one. Its item is built here and attached in **E9.T5**, where the verdict is in
  hand.
- **Which side of a discrepancy is wrong.** Nothing in the system knows.

## 2. A gap can exist with no status, so diagnostics take the ledger

This is the correction that shapes the whole task.

`ReconciliationStatus` values are produced only from `ControlAssertion` groups
(`mod.rs:262-272`). A coverage gap creates no group and therefore no status, and
that is not a theoretical case: `sync.rs` writes the gap at line 209 and can
then return at line 244 (an out-of-interval trade) or line 260 (the portfolio
describes another day) **before any assertion is recorded**. Such a gap is
stored, real, and invisible to anything that reads statuses.

Two consequences, both binding:

- The taints do **not** live only on `ReconciliationStatus`. `ReconciliationLedger`
  gains `gaps(&self) -> &[Taint]` holding every effective gap, whether or not a
  group correlated with it. `ReconciliationStatus` additionally gains the subset
  that tainted it, because a reader holding one status must not have to
  re-correlate.
- The diagnostic function takes `&ReconciliationLedger`, not `&[ReconciliationStatus]`.
  A signature over statuses would compile, pass its tests, and silently miss the
  case the acceptance criteria are about.

`Taint` is the public projection of the private `CoverageGap`: account, period,
source, parser version, tainted dimensions, refused count, refused rows.
`CoverageGap` itself stops discarding `refused` and `rows` at `mod.rs:404`.

## 3. Independence needs the document, not the source

`SourceChannel::is_independent_of` (`evidence.rs:58`) is
`parser_version != other.parser_version && document != other.document`, and its
documentation states that the **source identifier is deliberately not part of
the criterion**: two sources may share parsing code, and a common parser bug
corrupts both sides however many identifiers they carry.

So the honest transport fix is to render the **document** alongside the parser
version for the confirming and confirmed channels. An earlier draft of this spec
proposed adding `source`; that would have added the one coordinate the rule
ignores while still omitting the one it needs, and the test written for it would
have proved only that the DTO can print two different strings.

`SourceChannel.document` is `Option<RawHash>`. Two channels that both carry
`None` compare equal and are therefore **not** independent — an absent document
is not a distinct document. The DTO renders the absence as absence, and a test
covers the both-absent pair, because that is the case where a reader would most
easily assume independence.

## 4. Two new words, and no third

T2 settled that the agent is given **prose**, not a typed reason. That holds
here, and it is what keeps this task from growing a taxonomy of blockage.

`ActionState` gains one variant:

- **`Blocked`** — *no operation in this API is available for this item.* That is
  the definition, deliberately narrow. It is not "a needed outcome": an
  informational item needs no outcome and is still blocked in this sense.

`ActionCategory` gains one variant:

- **`Informational`** — nothing is required of anyone; this is a fact the agent
  should carry into its answer to the owner.

The two compose, and the composition is the whole vocabulary:

| category | state | means |
|---|---|---|
| `RequiredForGoal` | `Blocked` | this must happen before the goal is met, and no call does it |
| `Informational` | `Blocked` | here is what is known; nothing is required |

Deliberately **not** added: a typed distinction between "the artefact is
external" and "we have not built it". Both are `Blocked`; the prose separates
them. An agent acts on the sentence, and a code that only ever fed a sentence is
a second place to keep in step.

### What the new state forces on the existing type

- **`required_scope` becomes `Option<Scope>`.** It answers "which client may
  perform this call", and a blocked item has no call, so there is no honest
  `Scope` to write. The invariant is `Blocked ⇔ None`: a blocked item names no
  scope, and an item with an operation always names one. Leaving it a plain
  `Scope` would have every diagnostic assert an authorisation for a request that
  does not exist.
- **Six new `ActionKind` variants**, one per diagnostic in §6, each with its
  `id()` — the kinds are not optional, because `Action` requires one.
- **`ActionCategory` gains a documented total order** — `Blocking`,
  `RequiredForGoal`, `Recommended`, `Informational`, most urgent first — and the
  diagnostic functions return their items sorted by it. `frontier()`'s own order
  is **not** changed here; it emits blocking work first by construction, and
  re-sorting it is a behaviour change this task has no reason to make. How a
  concatenated list is ordered is E9.T5's decision.

**Invariants**, enforced in `Action::new` and tested one assertion each:

- `Ready` ⇒ target is an `Operation` (already holds).
- `Blocked` ⇒ target is `ActionTarget::None`. A blocked item carrying an address
  is exactly the lie this task exists to prevent, so it is a constructor error,
  not a lint.
- `Blocked` ⇔ `required_scope` is `None`.

## 5. The ledger and the flow report say why

### 5.1 The claim and its outcome, on the wire

The worker must not invent this contract, so it is settled here.

A value is rendered tagged, because `ClaimValue` is a sum
(`check.rs:15`) and an untagged number cannot say whether it is money or a
quantity:

```json
{ "money": { "amount": "1234.56", "currency": "RUB" } }
{ "quantity": "10" }
```

Amounts are decimal strings, matching every other money field in this API
(`MoneyFlowCurrencyDto` renders `String`). Quantities likewise.

`ClaimOutcomeDto` becomes:

```json
{
  "claim": {
    "kind": "cash_balance",
    "currency": "RUB",
    "at": "closing",
    "claimed": { "money": { "amount": "…", "currency": "RUB" } }
  },
  "outcome": {
    "code": "discrepant",
    "discrepancy": { "field": "amount", "claimed": {…}, "observed": {…}, "delta": {…} }
  }
}
```

Rules, each of which is a test:

- The claim object carries `kind` plus exactly the fields its variant has:
  `currency`/`at` for `CashBalance`; `instrument`/`custody`/`at` for
  `PositionQuantity`; `currency` for the three totals; `currency` for
  `CashTurnover`.
- **`CashTurnover` asserts two values, not one** (`claim.rs:98`). It renders
  `debit` and `credit`, and never a single `claimed`. A `Discrepancy` on a
  turnover names which side in its own `field`, whose documentation already says
  so.
- The outcome object carries `code` plus **exactly one** of `discrepancy`
  (`discrepant`), `reason` (`not_comparable`), `exception` (`excepted`), and
  none of the three for `matched`. Absent keys are omitted, not null.

`EvidenceDto` gains `confirming_document` and `confirmed_document` beside the
existing parser versions, omitted when absent.

`ReconciliationStatusDto` gains `taints`. A new top-level field on the
reconciliation response carries `gaps` — every effective gap, including those
that correlate with no status.

### 5.2 Undecomposed outflows learn their account

`MoneyFlow.not_decomposed` keys its count by `(AccountId, CurrencyCode)` rather
than by currency, aligning it with the amount ledger it was always meant to
match. A new accessor returns the breakdown by account;
`not_decomposed(currency)` keeps working as the total, so every existing call
site (`iaam-app/tests/categories.rs:223,258`, `dto.rs:2012`) still compiles.

Two consequences the worker must handle rather than discover:

- `MoneyFlow` derives `Serialize`/`Deserialize` (`money_flow.rs:59`). No
  persistence of this type exists in the repository, and backward
  deserialisation of an older serialised `MoneyFlow` is **not** promised. Say so
  in the field's doc comment.
- An internal overflow test inserts the old currency-only key directly
  (`money_flow.rs:1603`) and must be updated with the shape.

`MoneyFlowCurrencyDto.not_decomposed` gains the by-account breakdown. The
interval is the report's own `from`/`to` and is not repeated per line.
`unexplained` (residual by account) already exists and is correct; it is not
changed.

## 6. The diagnostic items

Pure functions in `iaam-app`:

```
pub fn ledger_diagnostics(ledger: &ReconciliationLedger) -> Vec<Action>
pub fn flow_diagnostics(report: &MoneyFlowReport) -> Vec<Action>
pub fn verdict_diagnostics(verdict: &Verdict) -> Option<Action>
```

They are **not** wired into `frontier()`. `frontier()` answers from SQL
aggregates and never folds the journal (T3); a ledger costs
`load_events_through` plus a full fold (`scenarios/reconciliation.rs:47-51`),
and the flow report folds at `scenarios/reports.rs:125`. Attaching these to the
responses that have already paid that cost is **E9.T5**.

| kind id | category | says |
|---|---|---|
| `coverage_gap_unrepaired` | see below | account, period, tainted dimensions, refused rows by name (or that a legacy gap cannot name them); no repair transition exists — **E4 (`iaam-evc2`)** would add one |
| `independent_confirmation_missing` | RequiredForGoal | the dimension reached `accepted_internal` and no further; a document from a **different parser and a different document** must be obtained before independence can be bound |
| `discrepancy_unresolved` | RequiredForGoal | account, period, field, both sides, delta; nothing identifies which side is wrong |
| `undecomposed_outflows` | Informational | account, currency, count and amount; the rows themselves are not identified, so no rule can be written from this report |
| `unexplained_residual` | Informational | account, currency, amount; the six quantities do not explain this account's cash change |
| `possible_duplicate_undecided` | RequiredForGoal | both event identities and the dedup level; the owner decides and no endpoint records that decision |

Every one of them is `Blocked`, with `target: ActionTarget::None` and
`required_scope: None`. That is the point of the task.

**`coverage_gap_unrepaired` is not unconditionally required.** The variant's own
documentation says the same operations may already be in the journal from
another channel (`kind.rs:250`), so a clean second channel can carry a dimension
to `accepted_independent` while an older gap stands. The category is therefore
computed: **`RequiredForGoal`** while any dimension the gap taints is still
short of `accepted_independent` for that account and period, and
**`Informational`** once every tainted dimension has been confirmed anyway. A
gap that correlates with no status at all is `RequiredForGoal`: nothing has
confirmed anything there.

Identity is scoped as T3 established: an item emitted once per account, period
or currency carries those in its `id`, because an agent deduplicates by `id` and
an unscoped id makes the second occurrence invisible.

`undecomposed_outflows` is `Informational` on purpose. A report with
undecomposed rows is a correct report — `NoCategories` is documented as the
honest state of a contour whose owner has written no rules — and the repair is
not blocked by us, merely not addressable from this report.

## 7. Tests

Written to fail before the change, which for the detectors means **positive
emission tests first**. An earlier draft listed only negative and universal
assertions, every one of which passes against detectors that return nothing.

**Detectors — one positive test per kind**, each asserting the emitted item's
id, category, prose substance, and that its target is `None`:

- a gap correlated with a status whose tainted dimension is unconfirmed →
  `coverage_gap_unrepaired`, `RequiredForGoal`;
- **a gap that correlates with no status at all** → still emitted,
  `RequiredForGoal`. This is the case §2 exists for; without it the signature
  change is untested;
- a gap whose every tainted dimension reached `accepted_independent` →
  emitted as `Informational`;
- a legacy gap with empty `rows` → emitted, and says it cannot name them;
- `accepted_internal` and no higher → `independent_confirmation_missing`;
- a `Discrepant` outcome → `discrepancy_unresolved` carrying both sides;
- undecomposed outflows → `undecomposed_outflows`, per account;
- a non-zero account residual → `unexplained_residual`;
- `Verdict::PossibleDuplicate` → `possible_duplicate_undecided`.

**Then, and only then, the universal assertions**, over a fixture that first
asserts it produced **exactly the six expected kinds** — so the sweep cannot
pass by sweeping an empty set:

- every emitted item has `target: None`, `state: Blocked`, `required_scope: None`;
- two accounts with the same diagnostic get distinct `id`s;
- a fully reconciled, fully decomposed report yields an empty set.

**Transport:**

- Each `ClaimOutcome` variant renders exactly its own detail and none of the
  other two; `matched` renders none.
- A `CashTurnover` claim renders `debit` and `credit` and no single `claimed`.
- **Both** conversion sites are covered — `dto.rs` and `routes.rs` — because
  they are independent copies.
- `EvidenceDto` distinguishes two channels that share a parser version and
  differ in document, and reports the both-documents-absent pair as not
  independent.
- The reconciliation response carries a gap that matched no status.

**Core:**

- A status tainted by a gap carries a `Taint` naming that gap's dimensions,
  refused count and rows; an untainted status carries none.
- `ledger.gaps()` includes a gap that correlated with no group.
- `Action::new` rejects `Blocked` with an `Operation`, and rejects `Blocked`
  with a `Some(scope)`.

**Flow, proving the count moved and not merely the amount:** two undecomposed
events on one account and one on another — assert each account's own count, that
the counts sum to the existing per-currency count, and a case whose amounts
cancel to zero, because a count must not disappear when the money nets out.

## 8. Not in this task

Attaching items to `/v1/reconciliation`, `/v1/reports/flow` and the import
response is **E9.T5**. A coverage-gap repair transition is **E4 (`iaam-evc2`)**.
The upload path's missing gap event is `iaam-hj1o`. Carrying row identity
through `MoneyFlow` is none of these and is filed if wanted. An endpoint for the
owner's duplicate decision is not designed here. The RFC 9457 migration of error
bodies is `iaam-3pkr`.

## 9. Risks

**`required_scope` changes shape for every existing action.** It becomes
`Option<Scope>`, which touches T2's constructor and its callers. Accepted: the
alternative is every diagnostic naming an authorisation for a request that does
not exist, which is the same class of lie as inventing the address.

**`ReconciliationStatus` and the ledger both carry taints.** Duplication is
deliberate: the ledger's set is complete, the status's set is what a holder of
that status needs. The risk is drift between them, bounded by deriving both from
one `collect_coverage_gaps` result in one pass.

**Six blocked items could read as six dead ends.** They are, and saying so is
the improvement. The failure mode to watch is the opposite one: a later change
that quietly gives one an operation without checking that the operation resolves
the problem. The exact-set assertion in §7 is the guard.
