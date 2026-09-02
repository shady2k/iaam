# E9.T5 — Actions ride along where the response already holds the context

Bead: `iaam-7zbb` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

`GET /v1/actions` answers "what does this instance need next" for the whole
instance. It is the right answer to that question and the wrong shape for an
agent that has just called something specific: after uploading a document, an
agent should not have to re-query a global queue and work out which of its items
its own upload caused.

This task adds the second carrier. **The same envelope, the same catalog, the
same items** — an action attached to a response is an `ActionDto` built by the
same `action_dto` that `/v1/actions` uses, resolved through the same
`ActionCatalog`. Two carriers, one truth. If the two ever disagree about what an
action is, this task has made things worse rather than better.

## The rule: context, not sentiment

**An action rides along only where the producing response has its subject fully
bound.** Not "where an action would be useful" — where the response already
holds every value the item names. A response that attaches an action about an
account it does not name is telling the agent to go and find out which account,
which is the thing the epic exists to stop.

Applied honestly, this rule silences some places that look like obvious
candidates. Those are enumerated in §4, and the silence is the deliverable
there, not a gap in it.

## 1. What each candidate response actually holds

Established by reading the handlers and the DTOs.

| Response | Holds | Verdict |
|---|---|---|
| `ReconciliationResponseDto` (`GET /v1/reconciliation`) | account, from, to from the query; statuses and gaps; the handler folds the ledger | **carries** |
| `MoneyFlowReportDto` (`GET /v1/reports/flow`) | contour, contour version, from, to; the handler folds the flow | **carries** |
| `SyncOutcomeDto` (`POST /v1/brokers/{broker}/sync`) | `recorded`, three counts, `assertions_withheld` — **and nothing else**. Account, period and broker live in `BrokerSyncRequest`, not in the response | **carries only self-contained verdict items** |
| `DocumentDto` (`POST /v1/documents`) | document hash, source, broker, parser version, `period_from`/`period_to` (both `Option`), rows | **carries only self-contained verdict items** |
| `Vec<VerdictDto>` (`POST /v1/ingest/csv`) | nothing: the response *is* a bare array with no envelope | **cannot carry** — §4 |

The sync response not holding its own subject is worth stating plainly, because
the bead assumed otherwise: it says broker sync "knows broker, account and
period". The **handler** knows them. The response does not, and it is the
response an agent reads.

## 2. Where an item is fully bound today

`Verdict::PossibleDuplicate { event, of, level }` is **self-contained**: both
event identities and the deduplication level are in the verdict itself, and
`verdict_diagnostics` (E9.T4) already builds its item from nothing else. It
therefore rides along on both import responses without either of them gaining a
field.

That is the whole of the verdict-carried set today, and it is not an oversight.
Of the ten `Verdict` variants, four are never constructed outside tests
(`Accepted`, `Discrepancy`, `NeedsReconciliation`, `NeedsClassification` — found
while reducing the agent skill, `iaam-zu6m`), so an item for them would be dead
code with a test that could only ever exercise a hand-built value. The rest
(`Duplicate`, `Rejected`, `Quarantined`, `Unsupported`) describe what was **not**
recorded and have no repair call.

`ledger_diagnostics` and `flow_diagnostics` are already whole-report functions
and attach directly to the two report responses.

## 3. Attachment, concretely

**Field.** Each carrying response gains `actions: Vec<ActionDto>`, **always
present, even when empty.** An empty array says "this was computed and there is
nothing"; an absent key says "this response does not do actions", and an agent
cannot tell the second from a bug. Additive and non-breaking on every carrier.

**Reconciliation.** `scenarios::reconciliation::report` already holds the
`ReconciliationLedger`. It computes the diagnostics **there**, while the ledger
is in hand, and returns them in `ReconciliationReport`. The handler must not
rebuild a ledger to get them: that would fold the journal twice per request, and
the second fold could disagree with the first.

Filtered to the requested account and to periods intersecting the requested
range, exactly as `statuses` and `gaps` already are. An agent asking about one
account and one month must not be handed items about another.

**Flow.** The handler calls `flow_diagnostics` on the report it already
computed.

**Imports.** `sync_broker` and `upload_document` map each recorded verdict
through `verdict_diagnostics` and collect what it returns. Deduplicated by `id`:
two rows can be flagged against the same prior event, and the same item twice is
noise an agent has to filter.

**Ordering** is the one thing T4 left to this task. The diagnostic functions
already sort by category. Where a response carries items from more than one
source, they are concatenated and sorted once, by `ActionCategory` and then by
`id` — the second key so the order is total and a test can assert it, rather
than depending on the order the sources happened to run in.

## 4. Where there is no action, and why

Each of these is a decision, recorded so it is not re-litigated as an oversight.

**`POST /v1/ingest/csv` gets nothing.** Its response is a bare
`Vec<VerdictDto>` with no envelope, so attaching would mean wrapping it in an
object — a breaking change to a shipped route, for items an agent can obtain
from `/v1/actions`. If that route is reshaped for another reason, it gains the
field then.

**Bare `Provisional` gets nothing — and the reason is not the one the bead
gives.** The bead proposes leaving it until its result carries a reconciliation
target, its concern being that `Verdict::Provisional { event }` carries an event
id and nothing else. That is true, and it is not the binding reason, because the
**carrier** could supply what the verdict lacks: `BrokerSyncRequest` has account
and period, and `DocumentDto` has both when the period was inferred.

The binding reason is duplication. The action a provisional fact wants is
"assert a control balance for this account and period", and `frontier()` already
computes exactly that as `provide_control_assertion`, once per account. A
per-verdict copy would give an agent the same work under a different `id`, which
defeats deduplication by `id` — the mechanism T3 scoped its identities for.

**So `Verdict` is not enriched.** Its cost, stated as the bead asks: every
future verdict-carried item is limited to what the verdict itself holds, and a
variant needing account or period will require either the carrier to supply it
or the type to change. That is the right default, because enriching `Verdict`
would put reconciliation context on a value produced by ingestion, which is a
coupling the two-channel design exists to avoid.

**`POST /v1/reconciliation/balance` gets nothing here.** It answers with
statuses and no gaps, which is `iaam-g5xk`. Attaching actions to a response that
is itself incomplete would build on a known gap; that bead decides its shape
first.

## 5. Tests

**Each attachment proves its subject is bound**, which is the acceptance
criterion:

- The reconciliation response's items name only the requested account, proved by
  a fixture with a diagnostic on a **second** account that must not appear.
- Its items name only periods intersecting the requested range, proved the same
  way with a non-overlapping period.
- The flow response's items name only accounts within the requested contour.
- A sync whose rows produce a possible duplicate carries exactly one item, and
  it names both event ids and the level found in the response's own `recorded`
  array — the binding is checked against the response, not against the fixture.
- Two rows flagged against the same prior event yield **one** item, not two.

**The envelope is the same envelope:**

- An item attached to a report and the corresponding item from `/v1/actions`
  serialise identically for the same underlying `Action` — asserted over the
  whole JSON object, not field by field, so a field added to one carrier and not
  the other fails.
- Every attached item resolves its operation through the same `ActionCatalog`;
  a blocked item carries `"type": "none"` and no scope.

**The always-present rule:**

- A clean instance's report carries `"actions": []`, present and empty. Asserted
  on the raw JSON, because `skip_serializing_if` on an added field is exactly
  the mistake this rule exists to prevent.

**Ordering** is total: two items of the same category come back in `id` order.

**Not attached, and tested as such:**

- `POST /v1/ingest/csv` still answers a bare array — asserted, so the decision
  in §4 is recorded in a test rather than only in prose.
- A response whose only verdicts are `Provisional` carries `"actions": []`.

**Prove the tests can fail.** For the account-filter test and the
same-envelope test, mutate the production code, confirm the failure, restore,
and report what it said. Check the mutation actually landed before believing the
result.

## 6. Not in this task

Adding gaps to the balance route is `iaam-g5xk`. `flow_diagnostics`'s panic on
aggregate overflow is `iaam-zhvh`. Wrapping `POST /v1/ingest/csv` is not
scheduled. New action kinds are not invented here — T5 carries what T2, T3 and
T4 compute, and if a carrier seems to want an item nobody computes, that is a
finding to report, not a licence to write one.

## 7. Risks

**Two carriers can drift.** The mitigation is that they share `action_dto` and
the catalog, and the same-envelope test asserts identical serialisation. The
failure to watch for is a carrier that builds an `ActionDto` by hand "just for
this case".

**A report response grows.** A ledger with many gaps attaches many items to a
reconciliation answer. Accepted: the items are the honest content of that
answer, and the alternative — truncating — would silently omit work, which the
epic already refused once when it cut pagination from the computed queue.
