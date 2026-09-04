# 0031. One movement a document printed twice is one fact

Date: 2026-09-04 · Status: proposed · Beads: `iaam-3qsq`, `iaam-9ck1`, `iaam-rdya`

## Context

A statement that covers two of the owner's own accounts prints a movement
between them **twice**: a departure on the account the money left and an arrival
on the account it reached, same day, same amount, opposite signs, under one word
of the source's own. Nothing in either row says the two are one movement.

The journal's shape for such a movement carries a leg on **each** account. So a
reading that takes both rows at face value does not record two halves — it
records the whole movement twice, and every account moves twice.

Three separate mechanisms met on this row and had to be decided together.

### The two rows raise two questions, and both answers are the same fact

`Answer::classification` maps `SentToOwnAccount { to }` and
`ReceivedFromOwnAccount { from }` alike to `Classification::InternalTransfer`,
and `ObservedRow::resolve` turns that into `OperationKind::Transfer` — with the
arriving row deliberately re-attributed to the account the money left, because a
transfer is submitted from its sending side.

That left a caller with two moves and no third:

- **Answer both legs**, and the transfer is recorded twice. The rows have
  different row keys, so deduplication does not see them as one; nothing refuses
  it. That is exactly the error the one-row rule exists to prevent.
- **Answer one leg**, and the session does not commit, because the other
  question stays open.

The defect is not confined to questions. A document whose counterparty column
names the far side on *both* rows produces the same double count with **no
question asked at all**: the directory recognises each name, `classify` answers
`InternalTransfer` on each row, and two complete transfers reach the journal.
Any fix written only against open questions would have missed the case that
needs no owner at all.

### The engine is the place, and that is an argument

`docs/import-boundary.md` refuses a caller that interprets rows. Making a caller
find the pairs is one level worse than that: it is making the caller re-derive
what the reader already had in front of it. The engine sees the whole document;
a caller sees questions.

`cross_source_matching` — the transfer pairing of `iaam-3ul2` — does not fit.
It relates a row printed by one institution to a row printed by another: two
documents, nothing in common but a shape, a three-day window because two banks
post on their own schedules, and a candidate the owner confirms. Widening it to
cover two rows of one document would let it decide the one thing it exists to
leave open.

### The other half: a movement whose far side was asserted

A row whose far side the source asserts becomes `OwnAccountMovement` — one
signed leg, no cross-account leg, no double count, no pairing needed. Two such
rows, one per account, are the ordinary output of a bank that files its internal
transfers under a word and names nothing.

`transfer_pairing::leg_of_event` offered only `CashOut` and `CashIn`, so such a
movement was never proposed, never appeared in `Proposals::unmatched`, and could
never be handed to `confirm_journal_pairing`. `contour::classify` answers
`Indeterminate` for it, so every internal transfer settled this way became two
indeterminate quantities in the money-flow report, permanently, with no
owner-facing route back.

### And a third: an asserted far side spelt like a derived one

`assess` took the classification out of `ClassificationResult::Resolved` and
dropped the `Basis` one line later, and `PlannedFact.records_as` names the
journal's event kind rather than the evidence. So a row a profile asserted a far
side for and a row one of the owner's standing rules settled reached a reader as
one word. Asserting is the cheapest way for a profile to make questions
disappear, and the damage it does is measured by what was **never asked** —
which appears in no list of open questions.

## Decision

### 1. Pairing is done by the engine, over the whole session, and derived

`iaam_ingest::mirror` is a pure test over the rows of one document: two sides
are one movement when they are on two different accounts, agree on currency,
magnitude and **day**, run opposite ways, and neither names a far side that is
not the other's account.

The day is exact and not a window. These rows came out of one document, and a
window would be a claim about two institutions posting on their own schedules —
which is the cross-institution matcher's case, not this one.

The pairing is **derived and never stored**, for `Generalisation`'s reason: a
stored copy would be a second place recording one determination, able to
disagree with the first in silence. Derived, the pairing is also the pairing
that holds *now* — a row fed later, or an answer that named a third account,
changes it on the next reading, which is what should happen to a hypothesis.

### 2. Ambiguity pairs nothing; identical multiplicity is not ambiguity

A row that could be the far half of movements on **two different accounts** is
matched with neither: which account it went to changes what is written, and
nothing in the document chooses.

Several rows that agree on everything *including* the two accounts are a
different case. Two identical movements between one pair of accounts print four
rows, and every way of matching them yields the same two facts, so they are
matched one-to-one in row order. Refusing them would double-count exactly the
document that stated itself most completely.

### 3. What a pair does depends on how much of it is settled

- **Both sides settled into a `CashTransfer`.** The row on the **sending**
  account keeps its fact — a transfer is recorded from its sending side
  everywhere else in this system — and the other row records nothing. This
  covers both the owner answering both legs and his directory recognising the
  far side printed on each of them.
- **One side settled.** That fact already carries a leg on the open row's
  account, so the open row has nothing left to add: it is settled by that
  answer rather than by one of its own, and its question is no longer open.
  This is what lets a session commit after **one** answer.
- **Neither settled.** Nothing is recorded and nothing is suppressed. The two
  questions are published with a shared `pair` identifier so that one decision
  can be put to the owner once instead of twice.

### 4. A pair is a hypothesis, and the answer is how it is refused

Two unrelated payments of one amount on one day exist, so «no, these are two
different things» must remain sayable — and it is said by **answering**. Any
answer that does not name the other row's account — *this was a payment*, *this
was a fee*, *this went to a third account of mine* — leaves the two rows as two
rows with two questions, exactly as they stand today.

No new answer shape was added for the refusal, and that is deliberate. A
dedicated «these are unrelated» answer would be a second way of saying what an
ordinary answer already says, and the two could be given inconsistently. The
settlement in case 3 rests on the owner's own words naming this account, this
day, this amount and this direction; nothing is ever settled on the shape alone.

### 5. The vocabulary: a row read, real, and yielding no fact

The mirror row is neither unreadable nor unanswered. A rejection would say a
fact is owed and open a coverage gap for a row nothing is owed for; a retention
would hold the commit for a row there is nothing to decide about.

It joins `NoFactReason`, which already holds `one_account_two_instruments` and
is the vocabulary `iaam-5bup` wants for a row the source declined. The second
member is `second_leg_of_one_movement`, and it names the row that **does**
record the movement — so the determination is auditable in the one way that
enumeration insists on: the reader can go and look at the fact that was kept.

`NoFactReason`'s members are therefore distinguished by the evidence each rests
on, not by who established it: the first is what the owner's directory
establishes on its own, the second is what the rest of the session's rows
establish. An owner-declared no-fact stays absent.

### 6. An own-account movement is not collapsed; it is proposed

Two rows the source settled as `own_account_movement` post one signed leg each
and count nothing twice. Collapsing one into the other would destroy a leg the
journal correctly holds.

The correction is `transfer_pairing::leg_of_event`, which now offers an
own-account movement to the matcher, reading its direction off the sign of the
leg the fact posts. The two halves become a proposed pair; the owner confirms;
the pair becomes a `CashTransfer` naming both accounts, which classifies as
internal on the ordinary path with nothing guessed.

`UnresolvedOwnAccountMovement` is deliberately not offered: it posts no leg, and
a leg needs a side. What it needs is the direction, and that is a question for
the owner, not a pairing.

**`FlowClass::Indeterminate` is not corrected and is not wrong.**
`contour::classify` sets out why «an account of the owner's» cannot be resolved
to «inside this contour» by any membership test. Counting these as internal
would silently change a return on the strength of a word a bank printed about
itself. What was missing was a route out, and the route is a pairing the owner
confirms — not a different answer from the projection.

### 7. A fact says on whose word it is written

`Basis` gains `Asserted`, told apart from `Derived` because they are not one
thing: the first is a conclusion this system reached from the owner's directory,
the second is a claim the source made about its own row that nothing here
checked or can check.

`assess` keeps the basis, `PlannedFact` gains `settled_by`, and
`PlannedFactDto` publishes it as one of `concluded`, `directory`,
`source_asserted`, `rule` or `answered`. `records_as` says what the fact **is**;
this says why it may be written, and the two were one word until now.

`FactBasis` is not `Basis` and is not a superset of it: `Basis` answers for the
one step `classify` takes, and a row can be settled without that step running at
all — by the caller having concluded, or by the owner having answered.

## Consequences

- A document covering two of the owner's accounts imports without recording any
  movement between them twice, whether the rows were settled by his directory,
  by his answers, or by one of each.
- A session commits after the owner answers **one** leg of a pair. The store
  still holds the other question unanswered, because it is settled by a reading
  of the whole session and not by a word of his about that row; the assessment
  is what says so, and it says so on every reading.
- `OpenQuestion` gains `pair`, and it must never be read as a stronger `alike`.
  Alike rows are the same decision about different money; a pair is one movement
  with one fact between it.
- A pair identifier is derived from the session and the two row numbers, so two
  readings of an unchanged session publish the same one — a minted value would
  move the session's revision stamp under a session nobody had touched.
- A reader of a plan can now count how many of its facts were settled by a word
  the source printed about itself. Nothing refuses on that count, and nothing
  should: what it does is make an over-asserting profile visible where it was
  invisible.
- The transfer-pairing section of an assessment can now propose pairs among
  own-account movements. That is a new candidate for the owner to read and is
  reported by `Readiness::RequiresOwnerDecision`, which does not refuse the
  commit — leaving them unconfirmed records what the journal records today.

## Alternatives considered

**Pair at the caller.** Refused: the caller sees questions, the engine sees the
document, and the derived double count needs no question at all — so a caller
mechanism would miss it entirely.

**Raise one question for the pair and never store the second row's.** The shape
the bead proposed. Refused for what it costs when the hypothesis is wrong: the
mirror row would have to have a question **created** for it at answer time, in
the answering call, on a store the plan is otherwise derived from. Publishing
both questions with a shared identifier gives the caller the same «one decision»
and keeps the refusal free — the owner refuses by answering, and the row he did
not pair is already waiting with its own question.

**A dedicated «these two are unrelated» answer.** Refused: it says what an
ordinary answer already says, it can be given inconsistently with the answer
beside it, and it would need an `OwnerPrompt` variant to explain a choice the
owner has no reason to make separately.

**Widen `MATCH_WINDOW_DAYS` and let the transfer matcher cover this.** Refused:
the matcher proposes and never decides, and this case must decide — the document
prints both halves and there is one fact between them. Making the matcher decide
here would make it decide across institutions too.

**Count `OwnAccountMovement` as an internal flow.** Refused: it is the same
mistake as reporting a transfer into one's own account as earnings, made in the
opposite direction. See `contour::classify`.
