# 0013. A movement whose far side is the owner's and unnamed

Date: 2026-09-04 · Status: proposed · Beads: `iaam-cp94`, `iaam-fmih`, `iaam-tb5o`

## Context

A source can say three different things about the other side of a cash row, and
the journal had shapes for only two of them.

- **It names the party.** The directory may recognise the name as one of the
  owner's accounts, and the row becomes `EventKind::CashTransfer`, which carries
  **both** accounts because contour classification needs both.
- **It names nobody.** The row becomes `CashIn` or `CashOut`, whose flow
  endpoints say the other side is a counterparty this system does not observe.
- **It names nobody and asserts the other side is an account of the owner's.**
  There was no shape for this, and the two above are both false about it: one
  needs an account nobody stated, and the other says the money crossed a
  boundary the source says it did not.

The vocabulary collapsed the third claim into a weaker one on the way in.
`ObservedDirection::Inner` — the value a converter had for a row filed as
internal — means, by its own doc comment, that the source called the movement
internal **to the institution**. That is equally true of a payment to a stranger
who banks there, and it says nothing about whose accounts were involved. So a
source making the stronger claim had it rounded off at intake, and nothing
downstream could separate the two situations again.

The cost showed up in a monthly import of fifteen rows. Eleven settled silently.
Four carried a date, an amount, a word for a movement between the owner's own
accounts, no direction and no counterparty. Each raised
`Question::UnresolvedDirection`, and four unanswered questions held the commit —
for rows about which the owner had nothing useful to say, because the source had
told him nothing the system had not already read.

Two different situations hide under that one wording, and they want different
answers:

1. **Two genuinely different accounts of the owner's.** The far side's statement
   arrives on a later import pass, and the two halves can then be related.
2. **Two payment instruments over one underlying account.** Money moves between
   them, the account's balance does not change, no asset class changes, and
   there will never be a second statement, because there is no second leg.

## Decision

### 1. The source's claim about the far side is a value of its own

```rust
pub enum FarSide {
    Unstated,
    OwnAccount,
}
```

Carried on `ObservedRow` and on `ClassificationSubject`, beside the counterparty
and beside the direction rather than inside either.

**Not a direction.** The wording says nothing about which way the money ran, and
the sources that use it commonly print no direction at all — that pairing is the
case the whole record is about.

**Not a counterparty.** `ObservedCounterparty` answers *who*, and this answers
*whose*; the two are independent, and a source may answer either without the
other. A variant of `ObservedCounterparty` would make them exclusive, so a row
that both asserts the far side is the owner's and prints a string for it would
have to drop one — and the string is what a rule matches on and what the
directory resolves, so dropping it would throw away the only thing that could
ever name the account.

**Not a `bool`.** `Unstated` is «the source said nothing», which is not «the far
side is somebody else's». No source states the negative, so there is no third
value for it.

Only a source may set it, and only where the export says so in words. Deciding
that a counterparty is one of the owner's accounts is a **conclusion**, it is
reached on the server against his directory, and it stays there. This reaches
the observation channel's published shape because decision 0006's parity rule
requires it: the observation channel must be able to say everything the
conclusive one can.

### 2. Two journal variants, and the line between them is the leg

```rust
EventKind::OwnAccountMovement { amount: Money }            // signed; one cash leg
EventKind::UnresolvedOwnAccountMovement { amount: Money }  // magnitude; no legs
```

Schema version 13.

The direction may be unstated, and without it the journal cannot honestly debit
or credit the near account. A single variant with `direction: Option<Movement>`
was refused: the difference between the two is not a flag, it is **whether the
fact has a leg**, and legs against no legs is the sharpest line in this journal
— `validate_structure`, `Balances`, `perimeter::assess` and `projection::flows`
all read it. One variant would put that line inside a payload for each of them
to rediscover, and would make `Some(direction)` with no leg, and `None` with a
leg, states somebody has to be refused rather than states nobody can write.

A typed family inside one variant, the shape `CorporateAction` uses, was the
other candidate. Its own criterion decides against it: a family belongs in one
variant when its members «are handled together wherever that is what matters».
These are not — one posts and is classified against the contour, the other posts
nothing and is inert — and the discriminant reaches storage, where a projection
looking for movements that might be internal wants only the first.

`OperationKind::OwnAccountMovement` — the **submission** shape — does carry
`movement: Option<Movement>`, and the asymmetry is deliberate: a submission is a
report of what a source said, and «it did not say» is a thing a source does. A
fact is not, and that is where the option has to be resolved into a structure.

### 3. An unnamed owned endpoint is indeterminate, for every contour

`FlowEndpoints::OwnAccountUnnamed`, which `contour::classify` turns into a fifth
`FlowClass::Indeterminate { contour, version }` when the near account is in the
contour, and `Irrelevant` when it is not.

**A contour cannot prove it holds every account the owner has.** A
`ContourDefinition` is a versioned *subset* he chose; a narrower one deliberately
leaves accounts out, and `AccountScope` lets him rule an account outside every
contour. Even a contour naming every account in his directory would prove
nothing, because the directory holds the accounts he has told this system about,
and the far side here is by construction one it was never told about. So there
is no membership test that resolves this to `Internal`, for any contour, and the
reports must say so rather than guess. Answering `Internal` on the strength of
the source's word is the same mistake as reporting a transfer into one's own
account as earnings, made in the other direction.

### 4. Where the amount appears, decided rather than defaulted

- **`projection::flows`** counts it as `FlowLog::indeterminate()`, apart from
  both `internal` and `irrelevant`, and pushes no `ExternalFlow`. The returns
  series is left short by exactly the movements nobody could classify, and the
  count says how many. Folding it into `internal` would silently change no
  return; folding it into `external` would change one on no evidence.
- **`projection::money_flow`** gains two quantities. `indeterminate` is signed
  and **inside** the identity, because the cash really did move and `cash_delta`
  has it; without it every account carrying such a row would show a residual
  with no reason beside it. `unstated` is a magnitude and **outside** the
  identity, because the fact it comes from posts nothing — folding it in would
  open a residual of exactly the amount the journal declined to invent.
- **Category assignment is never consulted** for either. «What did I spend it
  on» is not a question anyone can ask of a movement that may not have been
  spending.

### 5. Completion is the existing correction, not a second mechanism

When a later pass supplies the far side, the movement is **replaced** by a
complete `CashTransfer` and any separately recorded second leg is **reversed** —
the shape `confirm_journal_pairing` already writes, for the reason recorded
there: a relation kept outside the journal would be a second notion of what is
effective, and the append-only journal would stop being the whole account of
what the owner knows. Having a fact here is what makes that possible; without
one there would be nothing to replace.

**The pairing matcher does not take these as legs, and that is deliberate.**
`leg_of_event` offers only `CashIn` and `CashOut`, on the stated ground that a
`CashTransfer` already names both accounts and «is not half of anything». An
unresolved movement is the opposite case — it is half of something and carries
no direction — and `propose` matches an outgoing leg against an incoming one, so
it has nothing to match on. Admitting the **oriented** variant alone would pair
it with an ordinary `CashIn` from a shop and propose a transfer nobody made,
while its own counterpart, arriving on the other bank's statement, would carry
the same assertion and be equally unoriented. The counterpart is the case to
solve, and it needs a matcher that works on amount and date across two
unoriented facts. That is a different matcher and it is not taken here.

### 6. A row may be settled by producing no fact, and the session says so

`RowResolution::{Fact, NoFact(NoFactReason)}` on the session, with

```rust
pub enum NoFactReason {
    OneAccountTwoInstruments { account: AccountId },
}
```

For the two-instruments case the honest financial record is **nothing**. Not a
zero-net pair, which invents two movements the model says did not occur; not
`CashTransfer { from: X, to: X }`, which `EventValidationError::TransferToSelf`
refuses on exactly this ground; and not an ordinary transfer, because a payment
instrument is not an account whose balance matters — decision 0004 settled that,
and two cards over one account are one account with two aliases.

**The mapping this rests on already exists.** `provider_account_id` and dated
aliases are per account, so the identifier a source prints for the far side
resolves through the same tiering as any other, and when it resolves to the very
account the row is on, the determination is made by the importer with no
question asked. That is the owner's actual requirement. Nothing about the
account model changes; what changes is that the resolution stops being thrown
away — until now it produced `Classification::InternalTransfer { to: own }`,
which `ObservedRow::resolve` refused, so the row came out **unreadable** and the
one thing the directory had established about it went with the rejection.

A direction is not consulted and could not help: whichever way the money ran
between two instruments over one account, the account moved by nothing. That is
what lets this settle a row no question could have settled.

**Where the mapping cannot be established the question stays**, and it should.
The source's own-account wording alone does not separate the two situations, and
we do not use it to.

The disposition is published three times, in the three registers a caller reads:
`HeldRow::Settled` when the row is fed, `CommitDelta::settled_without_fact` in
the plan, and an eleventh verdict `no_fact` at commit. The verdict is needed
because `verdicts` is a list read **by row position**, and a settled row that
produced no candidate would otherwise renumber every row after it.

`Verdict::Quarantined` was considered and is wrong here. Its published meaning
is that no fact *could* be written, and it is what `ImportCoverageGap` is
computed from — so filing this row there would record a fact saying the import
could not confirm the dimensions this row moves, when the row moves none and the
import is complete without it.

## What a reader loses

**Two more variants in an enum that is deliberately exhaustive.** Every
incomplete consumer breaks at compile time, which is the mechanism working; but
the enum is now sixteen variants wide and one more axis has to be held in the
head when reading any match over it.

**A journal fact that posts nothing and is not a control record.**
`ControlAssertion` and `ImportCoverageGap` are legless too, and both are *about*
the journal. `UnresolvedOwnAccountMovement` is about the money, and it is the
first fact that claims a movement and settles none of it. A reader summing legs
will not see it, which is correct and is also a new way to be surprised.

**A batch that no longer agrees with the statement's own turnover.** A row
settled without a fact contributes no movement, so a control section covering it
disagrees on the turnover side and the session reads `does_not_reconcile`. The
plan names every settled row and why, so the difference is readable rather than
merely absent — but the reader must read it, and «the arithmetic does not come
out» is a heavier signal than the situation deserves.

**A fifth `FlowClass` in a four-way decision table.** The mutation guard is
already known to be nearly blind on `classify` and `flow_endpoints`
(`docs/irreversible-core.md`), and the table test that compensates is now six
rows by four columns rather than four by four.

**An observation channel a converter can lie in.** `far_side: own_account` is a
transcription with nothing checking it. A converter that sets it where the
export does not say so records movements that never leave the perimeter and
never appear as spending. The same is true of `direction` today, and the
mitigation is the same — the plan publishes every fact before commit — but the
new field is easier to reach for, because it makes questions go away.

## What this does not settle

- **An owner who knows the far side is his and not which account.** There is no
  answer shape for it. `Answer::movement` is total over the two directions, by
  decision 0006, and admitting an answer that states none would loosen that
  contract; the row is recorded rather than asked about instead, so nothing is
  blocked, but the owner cannot volunteer the assertion himself.
- **Pairing two unresolved movements across two banks.** §5 says why the
  existing matcher cannot, and what a matcher that could would need.
- **A returns-level `MaterialIssue` for an unplaced amount.** An indeterminate
  flow does limit what the return figure means. Every member of that vocabulary
  names a repair the owner can make, and the repair here is a confirmation the
  pairing route cannot yet propose; adding the issue before the repair exists
  would publish a problem with no answer.
- **A no-fact determination the owner makes himself.** `NoFactReason` is closed
  with one member, the one the directory can establish. His word is different
  evidence and would need its own way of saying so.
