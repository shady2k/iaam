# T4: the channel classifies its refusals and records the gap

Bead: iaam-ep05. Epic: iaam-40vm. Core half: iaam-dbvu (T3).

## What is missing

T3 built the fact and the rule. Nothing writes an `ImportCoverageGap`, and
`sync_broker` still returns `assertions: 0` whenever a row was quarantined, so
the rule is unobservable and the defect the epic exists for is still live.

## The refusals, and what each cannot confirm

A refused row cannot be recorded, but it is not therefore unknown: in almost
every case the channel told us what kind it was and we refused for a named
missing field. That is the whole basis of this task — a refused commission
cannot make a position quantity unprovable.

The classification, verified against the code rather than assumed:

| Refusal | Cannot confirm |
|---|---|
| `Transfer` | `Cash` |
| `BondAmortisation` | `Cash` |
| `BondRedemption` | `Cash`, `Positions` |
| `SecuritiesTransferIn` / `Out` | `Positions`, `TaxBasis` |
| order state, trade-row check | `Cash`, `Positions` |
| `Other(kind)` | all four |
| a parse rejection with a known `source_kind` | as for that kind |
| a parse rejection with an empty `source_kind` | all four |
| a normalisation rejection | as for the operation's kind |

Why each is what it is:

- **`BondAmortisation` does not touch `Positions`.** Amortisation reduces the
  outstanding principal and the security count is unchanged
  (`crates/iaam-broker/src/operation_kind.rs`, and the corporate-action model);
  its validated event shape carries a principal leg and **no** security leg
  (`crates/iaam-core/src/event/mod.rs`). It does move lot cost basis, but
  reconciliation's `TaxBasis` is constrained only by `TaxWithheldTotal`
  (`crates/iaam-core/src/reconciliation/claim.rs:121`), not by cost basis, so
  naming `TaxBasis` here would claim an effect the dimension does not have.
- **`BondRedemption` does touch `Positions`.** It disposes the whole lot
  (`crates/iaam-core/src/projection/lots.rs`) and its shape carries a negative
  security leg.
- **A securities transfer names `TaxBasis` conservatively.** The kind says
  securities moved to or from another depository; it does not say whether the
  move was custody-only inside one account, which preserves basis, or across
  accounts, which requires basis migration. Lot keys ignore custody, so the
  kind alone cannot separate the two.
- **Neither `Income` nor `TaxBasis` can currently affect a T-Invest control
  assertion**, because that channel's portfolio yields only `CashBalance` and
  `PositionQuantity` (`crates/iaam-broker/src/tinkoff/parse.rs:209-260`).
  Classifying them is still right — the fact outlives this channel — but no
  test may assert an effect that cannot exist.

The motivating case works only because of a detail worth stating: a
**field-level** parse failure keeps `source_kind`
(`crates/iaam-broker/src/tinkoff/parse.rs:280-330`), so the nano-precision
commission is still classifiable as `Cash`. A **whole-row** deserialisation
failure sets `source_kind` to an empty string (`:359-367`) and must taint
everything.

## The changes

**`Quarantined` carries the classification.** It gains
`dimensions: BTreeSet<Dimension>` beside `raw` and `reason`
(`crates/iaam-app/src/ports.rs`). The reason is prose for the owner and cannot
be the rule's input, and reparsing `raw` downstream would put channel
knowledge in the scenario.

**`adapt_operations` classifies at each refusal it already makes.** Every
`quarantined.push` site gains the dimension set, and `RowRefusal::Row` grows
from a bare reason to a reason with its dimensions. `RowRefusal::Adapter` does
not: it aborts, and an aborted response records nothing.

**`sync_broker` stops suppressing.** The `assertions: 0` early return loses its
quarantine half. Refusals are counted from **both** layers — the adapter's
list and the normalisation rejections introduced by iaam-bl07, which
`parsed.quarantined` never sees — and their dimension sets are unioned. When
the union is non-empty, one `ImportCoverageGap` is appended for the account and
interval, carrying that union and the refusal count.

The gap event is built like `assertion_event`: no legs, `CashPostedDate(to)`,
`EffectiveOrder` at `to`, and provenance from the same channel. **The
provenance is not decoration** — T3's rule correlates by source and parser
version, so a gap recorded through a different channel identity applies to
nothing. Its idempotency key follows the assertion's shape so a repeated sync
of the same interval does not append a second gap.

## What stays as it is

`has_out_of_interval_trade` keeps its early return. It is not a refusal — the
fact was recorded — and folding it into the same mechanism would stretch a
variant documented as "an import attempt refused rows" over something else.
Whether it should become a gap of its own is a separate question, filed as
iaam-l73j, not decided by a task that would only be passing through.

## Acceptance

Proven end to end through `sync_broker`:

1. A response containing one refused commission and a portfolio records the
   control assertions — where today it records none — and appends one
   `ImportCoverageGap` naming `Cash` only.
2. In that same journal, the position assertion still reaches
   `AcceptedIndependent` against an independent channel, and the cash one does
   not. This is the defect the epic exists for and it must fail if the union
   is written as "all dimensions".
3. A row refused by normalisation contributes its dimensions to the gap, so
   the layer iaam-bl07 added is not a silent hole.
4. A response with no refusals appends no gap at all: `ImportCoverageGap` is
   absent from the journal, not present with an empty set, which
   `validate_structure` would refuse anyway.
5. A whole-row parse failure, whose `source_kind` is empty, taints all four
   dimensions.
6. Synchronising the same interval twice appends one gap, not two.
7. The gap's provenance carries the channel's source and parser version, so
   T3's correlation finds it. A test that asserts the gap exists but never
   asserts it applies proves nothing.

## Out of scope

The wording of existing refusal reasons. `has_out_of_interval_trade`. The
pre-existing coarseness of `confirmed_dimensions`, which compares channels by
dimension and not by currency or instrument
(`crates/iaam-core/src/reconciliation/mod.rs`), so a gap is dimension-wide
rather than claim-key-wide.
