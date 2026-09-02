# E9.T5 — Actions ride along where the response already holds the context

Bead: `iaam-7zbb` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

`GET /v1/actions` answers "what does this instance need next" for the whole
instance, computed from SQL aggregates and deliberately never folding the
journal. That is the right answer to that question and the wrong shape for an
agent that has just called something specific: after a broker sync, an agent
should not have to re-query a global queue and work out which of its items its
own sync caused — and in the case of the diagnostics, the global queue does not
contain them at all.

This task adds the second carrier for the items T4 computes.

## 1. The correction that shapes the task

An earlier draft of this spec said the diagnostics were "also available from
`/v1/actions`" and built a test around an item appearing in both places. **That
is false.** `list_actions` calls `frontier()` and nothing else
(`routes.rs:86-99`), and `frontier()` never invokes `ledger_diagnostics`,
`flow_diagnostics` or `verdict_diagnostics` (`actions.rs:249`).

That is not an oversight to fix here. The frontier answers from store views
precisely so that asking "what next" does not cost a journal fold; wiring the
diagnostics into it would fold the journal **and** the flow report on every
call. So:

**For a T4 diagnostic, the ride-along response is the only carrier there is.**
"Two carriers, one truth" is true of the **envelope** — the same `ActionDto`,
built by the same `action_dto`, resolved through the same `ActionCatalog` — and
not of every item. The spec says envelope where it used to say item, and the
tests test the envelope.

## 2. What each candidate response actually holds

| Response | Route | Verdict |
|---|---|---|
| `ReconciliationResponseDto` | `GET /v1/reconciliation` | **carries** — the handler holds the ledger's diagnostics via the scenario |
| `MoneyFlowReportDto` | `GET /v1/reports/flow` | **carries** — contour, version, from, to (`dto.rs:1916`) |
| `SyncOutcomeDto` | `POST /v1/brokers/{broker}/sync` | **carries** the one self-contained verdict item |
| `DocumentDto` | `POST /v1/documents`, `POST /v1/documents/{id}/reparse` | **carries nothing** — §4 |
| `Vec<VerdictDto>` | `POST /v1/ingest/csv` | **carries nothing** — §4 |

Two precisions, because the bead and the earlier draft both got them wrong:

- **`SyncOutcomeDto` does not hold its own subject.** It has `recorded`, three
  counts and `assertions_withheld` (`dto.rs:4184`). The account and period are
  in `BrokerSyncRequest` (`dto.rs:4164`) and the broker is a **path** parameter
  (`routes.rs:856`) — that is, in the handler, not in the response an agent
  reads. This is why sync carries only an item that needs no subject from its
  carrier.
- **`ReconciliationResponseDto` does not name its subject either.** It is
  `{ statuses, gaps }` (`dto.rs:3921`); the account and range are per-status and
  per-gap, so an empty response loses the question it answered. Not fixed here —
  filed as `iaam-647w` — but it is why the account filter in §3 must be applied
  when the items are built rather than checked against the envelope afterwards.

## 3. The reconciliation and flow attachments

**The ledger is folded once.** `scenarios::reconciliation::report` loads the
journal and builds the `ReconciliationLedger` (`reconciliation.rs:54`); the
handler receives only the filtered `ReconciliationReport` (`routes.rs:420`).
Diagnostics are therefore computed **inside that scenario**, while the ledger is
in hand, and returned in `ReconciliationReport`. A handler that rebuilt a ledger
would fold the journal twice per request, and the second fold could disagree
with the first.

**Filtering needs a scoped API, and the worker must not invent one at the call
site.** `ledger_diagnostics(&ledger)` walks the whole ledger, and an `Action`
exposes no typed account or period — only an opaque `id` and prose
(`actions.rs:158-204`). Filtering the returned `Vec<Action>` would mean parsing
identifiers, which is exactly the string-archaeology this epic exists to
prevent.

So `ledger_diagnostics` gains a scoped sibling that takes the account and the
period and filters on the **typed** gap and status data, the same predicate the
scenario already applies to `statuses` and `gaps` (`reconciliation.rs:60-70`,
`reconciliation.rs:92-100`): same account, and periods that intersect. The
unscoped function stays, because a caller holding a whole ledger and wanting all
of it is a legitimate shape and T4's tests use it.

**Flow.** The handler calls `flow_diagnostics` on the report it already
computed. No scoping is needed: the projection never admits a leg outside the
contour (`money_flow.rs:175`), so the report cannot name an account it does not
cover.

## 4. Where there is no action, and why — each one a decision

**`POST /v1/documents`, its reparse sibling, and `POST /v1/ingest/csv` carry
nothing, because nothing they produce has an item.**
`Verdict::PossibleDuplicate` is constructed in exactly one place in production
code — `scenarios/sync.rs:189`, the broker sync path. The document and CSV paths
never produce it, and `verdict_diagnostics` returns `None` for every other
variant (`actions.rs:438`). Attaching a field to those responses would add an
always-empty array and, for `DocumentDto`, would drag `reparse_document` into
this task's contract (both routes share `document_dto`, `routes.rs:1776`) for no
item that can ever appear in it.

This supersedes the earlier draft's reason, which was that the CSV response is a
bare array with no envelope. That is true (`routes.rs:1538`) and it is not the
reason: there is simply nothing to put there.

**Bare `Provisional` carries nothing, and the reason is not duplication.** The
earlier draft argued that `frontier()` already emits `provide_control_assertion`
for the same account, so a per-verdict item would be a second copy. The account
part is right and the period part is wrong: the frontier asks for an assertion
over the account's **whole observed activity span**, first to last effective
date (`actions.rs:556`, `actions.rs:612`), while a sync knows only its own
requested range. A January sync containing one operation on the 15th produces a
frontier item for 15–15, not 1–31.

So the two are not duplicates; they are **two different requests for the same
underlying need, and nothing decides which period is the one to ask for**.
Emitting both would hand an agent two overlapping assertions to obtain from the
owner, and picking one silently would encode a rule this design has not made.
That is a reason to withhold, not a gap to paper over.

**`Verdict` is not enriched.** Its cost, stated as the bead asks: a future
verdict-carried item is limited to what the verdict itself holds. That is the
right default, because enriching `Verdict` would put reconciliation context onto
a value produced by ingestion, and the independence of the two channels is what
the whole reconciliation design rests on.

**`POST /v1/reconciliation/balance` carries nothing here.** It answers with
statuses and no gaps (`iaam-g5xk`); that bead settles its shape first.

## 5. Attachment, concretely

**Field.** Each carrying response gains `actions: Vec<ActionDto>`, **always
present, even when empty.** An empty array says "this was computed and there is
nothing"; an absent key says "this response does not do actions", and an agent
cannot tell the second from a bug.

**Two rows against the same prior event produce two items, not one.** An earlier
draft required collapsing them. That is impossible and it would be wrong:
`verdict_diagnostics` keys its identity on the new event, the prior event and
the level (`actions.rs:445-452`), and two recorded rows necessarily have
different new event ids. Collapsing by prior event would discard a new event the
item exists to name. The sync path genuinely produces this — it records each
possible duplicate and adds it to `known` (`sync.rs:186-193`) — so it is a
tested case, and the expectation is **two fully-bound items**.

**Ordering.** Items are sorted by `ActionCategory` and then by `id`. The second
key is what makes the order total and assertable; T4's sort by category alone
leaves ties in generation order.

## 6. Tests

Every positive test below **makes an HTTP request and asserts on the carrier's
raw JSON.** A test that calls `ledger_diagnostics` or `flow_diagnostics`
directly passes today, before any attachment exists — T4 already shipped those
functions, their ordering and their filtering-by-contour. Testing them again
proves nothing about this task.

**Each attachment proves its subject is bound:**

- `GET /v1/reconciliation` for one account returns `actions` naming only that
  account, proved with a fixture carrying a diagnostic on a **second** account
  that must not appear in the response JSON.
- The same, for a period that does not intersect the requested range.
- `GET /v1/reports/flow` returns items only for accounts in the requested
  contour.
- A sync whose rows produce two possible duplicates against the same prior event
  returns **two** items, each naming its own new event id — and each id is
  checked against the `recorded` array in the same response body, not against
  the fixture, so the binding is proved from the agent's point of view.

**The envelope:**

- An attached item carries every key `ActionDto` defines, and a blocked item
  carries `"target": {"type": "none"}` and no `required_scope` — asserted on the
  carrier's raw JSON. This cannot prove `action_dto` was reused (hand-built
  identical JSON would pass); reuse is a review point, and the implementation
  must call the existing function rather than write a second conversion.

**The always-present rule:**

- A clean instance's reconciliation and flow responses carry `"actions": []`,
  present and empty, asserted on raw JSON — because `skip_serializing_if` on the
  new field is exactly the mistake this rule exists to prevent.

**Ordering** is total: two items of the same category come back in `id` order,
asserted on the response.

**Not attached, and tested as such** — these are the decisions of §4 recorded in
tests rather than only in prose:

- `POST /v1/ingest/csv` still answers a bare array with no `actions` key.
- `POST /v1/documents` answers with no `actions` key.
- A sync whose only verdicts are `Provisional` returns `"actions": []`.

**Prove the tests can fail.** For the account-filter test and one
always-present test, mutate the production code, confirm the failure, restore,
and report what the failure said. Check the mutation actually landed before
believing the result — a previous attempt in this project silently did not apply
and its "proof" passed vacuously.

## 7. Not in this task

Wiring diagnostics into `/v1/actions` is refused, not deferred: the frontier's
cost model is the reason it is useful. Adding gaps to the balance route is
`iaam-g5xk`; `flow_diagnostics`'s panic on aggregate overflow is `iaam-zhvh`;
the reconciliation response not naming its own subject is `iaam-647w`. No
new action kinds are invented here — T5 carries what T2, T3 and T4 compute, and
a carrier that seems to want an item nobody computes is a finding to report.

## 8. Risks

**Two conversion paths can drift.** The mitigation is that attachment calls the
existing `action_dto`. No behavioural test can prove that, so it is a review
point: a carrier that builds an `ActionDto` by hand "just for this case" is the
failure to look for.

**A reconciliation answer grows with the number of gaps.** Accepted: the items
are the honest content of that answer, and truncating would silently omit work —
which this epic already refused once when it cut pagination from the computed
queue.
