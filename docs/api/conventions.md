# HTTP API conventions

This document is about **IAAM's own HTTP API**, the one rooted at `/v1` and
described by `/v1/openapi.json`. The contracts under `tinkoff-invest/` are
somebody else's API, read here as a reference; nothing in this file applies to
them.

The reader is a client author. Each section states a rule that every existing
route already follows, so that the shape of a route can be predicted before it
is called rather than discovered by calling it.

---

## 1. The shape of a list response

> **A list is a bare JSON array, unless the response carries something about the
> list itself — a page cursor, a version the items were computed under, a
> statement of what the list covered — in which case it is an object with
> `items` and that something beside it.**

### 1.1 Why the bare array is the default

A wrapper is not free. `{"items": […]}` costs the client one indirection on
every read, and it costs it forever: once published, the key cannot be removed
without breaking every caller. What a client buys for that price is a place to
put a fact that belongs to the answer as a whole. Where there is no such fact —
`GET /v1/accounts` returns the accounts, all of them, in one response, and there
is nothing true of the set that is not true of each member — the wrapper is an
empty box, and an empty box that can never be thrown away.

So the bare array is not laziness and the wrapper is not ceremony. The shape
reports whether the answer has something to say about itself.

### 1.2 Why a wrapper, when there is one

Two routes wrap today, and each wraps for a fact that has nowhere else to live.

**`GET /v1/actions` → `{"policy_version": …, "items": […]}`.** The items are
computed, not stored: the service derives the outstanding actions from the state
of the journal under a policy, and the policy has a version. That version
belongs to the whole computation. Copying it onto every item would invite a
client to believe two items could disagree about it, and they cannot; omitting it
would leave a client comparing two responses with no way to tell a change in the
owner's data from a change in the rules.

**`GET /v1/journal/events` → `{"rows": […], "next": …}`.** The journal is read a
page at a time, and the position to resume from is a property of the page, not
of any row in it. It is also the one field that cannot be reconstructed from the
rows: an absent `next` means "this was the last page", which no row states.

The same reasoning already appears in the code, at the type that first needed it:
`BalancesReportDto` is an object rather than an array of account rows because
`negative_cash` is one fact about the whole answer, and `MarketPriceSeriesDto`
carries `complete_through` for the same reason — it is returned even when `rows`
is empty, which is the only way a client can tell "this instance holds nothing
for the series" from "the series is complete and holds nothing in this interval".

### 1.3 The field beside the list is named for what the list is

The rule fixes that there **is** a field holding the list, not one universal
spelling of it. `items` is the default and the name to reach for; a route whose
payload has a more precise word uses it — `rows` for the journal page and for
the market series, `accounts` for the balances report, `statuses` and `gaps` for
reconciliation, which carries three lists and could not have named any of them
`items` without lying about the rest.

A client should therefore read the shape as: array at the top level, or an object
whose documented list field it looks up once.

### 1.4 A statement of what the list covered is list-level information

`GET /v1/reports/balances` and `GET /v1/reports/returns` both carry a
`population` block: the accounts the report covered and the known accounts it did
not. This is the third case named in the rule, and it is the clearest of them.
It cannot be a field on a row, because the accounts it reports on are precisely
the ones that have no row — the report left them out, and that silence is what
the block breaks. A report over part of the owner's money that did not say so
would read as an answer about all of it.

So `population` is exactly the kind of fact the wrapper exists for, and both
reports are objects for the same reason `GET /v1/actions` is.

### 1.5 What this means when a new list route is added

Decide the shape when the route is published, because it cannot be changed
afterwards without breaking callers. The question to answer is not "might this
list grow?" but "is there, or will there be, a fact about the answer as a whole
that no item can carry?" A page cursor, a version, a coverage statement, a
completeness boundary — those are the facts. A count is not one: a client that
holds the whole array can count it.

An example, invented: a route returning the owner's accounts at `Bank One` and
`Bank Two` is a bare array. The same route, if it reported which banks it managed
to reach and which it could not, is an object — because "Bank Two did not answer"
is not something an account row can say.

---

## 2. The lists as they stand

Every route whose success response is, or contains, a list of the owner's
things. Read it as the lookup table for §1.

| Route | Response | Shape | Why |
|---|---|---|---|
| `GET /v1/accounts` | `[AccountDto]` | bare array | whole list, nothing about the set |
| `GET /v1/instruments` | `[InstrumentDto]` | bare array | whole catalogue |
| `GET /v1/categories` | `[CategoryDto]` | bare array | whole history; each item carries its own retirement |
| `GET /v1/category-groups` | `[CategoryGroupDto]` | bare array | whole list |
| `GET /v1/category-rules` | `[CategoryRuleDto]` | bare array | whole history; the version is per rule, not per list |
| `GET /v1/classification-rules` | `[ClassificationRuleDto]` | bare array | whole history; the version is per rule, not per list |
| `GET /v1/tokens` | `[TokenDto]` | bare array | whole list, revoked included |
| `GET /v1/broker-access` | `[BrokerAccessDto]` | bare array | whole list, revoked included |
| `GET /v1/contours` | `[ContourDto]` | bare array | whole list; each contour carries its own version |
| `GET /v1/import-sessions` | `[ImportSessionDto]` | bare array | whole list, newest first |
| `GET /v1/actions` | `ActionsResponseDto` | object, `items` | `policy_version` — the policy the items were computed under |
| `GET /v1/journal/events` | `JournalPageDto` | object, `rows` | `next` — the position to resume the page from |
| `GET /v1/market/prices` | `MarketPriceSeriesDto` | object, `rows` | `complete_through` — how far the series is known |
| `GET /v1/market/fx` | `MarketFxSeriesDto` | object, `rows` | `complete_through` |
| `GET /v1/market/key-rate` | `MarketKeyRateSeriesDto` | object, `rows` | `complete_through` |
| `GET /v1/reconciliation` | `ReconciliationResponseDto` | object, `statuses` | three lists — `statuses`, `gaps`, `actions` — none of them a property of another's rows |
| `GET /v1/transfer-pairings` | `CrossSourceMatchingDto` | object, `candidates` | `unmatched` — the legs nothing paired with, which no candidate can carry |
| `GET /v1/accounts/{id}/transfer-partners` | `AccountTransferPartnersDto` | object, `partners` | `stated` — whether the owner has ruled at all, which an empty array cannot say |
| `GET /v1/import-sessions/{session}` | `ImportSessionContentsDto` | object, `questions` | the session it belongs to, and how many questions are unanswered |
| `GET /v1/reports/balances` | `BalancesReportDto` | object, `accounts` | `negative_cash`, `population` |
| `GET /v1/reports/returns` | `ReturnsAnswerDto` | object | not a list at the top level; `population` sits beside the report's own figures |
| `GET /v1/reports/flow` | `MoneyFlowReportDto` | object, `currencies` | the interval, the scope version, `population`, `actions` |
| `POST /v1/ingest/operations` | `[VerdictDto]` | bare array | one verdict per submitted row, in the caller's own order |
| `POST /v1/ingest/journal-events` | `[VerdictDto]` | bare array | one verdict per submitted row |
| `POST /v1/ingest/csv` | `[VerdictDto]` | bare array | one verdict per parsed row |
| `POST /v1/corrections` | `[VerdictDto]` | bare array | one verdict per submitted correction |
| `POST /v1/reconciliation/balance` | `[ReconciliationStatusDto]` | bare array | the statuses the balance changed |
| `POST /v1/import-sessions/{session}/rows` | `[ImportRowDto]` | bare array | one outcome per fed row; nothing was recorded |
| `POST /v1/documents` | `DocumentDto` | object, `rows` | the document hash, source, parser version and period |
| `POST /v1/brokers/{broker}/sync` | `SyncOutcomeDto` | object, `recorded` | the duplicate and assertion counts for the run, and `actions` |
| `POST /v1/import-sessions/{session}/commit` | `ImportCommitDto` | object, `rows` | the session and the revision the commit was planned from |
| `POST /v1/classification-rules` | `ClassificationRuleChangeDto` | object, `plan.corrections` | `applied: false` — that the plan was not carried out |
| `DELETE /v1/classification-rules/{id}` | `RecomputePlanDto` | object, `corrections` | `applied: false` |

A batch response — a verdict per submitted row — is a bare array for the same
reason a full list is: the caller supplied the batch, so it already knows what
the array is about, and each verdict carries the row number that ties it back.
Where a batch response is an object, it is because the run produced a fact of its
own: a document hash, a set of counts, a revision.
