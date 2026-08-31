# T2: a per-row refusal is a row's verdict, not the response's

Bead: iaam-bl07. Epic: iaam-40vm.

## The defect, and the half of it the bead does not name

`adapt_operations` propagates the error of `operation_to_submitted` and
`trade_operations` with `?` (`crates/iaam-app/src/adapters/tinkoff.rs:204-206`),
so one row the adapter cannot build a fact from ends the whole response: no
row of that batch is accepted, and none is quarantined either. The rows that
take that path are `Transfer`, `BondAmortisation`, `BondRedemption`,
`SecuritiesTransferIn`, `SecuritiesTransferOut` and `Other(kind)`
(`:308-346`), and each returns a reason that is true of one row — "transfer
does not contain a recipient account", "the fact is entered via the journal
endpoint". An account holding one transfer cannot synchronise at all.

The same defect sits one storey up. `sync_broker` calls `normalize` and maps
its `Rejection` into `AppError::Invalid` through `?`
(`crates/iaam-app/src/scenarios/sync.rs:131-142`), so a row the adapter
**accepted** and normalisation refused also ends the whole synchronisation.

And there is a half the bead does not name, found while designing this task.
**A quarantined row reaches the owner nowhere.** Its only use anywhere in the
system is the emptiness test at `sync.rs:176`; `SyncOutcome::recorded` is
built solely from `parsed.accepted`, and no DTO carries the quarantine list.
So today the *only* way a refusal reaches the owner is by aborting the
synchronisation with its text.

That reframes the task. Making the adapter quarantine and continue, on its
own, would convert a loud refusal into silence: the sync would succeed, the
owner would be told nothing, and a transfer or an amortisation would be
missing from the journal with nothing saying so. Silent incompleteness is a
worse failure than a loud one, so the two halves ship together or not at all.

## The answer

### A refusal of a row is typed apart from a defect of the adapter

Not every error `operation_to_submitted` returns is a property of the row.
Two arms are unreachable by construction — the `Buy | Sell` guard at `:260-263`,
which the caller's own branching excludes, and the income-kind mismatch at
`:276`, whose comment already says it must fail loudly rather than
substitute a dividend. Quarantining those would hide a defect of our own code
behind a row-shaped excuse.

```rust
/// Why one row produced no fact.
enum RowRefusal {
    /// A property of the row: it was read, and no fact can be built from it.
    /// The batch continues and the owner is told.
    Row(String),
    /// The adapter reached a state its own branching should have excluded.
    /// This is our defect, and it must stay loud.
    Adapter(&'static str),
}
```

`operation_to_submitted` and `trade_operations` return
`Result<_, RowRefusal>`. `adapt_operations` quarantines `Row` and continues;
`Adapter` becomes the `BrokerError` it is today and still ends the response.

### A quarantined row becomes a verdict

`Verdict` gains

```rust
/// The row was read and no fact was recorded from it. The reason names
/// what is missing, not that something went wrong.
Quarantined { reason: String },
```

with code `"quarantined"`, beside the existing variants
(`crates/iaam-ingest/src/verdict.rs:26-59`), and the corresponding arm in
`VerdictDto` (`crates/iaam-server/src/dto.rs:696-704`). `sync_broker` emits
one such verdict per entry of `parsed.quarantined`, appended after the
verdicts of the accepted rows.

`Verdict::Unsupported` is deliberately not reused: its documented meaning is
that the monetary effect **is** preserved while the economic interpretation is
not, and here nothing at all is recorded. `Verdict::Rejected` is not reused
either: it carries a structured `Rejection` of a row that could not be
**parsed**, and a refused amortisation parsed perfectly well.

Adding a variant is additive. `docs/irreversible-core.md` fixes the codes and
the recording of `Verdict`, which forbids changing what an existing code
means; it does not forbid a new one.

### A normalisation rejection stops the row, not the batch

In `sync_broker`, a `Rejection` from `normalize` becomes
`Verdict::Rejected { rejection }` for that row, and the loop continues. This
is the variant's documented purpose and it needs no new type.

## Order within the epic

T2 makes quarantined rows more common, and `sync.rs:176` still zeroes every
control assertion whenever any row is quarantined. That is T3's subject and
it is **not** touched here. Until T3 lands, an account with a transfer
synchronises and records its facts where today it cannot synchronise at all,
but still receives no control assertion. That is a strict improvement and a
stated one.

A hand-off T3 must not miss: after this task a row can also be refused at the
normalisation layer, where `parsed.quarantined` never sees it. T3's coverage
rule must count refusals from both layers, not from `parsed.quarantined`
alone.

## Acceptance

Proven rather than asserted:

1. A response whose rows include one `Transfer` and several ordinary
   operations records every ordinary operation and returns a
   `Verdict::Quarantined` naming the transfer's reason — where today the call
   returns an error and records nothing.
2. The same for `BondAmortisation` and for `Other(kind)`, so the answer is a
   property of the path and not of one kind.
3. A row that normalisation refuses yields `Verdict::Rejected` carrying the
   rejection's field, and every other row of the same response is still
   recorded.
4. An adapter defect still ends the response: a direct call to
   `operation_to_submitted` with `Buy` returns `RowRefusal::Adapter`, and
   `adapt_operations` turns it into an error rather than a quarantine row.
5. The quarantine reasons that already exist — order state, securities
   transfer, trade-row checks — keep reaching the owner through the same new
   verdict, so the paths T1 of the previous epic added are not a second,
   silent class.

## Out of scope

`sync.rs:176` and `has_out_of_interval_trade` (T3). The wording of the
existing refusal reasons: they are already written for the owner, and
rewording them here would bury the change under a diff of prose.
