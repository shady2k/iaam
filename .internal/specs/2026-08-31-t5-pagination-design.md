# T5: pagination, and the interval boundary it depends on — design

Bead: `iaam-yeqp`. Parent epic: `iaam-zn38`. Date: 2026-08-31.

Parent design: `.internal/specs/2026-08-31-tinkoff-execution-facts-design.md`
§9. Preceding tasks: T1 `iaam-woil`, T2 `iaam-dz94`, T3 `iaam-0gbe`,
T4 `iaam-6aun`, all closed. Line numbers are against the
`t1-order-state` worktree, which carries all four uncommitted.

Reviewed adversarially by codex on 2026-08-31, together with T6. Two
findings on this document, both accepted: the boundary argument in §2
overclaimed — it is safe under both readings, not correct under both —
and it missed the question that outranks it, which timestamp the filter
applies to at all; and the page cap was stated two different ways, one of
which rejects a complete import at exactly the boundary.

## 1. Two problems, and only one of them is mechanical

**Pagination.** `GetOperationsByCursorResponse` carries `has_next` and
`next_cursor` (`operations.proto:474-479`). `fetch_operations` sends one
request and parses one page (`iaam-app/src/adapters/tinkoff.rs:67-77`).
`parse_operations` refuses only the case where `has_next` is true and the
cursor is missing or empty (`iaam-broker/src/tinkoff/parse.rs:176-178`);
when a cursor **is** present it drops the page information on the floor
and returns the items, and its doc comment calls that "a complete
response". So a second page is lost silently.

**The interval boundary.** The adapter sends the final day as midnight at
its start — `rfc3339_midnight` is `{date}T00:00:00Z`
(`adapters/tinkoff.rs:678-680`) — while an `AssertionPeriod` includes the
whole final day: "Both boundaries are inclusive: a report for March
covers both the first and the thirty-first of March"
(`iaam-core/src/reconciliation/claim.rs:20-25`).

The second is why the first cannot be shipped alone. Paginating a wrong
interval perfectly is still truncation, and `sync_broker` records a
closing portfolio assertion over the interval it believes it imported.
An assertion over an interval missing almost all of its closing day is a
false statement about the data, not a gap in it.

## 2. The boundary

`from` and `to` are `google.protobuf.Timestamp`, documented only as
*«Начало периода по UTC»* and *«Окончание периода по UTC»*
(`operations.proto:463-464`). The contract does not say whether `to` is
inclusive.

The request therefore sends the **last instant of the final day**:
`{to}T23:59:59.999999999Z`, with `from` unchanged at `{from}T00:00:00Z`.

It is not *correct* under both readings of the contract — it is **safe**
under both, and the difference is worth being exact about:

- if `to` is inclusive, it covers the whole final day exactly;
- if `to` is exclusive, it excludes operations timestamped at exactly
  `23:59:59.999999999Z`, which is one representable instant.

The alternative — sending the start of the following day — is exact if
`to` is exclusive and wrong if it is inclusive, and its failure is worse:
it pulls in an operation timestamped exactly `00:00:00.000Z` of the next
day, which the next sync will also claim. A fact that belongs to two
intervals is worse than an instant that belongs to none, so the safe
option is taken and its cost is named rather than hidden.

The days are UTC days, consistent with T2 §9 and T3 §4.2.

### 2.1 The request boundary is necessary and not sufficient

There is a third question the contract does not answer, and it outranks
the first two: **which timestamp does the filter apply to?**

`OperationItem.date = 21` is *«Дата поручения»*
(`operations.proto:488`) — when the order was placed.
`OperationItemTrade.date = 6` is *«Дата сделки»*
(`operations.proto:523`) — when a fill executed. Since T2 a fact is dated
from the trade's timestamp (`adapters/tinkoff.rs:502`), not the order's.

If the gateway filters by order date, an order placed inside the interval
and filled after it comes back in this request and produces a fact dated
outside the interval we asked for — and `sync_broker` then records a
closing portfolio assertion over an interval its own facts do not match.
Fixing the request boundary fixes the interval we **ask for**, not the
interval we **get**.

T5 does not guess the semantics. It does two things instead:

1. establishing them from a live response is filed as `iaam-gawg`;
2. the discrepancy is made visible from our own data: if any fact
   produced by this sync carries a trade date outside `[from, to]`, the
   sync **records the facts and suppresses the control assertion** for
   that interval, on the same path a quarantined row already takes
   (`iaam-app/src/scenarios/sync.rs`). The facts are real and are kept;
   the statement about the interval is not made, because it would be a
   statement we cannot support.

This is why acceptance criterion 6 asserts the serialized request and
criterion 6b asserts the fact dates: asserting only the request would let
this whole class of error pass.

**If the gateway rejects nine fractional digits**, the fallback is
`{to}T23:59:59Z` and the loss becomes the final second rather than the
final nanosecond. That is a fallback to be taken on evidence — a
rejection from the gateway — and not pre-emptively.

## 3. Following the cursor

`parse_operations` stops discarding the page information. It returns the
items together with `has_next` and `next_cursor`; the existing refusal
for `has_next` with no cursor stays exactly as it is.

`fetch_operations` then loops:

1. first request: `from`, `to` and `limit` set, no cursor;
2. while the last page reported `has_next`: repeat the request with
   `from`, `to` and `limit` **unchanged** and `cursor` set to the page's
   `next_cursor`;
3. accumulate the items of every page in order.

`limit` is set explicitly to `1000`, the contract's maximum
(`operations.proto:466`). Explicit rather than defaulted because the
default is the gateway's to change, and a page size that changes under us
changes how many requests a sync makes without anything in our code
moving.

**Every cursor seen is recorded, and a repeat is a refusal.** A gateway
returning a cursor it has already returned — the A-B-A case — would
otherwise loop forever or, worse, accumulate the same page repeatedly
into what is presented as one interval's operations. The refusal names
the repeated cursor.

**An exhausted page limit is a refusal, not a truncation.** The loop is
capped at 100 pages, which at a limit of 1000 is 100 000 operations for
one account and one interval.

The cap is on pages fetched, and the refusal is triggered by needing one
more: a response that arrives as page 100 with `has_next` false is a
**complete** response and is accepted. Only `has_next` still true after
the hundredth page refuses. The distinction is not pedantry — a rule
written as "reaching 100 pages refuses" rejects a complete import at
exactly the boundary, and no test that hammers `has_next` forever would
ever notice.

Reaching that point means either an interval far larger than anything an
owner syncs, or a gateway that is not terminating. Both are conditions to
report, and neither is a reason to return a prefix and call it the
interval. The refusal names the page count and the cap.

None of the three failures returns partial data. This channel's caller
records a control assertion over the interval on the strength of the
result, so "some of the interval" must not be representable as success.

## 4. What the failures are, in the port's vocabulary

All three — a repeated cursor, an exhausted page cap, and the existing
`has_next` with no cursor — are `BrokerError`, not quarantined rows. A
quarantined row means "this row could not become a fact"; these mean
"this interval was not retrieved", which is a different statement and has
a different consequence for the assertion.

## 5. Not changed by T5

- `sync_broker`'s existing rule that a quarantined row suppresses the
  control assertion. T5 makes the interval retrieval honest; what the
  caller does with a partial *parse* is `iaam-dbvu`'s question.
- `TINKOFF_PARSER_VERSION` stays `tinkoff-api/3`: pagination changes
  which rows are retrieved, not how a fact is constructed.
- The portfolio endpoint, which is not paginated.
- Securities transfers — T6.

## 6. Acceptance criteria

1. A response with `has_next` true and a cursor causes a second request
   that repeats `from`, `to` and `limit` unchanged and carries the
   cursor.
2. The operations of both pages are returned, in page order.
3. A cursor that repeats one already seen is a refusal naming that
   cursor, and returns no operations.
4. A response arriving as page 100 with `has_next` false is accepted in
   full; `has_next` still true after page 100 is a refusal naming the
   count and the cap, and returns no operations.
5. `has_next` true with an absent or empty cursor stays the refusal it
   is today.
6. The request's `to` is the last instant of the final day, not its
   first, and `from` is unchanged.
6b. A sync whose facts include a trade date outside `[from, to]` records
   those facts and writes **no** control assertion for the interval.
7. `limit` is sent explicitly.
8. No failure path returns a partial list of operations.

## 7. Tests

Against the client and the adapter with a stubbed transport; no fixture
file changes — `tests/fixtures/api/` is a policy directory
(`scripts/check-diff-lint.sh:80-83`) and the recorded response may be
read, not modified.

- a two-page response returns the operations of both pages, in order;
- the second request repeats `from`, `to` and `limit` and carries the
  first page's `nextCursor` — asserted on the recorded request, not
  inferred from the result;
- a response whose `nextCursor` equals a cursor already seen refuses,
  naming it;
- an A-B-A sequence over three pages refuses at the third, not at the
  second;
- a gateway that always reports `has_next` refuses at the cap, naming
  the count;
- a response that ends on page 100 with `has_next` false is accepted, and
  all hundred pages' operations are returned — the boundary the previous
  case cannot distinguish;
- `has_next` true with an empty cursor still refuses;
- for an interval of one day, the request carries
  `T00:00:00Z` and `T23:59:59.999999999Z` — the boundary asserted
  directly, since nothing downstream would notice it silently;
- a single-page response makes exactly one request;
- a page containing an operation whose fill is dated after `to` yields
  the fact and no control assertion for the interval.

## 8. Gates

`cargo check` and the crates' own tests, as in T1 to T4. The workspace
gates run once at the end of the epic.
