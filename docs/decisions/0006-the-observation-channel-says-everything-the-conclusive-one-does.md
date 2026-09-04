# 0006. The observation channel says everything the conclusive one does

Date: 2026-09-04 · Status: proposed · Bead: `iaam-7l7v`

## Context

iaam offers a caller two ways to state a statement row.

**Conclusively.** `OperationKindDto` — `deposit`, `withdrawal`, `refund`,
`transfer`, `income` with an optional kind, `fee`, `tax`, and the instrument
kinds. The caller has decided what the row was and says so.

**As an observation.** `OperationKindDto::UnresolvedDirection` — the amount with
the sign the source printed, the direction word it used or none, the party it
named. The caller has decided nothing. The server classifies the row against the
directory and the owner's rules, and where neither settles it, it asks him
(`Question`), and his `Answer` settles it.

The second is the shape the system tells an agent to use. `CLAUDE.md` and
`docs/import-boundary.md` §4 both say it: an agent is never handed the owner's
statement, and it must not conclude what it was not told. The observation channel
is how a caller obeys that rule.

**The rule was not free to obey.** `Classification` had four outcomes —
`InternalTransfer`, `ExternalFlow`, `Fee`, `Income` — and every path out of an
observation went through one of them. Comparing the two channels outcome by
outcome, for the rows an observation can carry at all:

| What the row was | Conclusively | As an observation |
|---|---|---|
| Money in from outside | `deposit` | `ExternalFlow` + in |
| Money out to outside | `withdrawal` | `ExternalFlow` + out |
| Between the owner's own accounts | `transfer` | `InternalTransfer` + either |
| A charge | `fee` with an origin | `Fee { origin }` + out |
| An earning | `income` **with a kind** | `Income`, kind always `None` |
| A counterparty returning money | `refund` | **unreachable** |
| Tax | `tax` with an origin | **unreachable** |

Three gaps, and the first two are the ones that bite.

A **refund** could not be said at all. `EventKind::Refund` exists and the
money-flow projection subtracts it from what went out, in the category the money
was spent in, precisely so that a returned purchase does not appear as earnings
nobody earned. An observed return could only come out as `ExternalFlow` + in,
which is `CashIn`: money arriving. That overstates what came in and what was
spent, by the same sum, in the same month. And it could not be repaired
afterwards, because **no question was ever asked about a return** — the owner was
never given the chance to say so. `Question::IsInflowIncome`'s own doc comment
read "income or refund?" while its alternatives offered only `income` and
`received`.

An **income kind** was always dropped. `ObservedRow::resolve` built
`OperationKind::Income { kind: None }` with the correct comment beside it: the
source named no kind, and naming one there would record an invention (§4.9). The
comment is right about the source and wrong about the owner. He can name one, and
nothing let him.

**So the honest path was the losing one.** An agent that obeyed the rule it was
given produced a strictly poorer journal than a converter that concluded well,
and the poorer journal was the one that could not be repaired by answering a
question. `docs/import-boundary.md` §6 named this as the reason an external agent
imported the owner's converter into its own process rather than defer to the
server, and said that no amount of documentation moves the import boundary while
it stands.

## Decision

### 1. `Classification` gains `Refund`, and `Income` gains the kind

```rust
pub enum Classification {
    InternalTransfer { to: AccountId },
    ExternalFlow,
    Fee { origin: FeeOrigin },
    Refund,
    Income { kind: Option<IncomeKind> },
}
```

`Answer` gains `Refund` and `Income { kind }`; `AnswerShape` gains `Refund`, wire
word `refund`.

**Both go on `Classification` and not only on `Answer`, and that is the load
bearing half.** A rule is written in the `Classification` vocabulary. A decision
kept off it settles the one row the owner looked at and is dropped the moment his
answer becomes a rule — so next month's statement asks him again, or worse,
matches a rule that says something weaker than what he said. "This is a return"
and "this is interest on a balance" are claims about every row the matcher
matches, exactly as "this is a fee" is. The one thing that stays off is the
**direction**, for the reason already recorded on the type: a rule fires on rows
the owner has never seen, and a direction carried over from the row he wrote it on
would be asserted about all of them.

### 2. A refund implies the direction; `implied_movement`'s contract does not widen

`Classification::Refund.implied_movement()` is `Some(Movement::In)`, beside
`Fee` → out and `Income` → in.

This was the question worth arguing, because a refund's direction *looks* like a
property of the row — the sign the source printed, the direction word beside it —
and `movement_of` does consult the row first. But the question `implied_movement`
answers is a different one: what does the classification claim when the row
states nothing? For a refund the answer is *in*, because the journal holds no
other kind. `EventKind::Refund` carries a single positive cash leg; a refund that
left the account is not an under-specified refund, it is not a fact this system
records. Answering `None` would open a question with no admissible answer.

So `ObservedRow::resolve` refuses `(Refund, Out)`, beside the arms that already
refuse a fee that arrived and income that left.

**That refusal is reachable, unlike its two neighbours, and deliberately so.**
Both answers naming a refund state that money arrived, so no owner can produce
the pair. A *rule* can: it carries no direction, and a matcher written on a
merchant's name matches that merchant's purchases as readily as its returns. The
rejection is per row and the import continues (§10.1), so the owner sees exactly
which rows his rule was too wide for. The alternative — making `classify` skip a
rule whose outcome disagrees with the row's direction — would quietly make rules
direction-sensitive, which is the property the vocabulary is built to deny.

### 3. `refund` is offered by every question that leaves an arrival open

`IsInflowIncome`, `IsTransferInternal` and `UnresolvedDirection` publish
`AnswerShape::Refund`. `IsOutflowAFee` does not.

The split is by what each question leaves open, not by which of them mentions
returns. `IsOutflowAFee` is the one question both of whose alternatives run the
same way — a fee and a payment out both leave the account — and it is asked only
where the direction is settled outward. The other three already publish
alternatives pointing both ways: `IsTransferInternal` offers `paid` beside
`received` although the row stated a direction, because an answer states its own
direction and the owner is entitled to contradict the source.

`IsTransferInternal` is the important one and is easy to miss. A card return
prints the merchant's name beside a positive amount, so the common refund is a
row with a *named* counterparty — `IsTransferInternal`'s case, not
`IsInflowIncome`'s. Adding the shape only to the question whose doc comment
already said "or refund?" would have left the ordinary refund unreachable.

`IsInflowIncome`'s wording changes with it. It read "income, or money coming
back?", where "money coming back" was the sentence for `received` — money
arriving from outside — and read to a human, and to an agent relaying it, as the
refund the vocabulary could not express.

### 4. `classification_of` reads a recorded refund back as a refund

`EventKind::Refund` maps to `Classification::Refund` rather than to
`ExternalFlow`, and `EventKind::Income { kind }` carries its kind across. The two
were one answer while the vocabulary had one word for both; now that it has two,
saying `ExternalFlow` would make every refund in the journal look to
`recompute_plan` like a row a refund rule still has to correct.

## Consequences

**A rule that says `income` with no kind now proposes correcting a coupon it
matches.** `Classification::Income { kind: None }` means *income, and no kind was
stated* — the meaning §4.9 already fixes for that field on `EventKind::Income`
and `OperationKind::Income`. It is not a wildcard, so a rule written without a
kind disagrees with a recorded coupon, and `recompute_plan` says so. This is the
rule saying what it says; the plan is returned and never applied, so the owner
sees it before anything is written. Making `None` mean "leave the kind alone"
would give one spelling two meanings, which is the defect §4.9 exists to prevent
— but it is a real cost, and the alternative reading is the obvious falsification
of this record.

**The wire breaks in four places** (acceptable this wave; a tester re-runs from a
clean instance):

- `POST /v1/import-sessions/{s}/questions/{q}/answer` takes `refund` as an
  `answer`, and takes `income_kind` beside `income`. A kind sent beside any other
  answer is refused, not dropped.
- Questions publish `refund` among their alternatives. A client with a hard-coded
  list of six answer words sees a seventh.
- `ClassifiedAsDto` gains `income_kind` and admits `refund` as a `kind`. A client
  that enumerated four outcomes sees five.
- Stored rules gain an `income_kind` member in their outcome JSON. Rules written
  before this read back unchanged: the member is absent, which is `None`, which is
  what they meant.

**What this does not settle.**

- **Tax.** It is the third parity gap and it is left standing on purpose.
  `classification_of` answers `None` for a recorded tax, so tax sits outside rule
  recalculation entirely; a `Classification::Tax` would overturn that decision in
  passing rather than by one. An observed tax payment still resolves as a
  withdrawal, and the owner's converter is still the only thing that can say
  `tax`. It deserves its own record.
- **An income instrument.** An observation resolved as income carries
  `instrument: None`, and that is not the same absence as the kind beside it: the
  kind is one word true of every row a rule matches, while the instrument is a
  different security on every row, so there is nothing for a rule or an answer to
  carry. A broker report states the instrument and goes through the conclusive
  channel, which is the right place for it.
- **Whether an alternative should publish the fields it admits.** A question says
  `needs_account` and says nothing about `origin` or `income_kind`; a client
  learns those from the schema. The three could be one mechanism. Doing it here
  would have been a second change riding on this one.
