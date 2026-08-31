# T1: order state becomes a typed contract — design

Bead: `iaam-woil`. Parent epic: `iaam-zn38`. Date: 2026-08-31.

Scope of the parent design: `.internal/specs/2026-08-31-tinkoff-execution-facts-design.md`
(revision 3), §4 and §5. This document decides only what that design left
as an open question, and nothing else: **no money, no dates, no expansion
of one order into several facts.** Those are T2 and T3.

Reviewed adversarially by codex on 2026-08-31. Four findings, all
accepted: the gate's position relative to kind resolution (§3.2), the
false claim that `trades_info` is unread (§3.4), the several families
that produce an error rather than a fact on the executed path (§3.2), and
acceptance criterion 4, which could pass while the partial-fill
overstatement stood — now §3.7 and a rewritten criterion.

## 1. Problem

`ChannelOperation.state` is parsed as a `String` (`tinkoff/parse.rs:95`,
filled at `:230`) and read by nobody: `grep -rn '\.state' crates/ --include=*.rs`
outside `parse.rs` and `client.rs` finds only unrelated types. The field
crosses the whole channel and is dropped.

Two consequences:

- Every order becomes a journal fact regardless of its state. A cancelled
  order and an executed one are indistinguishable downstream.
- The value is a free string, so any future check on it is a wildcard
  `match` decided at runtime. An unknown state cannot break compilation,
  which is exactly the property `ChannelOperationKind::Other(String)`
  was introduced to preserve for kinds (`operation_kind.rs:64-73`).

Revision 2 of the parent design gated state inside the `Buy | Sell`
branch only, leaving coupons, fees, deposits and withdrawals unguarded.
That mistake is recorded in the parent's §12 and must not be repeated.

## 2. Contract

`docs/api/tinkoff-invest/operations.proto`, commit `3eaf23a`:

- `OPERATION_STATE_UNSPECIFIED = 0`
- `OPERATION_STATE_EXECUTED = 1` — *«Исполнена частично или полностью»*
- `OPERATION_STATE_CANCELED = 2`
- `OPERATION_STATE_PROGRESS = 3`

The wire form in `GetOperationsByCursor` JSON is the enum name, e.g.
`"OPERATION_STATE_EXECUTED"`.

`EXECUTED` therefore never proves that the ordered quantity was the
executed quantity. T1 does not fix that — it cannot without
`trades_info`, which T2 reads. T1 records the gap and refuses everything
it cannot justify.

## 3. Decision

### 3.1 The state becomes a typed enum

In `iaam-broker/src/tinkoff/parse.rs`, next to `ChannelOperation`:

```rust
/// State the channel reported for the order.
///
/// Typed rather than a string so that an unrecognised value is a named
/// variant carrying the raw text, and adding a member breaks compilation
/// wherever the state is consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOrderState {
    /// `OPERATION_STATE_EXECUTED`: executed in part or in full.
    Executed,
    /// `OPERATION_STATE_CANCELED`.
    Cancelled,
    /// `OPERATION_STATE_PROGRESS`.
    InProgress,
    /// `OPERATION_STATE_UNSPECIFIED`: the channel did not name a state.
    Unspecified,
    /// A value absent from the contract, carrying the raw text.
    Unrecognised(String),
}
```

`ChannelOperation.state` changes from `String` to `ChannelOrderState`.

Mapping lives in a `match` on the four contract literals with a
`_ => Unrecognised(raw)` arm. This is deliberately **not** dictionary
data, unlike `source_kind`: the operation-kind set is open and belongs to
the broker (`operation_kind.rs:9-14`), whereas the state set is closed by
the contract and an addition to it is a contract change we must see.

A missing `state` field stays a whole-row rejection, as today
(`required_or_reject` at `:230`). Absent is not `Unspecified`: the
contract distinguishes "the channel said nothing" from "the channel said
unspecified", and collapsing them would lose which one happened.

`rejected_operation` fills `state: ChannelOrderState::Unspecified` in
place of today's `String::new()` — the row is already rejected, and the
field carries no meaning there.

### 3.2 The state gates every family

In `iaam-app/src/adapters/tinkoff.rs`, the gate runs in
`adapt_operations`, **after** `dictionary.kind_of` and **before**
`operation_to_submitted` (`:123-124`). Both halves matter and the first
is not the obvious one:

- after the kind, because §3.3 and §3.4 give a family-specific reason,
  and the family does not exist before `kind_of` resolves it —
  `source_kind` is an opaque broker string until then
  (`operation_kind.rs:9-14`);
- before `operation_to_submitted`, because that function is where
  revision 2 put the check, per family, which is how coupons and fees
  ended up unguarded. Resolving the kind is not the same as dispatching
  on it: the gate reads the kind, it does not branch into a family's
  construction.

Only `Executed` lets a row reach `operation_to_submitted`. Every other
state stops there, produces no fact, and yields one quarantine row.

| State | Buy / Sell | Every other family |
|---|---|---|
| `Executed` | unchanged, as today | unchanged, as today |
| `Cancelled` | quarantine (transitional, §3.4) | quarantine — refused for lack of evidence |
| `InProgress` | quarantine (transitional, §3.4) | quarantine — refused for lack of evidence |
| `Unspecified` | quarantine | quarantine |
| `Unrecognised(v)` | quarantine, reason quotes `v` | quarantine, reason quotes `v` |

"Unchanged, as today" is deliberately not "produces a fact": on the
executed path several families do not produce one now. `Transfer`,
`BondAmortisation`, `BondRedemption` and `Other` return an error
(`adapters/tinkoff.rs:186-212`), and T1 leaves that exactly as it is.

The behaviour is uniform; the **justification** is not, and the two must
not be confused, because T2 changes one column and not the other.

### 3.3 Why a non-executed non-trade family is refused, not dropped

This is the question the bead asked and the reason the answer differs by
family.

A trade has independent execution evidence: `trades_info` says what was
actually filled, whatever the order's final state. A coupon, a dividend,
a fee, a deposit and a withdrawal have none — the contract carries
`trades_info` on `OperationItem`, but nothing populates it for a payment,
and there is no second field that says whether the money moved. For those
families the state is the only evidence there is, and a state other than
`Executed` says the channel does not assert the money moved.

Recording such a row would assert a cash movement the source did not
confirm, into an append-only journal where the only correction is a
reversal (`docs/irreversible-core.md`). Dropping it silently would hide
from the owner that the channel reported something the system chose not
to record. Quarantine is neither: the row is not a fact and is not lost,
and the owner sees the raw JSON with a reason.

So: **refused for lack of evidence, permanently.** No later task in this
epic changes it, because no field exists that would.

### 3.4 Why a non-executed trade is refused *for now*

A cancelled or in-progress order may still have fills. Today the channel
records the whole ordered quantity as a trade, which is bug
`iaam-d8b.23`: the order says «buy 100», ten filled, and the journal
gets 100 for ever.

T1 does not read the execution quantities, so it cannot record the ten.
`trades_info` is not untouched today — `source_time_or_reject` takes the
first trade's timestamp from it (`parse.rs:228`, `:513-520`) — but the
quantities, the prices and every trade after the first are discarded, and
those are what a fill is made of. Saying "trades_info is not read" would
be false; what is not read is the execution itself.

T1 has three options and takes the third:

1. keep recording 100 — the bug, unacceptable;
2. drop the row silently — erases fills that happened;
3. quarantine — records nothing, loses nothing, tells the owner.

The quarantine reason must say the evidence is not read yet, not that the
row is unusable, so it is legible while T1 is the tip and obviously
obsolete once T2 lands:

```
order state OPERATION_STATE_CANCELED: this parser version does not read
execution quantities, so a partial fill cannot be distinguished from a
non-fill
```

**T2 replaces this branch.** After T2, a cancelled order expands its
fills, and a cancelled order with no fills produces neither fact nor
quarantine (parent design §14, case 2). T2's acceptance must include
removing this transitional quarantine, and this section is the record
that it is transitional. Tests written here for cases 3.4 are expected to
be rewritten by T2; they are not a contract.

### 3.5 Quarantine is per row, not per batch

`adapt_operations` currently propagates `operation_to_submitted`'s error
with `?` (`adapters/tinkoff.rs:124`), so one unsupported row aborts the
whole sync. The state gate must not inherit that: it pushes onto
`quarantined`, the same path an unparsable row already takes
(`:120-126`).

Changing the existing `?` for *kind* failures is **out of scope** — it
is a real defect, filed separately rather than fixed here, because it
changes the outcome of rows T1 does not touch.

### 3.6 Not changed by T1

- The parser version. `TINKOFF_PARSER_VERSION` stays `tinkoff-api/2`:
  T1 changes which rows become facts, not how a recorded fact is
  constructed, and the version identifies the construction. T2 bumps it.
- Dates, money, quantity, identity, custody, pagination.
- `client.rs:85`'s `state: Option<String>` request filter — that is a
  query parameter sent to the gateway, not a parsed response value.

### 3.7 What T1 knowingly leaves broken

`OPERATION_STATE_EXECUTED` is *«Исполнена частично или полностью»*
(`operations.proto:296`). The adapter builds the trade from the order's
own `quantity` (`adapters/tinkoff.rs:234-236`) and never looks at
`quantity_done` (`operations.proto:505`) or the trade list. So an order
for a hundred that filled ten, and settled in state `Executed`, still
records a hundred after T1.

That is `iaam-d8b.23` and T1 does **not** close it. Preserving today's
executed path preserves the overstatement, and this section exists so
that no acceptance criterion below can be read as claiming otherwise.

T1 does not quarantine executed trades to hide from this. Quarantining
every `Buy`/`Sell` row would refuse work the channel does report, undo
what `iaam-jdmc` delivered a commit ago — the recorded SBER purchase
reaching the journal — and do T2's job with T2's evidence unavailable.
The overstatement is a *quantity* defect; the quantity is T2's. Mixing
the two is how the monolithic design failed three reviews (parent §12).

The consequence is stated rather than mitigated: **facts recorded by the
executed path between T1 and T2 may overstate quantity and will need the
same re-import as facts recorded under `tinkoff-api/1`.** The epic's
close reason must carry it (parent §15, last criterion).

## 4. Acceptance criteria

1. `ChannelOperation.state` is `ChannelOrderState`; no `String` state
   survives in the channel type.
2. Each of the four contract literals parses to its variant; any other
   value parses to `Unrecognised` carrying the value verbatim.
3. A row with `state` absent is rejected as a whole row, as today.
4. An `Executed` row of every family reaches exactly the outcome it
   reaches today — a fact, or the same error, unchanged. This criterion
   is satisfied while `iaam-d8b.23` is still open: it asserts that T1
   changed nothing on the executed path, **not** that the executed path
   is correct (§3.7).
5. A `Cancelled`, `InProgress`, `Unspecified` or `Unrecognised` row
   produces no fact and one quarantine row, **for a coupon as well as
   for a trade**.
6. The quarantine reason names the state; for `Unrecognised` it quotes
   the raw value.
7. One quarantined row does not abort the batch: an accepted row in the
   same response is still accepted.

## 5. Tests

Unit tests, in the crates that own the code. No fixture files are added:
every case is expressible against the existing recorded response or a
minimal inline JSON row.

In `iaam-broker` (parsing):

- each of the four literals maps to its variant;
- `"OPERATION_STATE_SOMETHING_NEW"` maps to `Unrecognised` with the value
  preserved verbatim;
- an absent `state` rejects the row.

In `iaam-app` (adaptation):

- an `Executed` buy still becomes a trade fact (the recorded SBER row);
- an `Executed` transfer still returns the same error it returns today —
  the state gate did not turn an error into a quarantine row;
- a `Cancelled` buy produces no fact and one quarantine row naming the
  state;
- a `Cancelled` **coupon** produces no fact and one quarantine row —
  the case revision 2 left unguarded;
- an `Unrecognised` coupon quarantines with the value quoted;
- a response holding one `Executed` and one `Cancelled` row yields one
  accepted and one quarantined, not an error.

## 6. Gates

`cargo check` and the two crates' own tests. The workspace gates
(`make check`, `make mutants-diff`) run once at the end of the epic, not
per task.
