# T1: the portfolio is not the closing balance of an interval it does not describe

Bead: iaam-9zur. Epic: iaam-40vm.

## The defect

`BrokerChannel::fetch_portfolio` promises control values **as at a date**
(`crates/iaam-app/src/ports.rs:577-585`). The T-Invest adapter takes that
date as `_at` and discards it (`crates/iaam-app/src/adapters/tinkoff.rs:86-97`),
calling `get_portfolio`, which the contract defines as the **current**
portfolio: `PortfolioRequest` carries `account_id` and an optional display
currency, and no date at all
(`docs/api/tinkoff-invest/operations.proto:96-104`).

`sync_broker` then records those claims through `assertion_event` with
`AssertionPeriod { from, to }`, dated `CashPostedDate(to)` and ordered at
`to` (`crates/iaam-app/src/scenarios/sync.rs:168-184`, `:313-346`). The
claims themselves carry no date of their own: `parse_portfolio` sets
`at: BalancePoint::Closing` (`crates/iaam-broker/src/tinkoff/parse.rs:227-231`),
a point relative to the period. The period is therefore the whole
statement, and for any sync whose `to` is not the current date the
statement is false.

The journal is append-only. A false `ControlAssertion` cannot be edited,
only reversed. Reconciliation then compares it against the projection for
that interval and produces either a discrepancy that sends the owner to
investigate an artefact of our own request, or — worse — a match that
raises the interval to `accepted_independent` on evidence about a
different day.

The date is not merely unpassed. The gateway has no historical portfolio
method, so the port's signature promises something this channel cannot
deliver. Plumbing the argument through is not available as a fix.

## The answer

The port stops promising what a channel may be unable to give, and says
instead what its answer describes.

```rust
/// What a channel's portfolio answer describes.
///
/// A channel that can only report its present holdings must say so rather
/// than accept a date it will ignore: the caller records the answer as a
/// fact, and a fact dated by the question rather than by the answer is
/// false.
pub enum PortfolioAsOf {
    /// The channel answered for the date that was requested.
    Requested,
    /// The channel reports its current portfolio, whatever was requested.
    Current,
}

pub struct PortfolioSnapshot {
    pub as_of: PortfolioAsOf,
    pub claims: Vec<ControlClaim>,
}

async fn fetch_portfolio(
    &self,
    account: AccountId,
    at: Date,
) -> Result<PortfolioSnapshot, BrokerError>;
```

The T-Invest adapter returns `PortfolioAsOf::Current`. It does not gain a
clock: the adapter states a property of its own contract, and the scenario,
which already holds `services.clock` (`crates/iaam-app/src/lib.rs:57`),
resolves that property to a date.

`sync_broker` resolves the snapshot's date and compares it with the
requested interval:

- `Requested` — the claims describe `to`, and the assertion is recorded
  over `[from, to]` exactly as today. This is the unchanged path.
- `Current` and the clock's date lies within `[from, to]` — the claims
  describe a day the interval covers, and the assertion is recorded over
  `[from, to]`. This is the ordinary sync, where `to` is today.
- `Current` and the clock's date lies outside `[from, to]` — no assertion
  is recorded, and the outcome names why.

The third branch is a refusal, not silence. `SyncOutcome` gains

```rust
/// Why no control assertion was recorded, when none was.
pub assertions_withheld: Option<AssertionsWithheld>,
```

with one variant for now, `PortfolioDescribesAnotherDay { as_of: Date }`,
carrying a `code()` in the manner of `NotComparable::code`
(`crates/iaam-core/src/reconciliation/check.rs:50-58`). `SyncOutcomeDto`
(`crates/iaam-server/src/dto.rs:3174-3190`) gains the corresponding
optional field. The addition is additive: an absent field means nothing
was withheld.

## Why not the alternatives

**Record the claims over `[today, today]`.** They would be true, and this
was the first answer. It is rejected because a March sync would then
append an assertion about today against a journal that holds nothing after
March, producing a discrepancy that is an artefact of the request rather
than a fact about the account. A true statement recorded where it will be
read as an answer to a different question is not an improvement on a false
one.

**Refuse the whole sync when `at` cannot be honoured.** The operations
import is unaffected by the portfolio's limitation, and refusing it would
discard facts the channel did deliver.

**Leave the port and guard inside `sync_broker`.** That places knowledge of
one channel's contract in the scenario, where the next channel's different
limitation would have to be added beside it.

## Acceptance

Proven rather than asserted:

1. A sync whose `to` is the clock's date records the portfolio assertions
   exactly as before — the count is unchanged and `assertions_withheld` is
   `None`.
2. A sync whose interval ends before the clock's date records **no**
   assertion, records every operation fact it would otherwise have
   recorded, and reports `PortfolioDescribesAnotherDay` naming the date.
3. A channel returning `PortfolioAsOf::Requested` records the assertion
   over `[from, to]` regardless of the clock, so the honest historical
   channel that does not exist yet is not penalised for the T-Invest
   limitation.
4. The T-Invest adapter returns `PortfolioAsOf::Current` — a test that
   fails if a later change silently claims the requested date.

Existing test doubles that implement `fetch_portfolio`
(`crates/iaam-app/tests/sync.rs:51`, `crates/iaam-server/tests/contract.rs:90`
and `:138`) are updated to the new return type; each must state which
variant it returns rather than defaulting to one.

## Out of scope

`iaam-bl07` (T2) and `iaam-dbvu` (T3) of the same epic. The early return
at `sync.rs:159-166` that zeroes assertions on any quarantined row stays
as it is: T3 replaces it, and moving it here would entangle two
independently reviewable changes.
