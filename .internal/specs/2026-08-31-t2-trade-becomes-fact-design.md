# T2: one trade becomes one fact — design

Bead: `iaam-dz94`. Parent epic: `iaam-zn38`. Depends on `iaam-woil` (T1,
closed). Date: 2026-08-31.

Reviewed adversarially by codex on 2026-08-31. Three findings, all
accepted: §7 was wrong — identical fills are distinguishable through lot
identity, so an emission order is now declared; §6's boundary leaked, and
the price type, the rounding path and the sign rule are now stated; and
acceptance criterion 1 could pass with the gross taken from the order's
`payment`, so §12 gained criteria 5b and 5c.

Parent design: `.internal/specs/2026-08-31-tinkoff-execution-facts-design.md`
(revision 3), §3, §4, §5. T1's design:
`.internal/specs/2026-08-31-t1-order-state-design.md`. This document
settles the four questions `iaam-dz94` names as blocking and nothing
else. Money — the completeness equation, commission allocation, accrued
interest — is T3 (`iaam-0gbe`), except for the one multiplication §6
hands to T2 and says why.

## 1. Problem

The channel records the **order**. `parse_operation` reads
`trades_info` for exactly one thing — the timestamp of the first trade,
as a `Time` (`parse.rs:553-566`) — and discards the rest:
`RawTrade` declares only `date` (`parse.rs:621-623`). `quantity_done`,
`quantity_rest` and `cancel_reason` are not declared at all.

So the fact records the ordered quantity. `OPERATION_STATE_EXECUTED` is
*«Исполнена частично или полностью»* (`operations.proto:296`): an order
for a hundred that filled ten is `Executed` and records a hundred. T1
gated the state and deliberately left this standing (T1 design §3.7).
This task closes it.

The contract's trade element (`operations.proto:521-528`):

```proto
message OperationItemTrade {
  string num = 1;                      // Номер сделки
  google.protobuf.Timestamp date = 6;  // Дата сделки
  int64 quantity = 11;                 // Количество в единицах
  MoneyValue price = 16;               // Цена
  MoneyValue yield = 21;
  Quotation yield_relative = 22;
}
```

Note the field is `date`, not `at`: `iaam-dz94`'s wording predates
reading the contract. The parsed struct may still call it `at`, since it
carries a full timestamp, but the JSON key is `date`.

## 2. Decision

One trading row expands into one `SubmittedOperation` per element of
`trades_info.trades`. Quantity, price and moment come from the trade.
An order with no trades produces no fact.

Every other family stays one row, one fact, untouched.

## 3. Parsing

```rust
/// One execution of a trading order.
pub struct ChannelTrade {
    pub num: String,
    pub at: OffsetDateTime,
    pub quantity: Quantity,
    /// Exact, not posted: see §6.
    pub price: CalcMoney,
}
```

`price` is `CalcMoney`, not `ChannelMoney`, and that is a decision rather
than a detail. `parse_money` refuses any `nano` finer than the currency's
minor units (`parse.rs:387-396`, `ParseError::NonRepresentableFraction`),
while the contract puts no such limit on a trade price
(`operations.proto:521-528`). A bond quoted to four decimal places would
be refused at parse time — a row rejected for a precision the source is
entitled to use. `CalcMoney` is the type this codebase already uses for
an exact source value that is not itself posted; the commission took the
same route in `iaam-jdmc`.

`ChannelOperation` gains `trades: Vec<ChannelTrade>`, `quantity_done:
Quantity`, `quantity_rest: Quantity` and `cancel_reason: Option<String>`.
`source_time` stays, and is now derived from `trades[0]` rather than
parsed separately, so one code path reads the trade list.

Rules, from the parent design §4:

- a trade with a missing or unparsable field rejects the **whole row**.
  A partially parsed list would record fewer executions than happened,
  and the row is the unit the owner can act on;
- `trades_info` absent or empty gives an empty vector, not a rejection: a
  cancelled order legitimately has none;
- `quantity_done` and `quantity_rest` are proto3 scalars, so absent and
  zero are indistinguishable on the wire. Both are parsed as **present
  with value zero**, never as "not reported". §5 depends on this and
  states the consequence.

**A trade element whose price carries an empty currency is a
placeholder, not an execution, and yields no `ChannelTrade`.** The
recorded response puts one on every `OPERATION_TYPE_INPUT` row —
`quantity: "0"`, `price: {"currency": "", "units": "0", "nano": 0}`
(`tests/fixtures/api/tinkoff-operations.json`) — and an empty currency is
not a currency: `parse_currency` refuses it. Parsing those strictly would
reject every deposit, which works today.

This rule is about the **shape of the element**, not about the operation
family, and that distinction is the point. The parser must not decide it
by operation type: the mapping from a broker's code to a meaning lives in
the dictionary, in data, and not in a `match`
(`iaam-broker/src/operation_kind.rs:9-14`). A parser that hard-codes
`OPERATION_TYPE_BUY` would silently stop reading fills on an installation
whose dictionary names a different code, or after the broker adds one —
the original bug restored, with no message. Expansion into facts stays
keyed on the dictionary-resolved kind, in the adapter, where the kind
exists.

Strictness therefore applies to elements that claim to be executions: an
element with a real currency and a missing or unparsable field rejects
the whole row, as above.

## 4. Identity, and duplicate `num`

Per trade:

```
source_operation_id = {esc(operation_id)}#{esc(trade.num)}
idempotency_key     = {esc(broker_account_id)}/{esc(operation_id)}#{esc(trade.num)}
```

`esc` is percent-encoding over the UTF-8 bytes of the component,
escaping `%` first and then `#` and `/`, each as `%XX` with uppercase
hex. Escaping `%` first is what makes the joining injective: without it
a component containing the literal text `%23` and one containing `#`
would produce the same key.

`num` alone is not the key. In the recorded response `num` equals the
operation's own `id`, so its independent uniqueness is not demonstrated,
and `operations.proto:522` says only *«Номер сделки»*.

**Duplicate `num` within one order is refused, not resolved.** If two
elements of one `trades_info.trades` carry the same `num`, the whole row
is quarantined, with the value quoted and the count. The alternative —
disambiguating by position in the array — would make identity depend on
the order the channel returned, so a re-import in a different order would
produce different keys for the same fills, and the second import would
record every fill twice. Refusing keeps a silent identity collision
impossible; if the case ever occurs, the quarantine is the evidence that
it does, which is what we would need before designing anything better.

## 5. Completeness by quantity

`Σ trades[].quantity` must equal `quantity_done`. A mismatch quarantines
the row, with both numbers in the reason.

The money-completeness check — `Σ (quantity × price) + accrued_int`
against `payment` — is **T3's**, not this task's. T2 must not add it: it
needs the sign rule and the accrued-interest reading that `iaam-0gbe`
exists to settle.

Consequence of the proto3 rule in §3, stated rather than hidden: a
gateway that omits `quantityDone` on an order that did fill sends
`quantity_done = 0` against a non-empty trade list, and the row
quarantines. That is the correct direction — the alternative is
recording fills whose completeness nothing checked — but it means a
gateway change can quarantine every trading row at once. The reason text
must therefore name both numbers, so that failure mode is legible from
one quarantined row rather than needing the contract to diagnose.

## 6. The per-fill gross, and exactly where T3 begins

Building a `SubmittedOperation` at all requires a gross amount, and after
T2 it can no longer be the order's `payment`: that is the sum over fills.
So T2 owns the per-fill gross, and owning it means owning three rules,
none of which may be left implicit.

**Precision.** The price is kept exact as `CalcMoney` (§3) and multiplied
by the trade quantity as an exact decimal. Rounding the price to minor
units first and multiplying afterwards is forbidden: a price of
`12.3456` on ten units is `123.456`, and pre-rounding turns it into
`123.46` or `123.45` before the quantity has been applied.

**Rounding.** The exact product is converted with
`CalcMoney::rounded_minor` (`iaam-core/src/money.rs:288`), the codebase's
declared calculated-to-posted path, and no other rule is invented.
`basis_fee_money` already takes exactly this route
(`iaam-ingest/src/operation.rs:531-536`).

The exact product is then **discarded**: `EventKind::Trade` has a
`basis_fee_exact` but no `gross_exact`. Whether the exact per-fill gross
must be retained is T3's to decide, because only T3's reconciliation can
show whether the rounded parts sum back to the rounded aggregate, and
adding a core field is the kind of change `iaam-jdmc` did deliberately
and not as a side effect.

**Sign.** The product is a magnitude. `validate_trade` requires a
positive gross and a positive quantity (`iaam-core/src/event/mod.rs:394-397`),
and direction is carried by the `Buy`/`Sell` variant, never by the sign —
the rule `ChannelMoney::magnitude` already documents. The order's
`payment` is negative for a purchase and is not the source of the
per-fill gross, so no sign is propagated from it.

**What stays T3's**, unchanged by the above: the money-completeness
equation and its sign rule across the whole order, commission allocation
across fills, accrued interest, and proving that rounded parts sum back
to the rounded aggregate. T2 produces per-fill amounts; T3 reconciles
them. T2 must not round-and-distribute anything.

This is the one boundary between the two tasks that is a shared edge
rather than a clean cut, and it is named here so that T3's review starts
from it rather than discovering it.

## 7. Ordering: fills are emitted in a declared order

`compare_for_replay` (`iaam-core/src/event/mod.rs:168-192`) orders by
date, then `source_time`, then `raw_hash`, then `sequence`, then `id`.
The fingerprint covers `{v, account, kind, dates}` only
(`iaam-ingest/src/dedup.rs:350-355`) — not `source_time`, not `num`.

So two fills of one order at the same timestamp, with the same quantity
and the same price, are equal on every term down to `sequence`, and
`sequence` is assigned by storage in insertion order.

**Insertion order is therefore T2's output order, and T2 declares it:**
fills are emitted sorted by `(at, num)`, `num` compared as bytes. A
response returning the same two fills in the opposite order produces the
same sequence of facts.

The first instinct — that no rule is needed because two identical fills
are economically interchangeable — is wrong, and the reason is worth
recording. Each purchase creates a lot whose id is its acquisition
event's id (`iaam-core/src/rules/lot_disposal.rs:34-40`), and FIFO
disposal walks the lot vector in replay order and reports **which lot**
it consumed (`lot_disposal.rs:155-165`). The amounts do not change when
two identical lots swap, but the lot identity in the disposal record
does, and that record is what an owner reads to see which acquisition a
sale was matched against. A projection whose audit trail depends on the
order a gateway happened to return rows in is not reproducible.

The sort is a **local determinism rule and asserts nothing about
execution chronology.** Where the source names an order, `at` carries it;
`num` is the tie-break only when `at` cannot distinguish, and it is
chosen over array position because array position is the thing that
varies between responses.

It is done in T2's own emission rather than in `compare_for_replay`
because the core cannot see `num`: putting it into the ordering would
mean putting it into the fingerprint, which is a cross-channel format
(§8).

## 8. Two identical fills arrive as `PossibleDuplicate`

A consequence of the same shared fingerprint, and it reaches the owner,
so it is a design decision and not an implementation detail.

`dedup::assess` (`iaam-ingest/src/dedup.rs:209-234`) tries the key first;
`{op}#{num}` differs per fill, so no `Duplicate`. It then looks for a
known record with the same fingerprint from a different document. The API
channel has `document: None`, and `same_document` is deliberately false
when either side is absent (`dedup.rs:243-248`). `sync_broker` pushes each
recorded event onto `known` inside the loop
(`iaam-app/src/scenarios/sync.rs:126`), so the second identical fill
is compared against the first and comes back
`PossibleDuplicate { level: Probabilistic }`.

Nothing is lost — the event is appended before the verdict is chosen
(`sync.rs:113-122`) — but the owner is shown a possible duplicate for every
order with two identical fills, which is an ordinary market event.

T2 **accepts this and does not fix it.** The fix would be to include
`source_time` in the canonical form, and that is a change to the
fingerprint format shared by every channel: `CANONICAL_VERSION` would
have to change, and every stored fingerprint would stop matching, which
would make every re-import of every source look fresh. That is not a
change to make inside a task about one broker's trades.

Filed as `iaam-8j6h`. Until then the design states the noise, and §13
requires a test asserting both fills are recorded — the property that
actually matters.

## 9. Dating and the timezone

`ChannelTrade.at` is an `OffsetDateTime`. The trade date and the source
time are both derived in **UTC**: `at.to_offset(UtcOffset::UTC)`, then
`.date()` and `.time()`.

UTC because the channel is already read that way and consistency is the
only thing available: `source_time_or_reject` already converts to UTC
(`parse.rs:563`), and the order `date` takes `.date()` of the parsed
RFC 3339 value (`parse.rs:549-551`). Choosing Europe/Moscow instead would
mean the trade date and the order date of the same response were read in
two zones, and it would need a timezone database this workspace does not
depend on.

**The failure mode, stated because it is real:** a fill executed between
00:00 and 03:00 Moscow time lands on the previous UTC day, and T-Bank
does run sessions in that window for some instruments. The evidence to
decide this — a live response containing a night-session fill — does not
exist in the repository, and choosing Moscow time from no evidence would
be inventing a rule rather than reading one. Filed as `iaam-m7hy`; until
it is answered the date is UTC and this paragraph is why.

## 10. What T2 changes in T1's work

T1 quarantined every non-`Executed` trading row as transitional (T1
design §3.4). T2 removes that branch for trades:

| State | After T2 |
|---|---|
| `Executed` | expand its trades |
| `InProgress` | expand its trades |
| `Cancelled` | expand its trades; none present is not an anomaly and produces neither fact nor quarantine |
| `Unspecified` | unchanged from T1: no facts, quarantined |
| `Unrecognised(v)` | unchanged from T1: no facts, quarantined, reason quotes `v` |

Non-trade families keep T1's behaviour exactly, including the permanent
refusal of a non-`Executed` coupon, dividend, fee, deposit or withdrawal
(T1 design §3.3). Only the trade column moves.

T1's tests `a_cancelled_buy_is_quarantined_without_a_fact` and the
`order_state_reason` branch for `Buy | Sell` are expected to be rewritten
or deleted here. T1's design said so; this is that.

## 11. Parser version

`TINKOFF_PARSER_VERSION` becomes `tinkoff-api/3`. T2 changes how a
recorded fact is constructed, which is what the version identifies.

It then stays `/3` for the rest of the epic, on the condition that the
epic lands as one branch with no release between its tasks — no facts are
recorded in between, so a second bump would distinguish nothing. If any
task of this epic ships on its own, that task bumps again. The epic's
close reason must state which happened.

## 12. Acceptance criteria

1. A trading row with N trades produces N `SubmittedOperation`s; each
   carries its own trade's quantity and its own `trade.at` as date and
   source time.
2. A trading row with no trades produces no fact, and is never dated by
   the order timestamp.
3. `Σ trades[].quantity ≠ quantity_done` quarantines the row, with both
   numbers in the reason.
4. Two elements with the same `num` in one order quarantine the row, with
   the value quoted.
5. Each fact's `source_operation_id` and `idempotency_key` are the
   escaped composites of §4, and components containing `#`, `/` or `%`
   do not collide with any other component pair.
5b. Each fact's gross is the trade's own exact price multiplied by the
   trade's quantity and then rounded through `CalcMoney::rounded_minor`,
   as a positive magnitude — never the order's `payment`, never a price
   rounded before the multiplication, never a sign taken from `payment`.
   A price finer than the currency's minor units is carried through
   without refusing the row.
5c. Two fills that differ only in the order the response returned them
   produce the same facts in the same order, including the lot
   identities a later disposal reports.
6. A cancelled order with fills produces facts for its fills; a cancelled
   order with no fills produces neither fact nor quarantine.
7. Non-trade families behave exactly as T1 left them.
8. `TINKOFF_PARSER_VERSION` is `tinkoff-api/3`.

## 13. Tests

`tests/fixtures/api/` is a policy directory
(`scripts/check-diff-lint.sh:80-83`): a fixture change needs its own commit
with `POLICY_CHANGE_APPROVED=1` and the `policy-change` label, which an
agent may not grant itself. T2's cases are therefore written as inline
JSON in the crates' own test modules, as T1's were. The parent design's
hand-built multi-trade and two-page fixture files are left to whoever
obtains the owner's approval.

In `iaam-broker` (parsing):

- a two-trade order parses two `ChannelTrade`s with their own quantities,
  prices and timestamps;
- a trade missing `quantity`, `price`, `num` or `date` rejects the whole
  row, and the row still carries its raw JSON;
- absent and empty `trades_info` both give an empty vector and no
  rejection;
- absent `quantityDone` parses as zero, not as unknown.

In `iaam-app` (adaptation):

- an order with two trades produces two facts;
- an order whose two trades fall on different days produces two facts
  with two different trade dates;
- a cancelled order with fills produces facts for its fills;
- a cancelled order with no fills produces neither fact nor quarantine;
- `Σ quantity ≠ quantity_done` quarantines with both numbers;
- duplicate `num` quarantines with the value quoted;
- identity: `operation_id` containing `#` and `num` containing `/` produce
  keys that differ from the pair with the components swapped, and from a
  pair whose components contain the literal `%23`;
- two fills equal in time, quantity and price are **both** recorded;
- feeding one order's fills in the reversed response order produces an
  identical sequence of facts, and a subsequent sale reports the same
  consumed lot ids — the property §7 exists for;
- a trade price finer than the currency's minor units (a bond at four
  decimal places) does not reject the row, and its per-fill gross is the
  exact product rounded once, not the rounded price multiplied;
- a per-fill gross is positive for a sale as well as a purchase, with
  direction carried by the operation variant.

## 14. Gates

`cargo check` and the two crates' own tests, as in T1. The workspace
gates run once at the end of the epic.
