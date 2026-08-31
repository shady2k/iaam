# T-Invest: a trade, not an order, becomes a journal fact — design

Bead: `iaam-d8b.23`. Date: 2026-08-31.

Revision 3. Revisions 1 and 2 were reviewed adversarially with codex and
neither survived; the second review concluded that the blocking problems
were not in this channel at all. They became epic `iaam-jdmc`, which is
now closed, and this revision is written against the core it produced.
§12 records what the earlier revisions got wrong, because a design whose
first two versions were wrong must say where.

Source of truth for the channel contract:
`docs/api/tinkoff-invest/operations.proto`, from
`RussianInvestments/investAPI` at commit `3eaf23a` (7 November 2025).
Every claim about our own code was checked against the code.

## 1. Problem

The channel turns an **order** into a journal fact. An order is not a
trade: it may be cancelled, fill partially, fill across several days, and
fill at a price the order row never carries. The journal is append-only.

`OPERATION_STATE_EXECUTED = 1` is documented as *«Исполнена частично или
полностью»*, so even the conclusive-sounding state never proves that the
ordered quantity was the executed quantity. `date = 21` is *«Дата
поручения»*. The execution data is in the response and discarded:
`grep -rn 'tradesInfo|quantityDone|quantityRest|cancelReason' crates/`
returns nothing.

## 2. What the core now provides

Epic `iaam-jdmc` changed four things this design previously had to work
around. They are preconditions, not context:

- **Identity is scoped by the source's own guarantee.** `IdentityScope`
  (`iaam-core/src/reconciliation/evidence.rs`) is declared per channel
  through the `BrokerChannel` port (`iaam-app/src/ports.rs:590`), and the
  T-Invest channel already returns `IdentityScope::Account`
  (`adapters/tinkoff.rs:98`). Migration 0012 widened the unique index
  accordingly.
- **A possible duplicate reaches the owner.**
  `Verdict::PossibleDuplicate { event, of, level }` is threaded through
  `sync_broker` and reported by the transport. Revision 2 claimed this
  behaviour existed; it did not.
- **Intraday order comes from the source.** `EffectiveOrder` carries
  `source_time`, and `SubmittedOperation.source_time` carries it in
  (`iaam-ingest/src/operation.rs:192-195`). Within a day, timed events
  sort before untimed ones, equal times break on `raw_hash`.
- **A commission can reach the lot basis without inventing cash.**
  `EventKind::Trade` carries `basis_fee` (posted, rounded) and
  `basis_fee_exact` (`kind.rs:141-146`); the commission is parsed as
  exact `CalcMoney`, field-locally, so it no longer rejects the row, and
  `TINKOFF_PARSER_VERSION` is already `tinkoff-api/2`.

Consequence already visible: the SBER purchase in the recorded fixture
reaches the journal. Before the epic the channel accepted only cash
deposits.

## 3. Decision

A **trade**, not an order, becomes a journal fact. `trades_info` is the
evidence of what happened; the order state explains the remainder and
never negates an execution.

One trading order expands into N `SubmittedOperation`s — one per element
of `trades_info.trades`. Quantity, price and timestamp come from the
trade. An order with no trades produces no fact and is never dated by
the order timestamp.

## 4. Parsing

`ChannelOperation` gains what it currently discards:

```rust
pub struct ChannelTrade {
    pub num: String,
    pub at: OffsetDateTime,   // full timestamp, not a date
    pub quantity: Quantity,
    pub price: ChannelMoney,
}
```

plus `trades: Vec<ChannelTrade>`, `quantity_done`, `quantity_rest`,
`cancel_reason`, and `position_uid` (§8).

**The order state becomes a typed enum**, not the `String` it is today
(`parse.rs:93`), so that an unrecognised value is a named variant
carrying the raw text rather than a wildcard match at runtime:

```rust
pub enum ChannelOrderState {
    Executed,
    Cancelled,
    InProgress,
    Unspecified,
    Unrecognised(String),
}
```

Parsing rules:

- a trade with a missing or unparsable field rejects the **whole row**;
  a partially parsed trade list would silently record fewer executions
  than happened;
- `trades_info` absent or empty gives an empty vector, not a rejection:
  a cancelled order legitimately has none;
- `quantity_done` is a proto3 scalar, so absent and zero are
  indistinguishable on the wire. The parser must therefore treat it as
  **present with value zero** and never as "not reported"; the
  completeness check in §5 is written accordingly.

## 5. Adaptation

`adapt_operations` stops being 1:1. A `Buy`/`Sell` row expands into one
`SubmittedOperation` per trade; every other family stays one row, one
fact.

**State gates every family, not only trades.** Revision 2 put this check
inside the `Buy | Sell` branch, which left coupons and fees unguarded.

| State | Behaviour |
|---|---|
| `Executed` | expand its trades |
| `InProgress` | expand its trades |
| `Cancelled` | expand its trades; none present is not an anomaly and produces no quarantine row |
| `Unspecified` | no facts; quarantined as "the channel did not name the state" |
| `Unrecognised(v)` | no facts; quarantined, reason quotes `v` |

**Trades win over state.** The contract carries `quantity_done` and
`trades_info` on `OperationItem` regardless of state, and nowhere says a
cancelled order has no fills — a partially filled order whose remainder
is cancelled is an ordinary market event. Discarding its executions would
erase facts that happened.

**Two completeness checks, and both are now justified by arithmetic.**

1. `Σ trades[].quantity` must equal `quantity_done`; a mismatch
   quarantines the row with both numbers.
2. `Σ (quantity × price) + accrued_int` must equal the order's
   `payment`; a mismatch quarantines the row with both amounts.

Revision 2 refused the second check as an unproven premise. That was
wrong, and hid something: in the recorded response the BUY row's
`payment` is exactly `-270.130000000`, which is `1 × 270.13` — the
commission is **excluded** from it. So `payment` is the sum over trades,
and reconciling it catches a truncated `trades_info` by money as well as
by quantity.

**The accrued-interest term is inferred, not proven, and the design says
so.** Every operation in the recorded response is a share with
`accruedInt` zero, so what is actually verified is the degenerate case
`payment = Σ (quantity × price)`. For a bond the contract carries
`accrued_int = 46` alongside `payment = 41` and does not state their
relationship. Including the term is the reading under which a bond
purchase reconciles at all; if it is wrong, the check quarantines the
row with both amounts rather than recording a distorted fact, which is
the correct failure direction. Confirming it against a live bond
purchase is filed in §13 — until then, a bond row that fails this check
is refused, and that refusal is evidence about our formula as much as
about the source.

**Row identity.** Per trade,
`source_operation_id = {operation_id}#{trade.num}` and
`idempotency_key = {broker_account_id}/{operation_id}#{trade.num}`.
Components are percent-escaped before joining: the contract constrains
neither `#` nor `/` out of either component, and an ambiguous key is a
silent identity collision. Composite rather than bare `num`, because in
the recorded response `num` equals the operation's own `id`, so its
independent uniqueness is not demonstrated.

The contract calls `id` mutable, so a renumbering still defeats the key.
It now degrades correctly rather than silently: the content fingerprint
matches, and after `iaam-gcsh` the owner is shown the pair instead of
getting a second entry with no hint.

**Intraday order.** `SubmittedOperation.source_time` is filled from
`trade.at`, so several fills of one order keep the exchange's order
regardless of the order the channel returned them in.

## 6. Commission

`fee_minor` stays `None` for this channel and the commission becomes the
trade's `basis_fee`.

This is not a preference. The cash leg must equal what the source
reported — `payment` = `-270.13` — and `validate_trade` enforces
`cash_leg == gross ± fee ± accrued` exactly
(`iaam-core/src/event/mod.rs:352`). Putting the commission in `fee` would
post `-270.27`, money that never moved. Leaving it out entirely would
understate the lot basis, which `lots.rs:774` defines as including the
commission (§7.2). `basis_fee` is the field that satisfies both.

The commission is allocated across the order's trades pro rata by
quantity, as a versioned rule in `iaam-core` modelled on
`resolve_basis_allocation` — monetary arithmetic belongs in the core and
must carry evidence of its inputs. **The remainder goes to the trade with
the largest quantity, ties broken by `trade.num`, never by position in
the response**: the array's order is not guaranteed, and a
position-based rule would move a kopek between lots when the response
order changed.

## 7. Accrued interest

Read from `accrued_int`. **A known zero stays `Some(0)`** — the core
distinguishes it from unknown deliberately (`lots.rs:792`), and `None`
makes lifetime metrics refuse (`zero_reinvestment.rs:375-377`) while a
known zero lets them compute. Revision 1 collapsed the two.

The contract reports it per operation, not per trade, and accrued
interest accrues by **time**, not by quantity:

- all trades on one day — allocated pro rata by quantity, which is exact,
  because the per-unit value is the same for all of them;
- trades on different days — not derivable. Each trade is still recorded,
  with `accrued_interest` unknown, and the lifetime metric refuses with
  `AccruedInterestAtAcquisitionUnknown`.

Splitting a time-accrued amount by quantity across days would sum to the
right total while putting a wrong figure on every lot: the arithmetic
would hide the error instead of revealing it.

## 8. Custody

Trades take `custody` from `position_uid = 35`, the identifier the
portfolio side already uses (`parse.rs:186-192`). Today they fabricate it
as `CustodyId(account.inner())` (`adapters/tinkoff.rs:236`), so positions
derived from trades cannot be reconciled against the broker's portfolio —
the two sides are keyed differently. A row without `position_uid` is
refused rather than guessed.

Facts recorded under `tinkoff-api/1` carry account-derived custody and
will not reconcile until re-imported. The epic's close reason must say
so.

## 9. Pagination

`fetch_operations` follows `next_cursor` until `has_next` is false,
accumulating pages, and **every cursor request repeats `from`, `to` and
the page limit unchanged** — they are independent request fields, and
varying them mid-walk silently changes what is being paginated.

Guards, because an unbounded loop against a remote source is its own
defect:

- a cursor already seen in this walk aborts with a named refusal — track
  the set, not just the previous value, so an A→B→A cycle is caught;
- a page-count limit whose exhaustion is a **refusal**, not a truncation:
  a partial interval must never be reported as complete;
- `parse_operations` keeps its `PartialResponse` check, and its doc
  comment is corrected — it parses one page, not "a complete response".

**The interval boundary is a separate, unresolved question.** The
application's interval is inclusive at both ends
(`reconciliation/claim.rs:20`), while the channel is sent `to` as
midnight at the start of the final day (`adapters/tinkoff.rs:63,297`),
and the contract calls it only the end of the period. Following every
page of the wrong interval is still truncation. This design does **not**
fix it; it is filed (§13), because guessing the boundary is the same
error class as guessing a date.

## 10. Securities transfers

`INPUT_SECURITIES` and `OUTPUT_SECURITIES` are transfers of securities
(`operations.proto:306,320`); the dictionary maps them to cash
`deposit`/`withdrawal` (`dictionary_seed.rs:51,56`) and the adapter
builds cash movements. Money that never moved is recorded.

Removing them from the seed is **not** sufficient: the seed is explicitly
not the source of truth after first insertion
(`dictionary_seed.rs:9-11`) and storage inserts with
`ON CONFLICT DO NOTHING` (`broker_operation_kinds.rs:90`), so existing
installations keep the old mapping. The refusal therefore belongs in the
adapter, where these kinds are refused with a named reason regardless of
what the dictionary says, and a data migration corrects existing rows.

This is a loss of coverage and a gain in truth; the alternative is
fabricated cash, which §4.9 does not allow. A securities-transfer fact in
the journal model is filed separately (§13) — the existing corporate
action variants are redemption, amortisation and conversion only
(`event/corporate_action.rs:22`), so it cannot be smuggled in here.

## 11. Dating, and what is deliberately not changed

Trades are dated `trade` from `trades[].at`.

**Non-trading operations keep today's dating.** This is a deliberate
refusal to improve them. The single `date` is called *«Дата поручения»*;
moving a coupon to `cash_posted` would feed it into payment
reconciliation, which reads exactly `cash_posted` then `paid` and
deliberately refuses `trade` and `settled` because substituting one
"would silently move the fact to another day (§4.9), and the one-sided
matching window would accept or reject it based on an unrelated date"
(`projection/income.rs:93-99`). Today those coupons sit in `trade` and
are therefore invisible to that reconciliation — conservative and wrong
in name. Relabelling them would make them visible on a date the source
never called a posting date: wrong in substance, which is worse.

`settled` stays `None` everywhere: the contract has no settlement date
(`iaam-d8b.22`, `iaam-d8b.14`).

The fee's own cash movement stays unposted. Summing every `payment` in
the recorded response gives `199729.734935` against the broker's reported
`199729.73` (`tinkoff-portfolio.json`), which proves a settlement rule
exists and does not reveal it. `CalcMoney` documents that a calculated
value becomes posted only through a confirmed source fact, never by
rounding (`money.rs:254-258`).

## 12. What the earlier revisions got wrong

Recorded because the errors share one shape — asserting behaviour without
executing the path that would prove it.

Revision 1: claimed `PossibleDuplicate` reached the owner (it did not);
proposed dropping `fee_minor`, silently removing the commission from
basis and proceeds; collapsed absent and zero accrued interest into
unknown, violating the §4.9 rule it cited; claimed the loss of accrued
interest broke acquisition basis (it is excluded from basis by design);
proposed dating coupons `cash_posted`; gated state only for trades;
discarded cancelled orders wholesale; attributed to the contract words it
does not contain; missed pagination, custody, securities transfers and
intraday ordering entirely.

Revision 2: refused to reconcile `payment`, which arithmetic shows is
exactly the sum over trades; kept the commission in `fee_minor`, which
would post cash the source never reported; proposed scoping identity in
the store alone, which cannot work because dedup runs before the store
and `KnownRecord` carried no account; claimed a `/1 → /2` re-sync would
be uniformly `Fresh` (single-fill orders can match on fingerprint);
deferred the intraday-order mechanism to the plan when it was a design
decision.

## 13. Follow-up beads

- The interval boundary sent to the channel versus the inclusive
  application interval (§9).
- A securities-transfer fact in the journal model (§10).
- Establish what the channel's `date` means per operation family, from a
  live response (§11).
- The fee's settlement rule, from a live sequence with balances around
  one fee (§11, recorded on `iaam-7xe8`).
- Replace the hand-built multi-trade and two-page fixtures with recorded
  ones.
- Confirm against a live **bond** purchase whether `payment` includes
  `accrued_int` (§5). Every operation in the recorded response is a share
  with zero accrued interest, so the money-completeness check is verified
  only in its degenerate form.

## 14. Testing

`tests/fixtures/api/` is a policy directory
(`scripts/check-diff-lint.sh:80`): fixture changes go in their own commit
with `POLICY_CHANGE_APPROVED=1` and the `policy-change` label.

Two fixtures are added — a multi-trade order and a two-page response —
both **hand-built from the contract**. The fixture files and the beads
must say so: they prove the code matches the documented contract, not
that the broker behaves this way.

1. A cancelled order with fills produces facts for its fills.
2. A cancelled order with no fills produces neither facts nor quarantine.
3. An unrecognised state quarantines, quoting the value — on a **coupon**
   row as well as a trade.
4. An in-progress order with two trades produces two facts.
5. A two-day order produces two facts with two different trade dates.
6. `Σ quantity ≠ quantity_done` quarantines with both numbers.
7. `Σ quantity × price ≠ payment` quarantines with both amounts.
8. Each fact carries its own escaped `{operation_id}#{num}` identity, and
   a component containing `#` or `/` does not collide with another key.
9. Two fills of one order keep the exchange's order when the response
   returns them reversed.
10. The commission is allocated across trades, sums back to the order
    amount, and the remainder lands by `trade.num` — proven by a
    non-divisible case fed in both response orders.
11. A known zero accrued interest is `Some(0)`; absent is unknown;
    multi-day trades leave it unknown and the lifetime metric refuses.
12. A trade's custody is the `position_uid`; a row without one is
    refused.
13. `INPUT_SECURITIES` is refused with a reason naming securities, on an
    installation whose dictionary still maps it to `deposit`.
14. A two-page response returns both pages; a repeated cursor refuses;
    the second request repeats `from`, `to` and the limit.

Gates: `make check`, and `make mutants-diff BASE=main` for the touched
modules.

## 15. Acceptance criteria for `iaam-d8b.23`

- An order in an unrecognised or unnamed state produces no fact and is
  refused with the value quoted — for every operation family.
- Executions are never discarded because of the order's final state.
- Quantity comes from the trade; both completeness checks hold.
- The event date comes from the trade; an order with no trades is never
  dated by the order timestamp.
- Each trade is its own fact with its own escaped, account-scoped
  identity, and several fills keep the exchange's order.
- The commission reaches the lot basis and never the cash leg for this
  channel.
- A known zero accrued interest is recorded as zero, not as unknown, and
  it is never split across days.
- Custody comes from `position_uid`.
- A securities transfer is not recorded as cash, on existing
  installations as well as new ones.
- Pagination is followed; an incomplete interval is refused, not
  truncated.
- The re-import consequence for facts recorded under `/1` is stated in
  the epic's close reason.
