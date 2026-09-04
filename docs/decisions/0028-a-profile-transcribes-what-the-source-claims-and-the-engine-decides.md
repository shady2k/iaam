# 0028. A profile transcribes what the source claims about a row, and the engine decides what follows

Date: 2026-09-04 · Status: proposed · Beads: `iaam-b0r0`, `iaam-rdya`, `iaam-2hq0`

## Context

The system's owner ran his first import through a shipped profile and asked why
he was being asked anything at all. It is a fair question. One reading of one
card statement raised roughly three questions per distinct counterparty, and the
document had already answered most of them — in columns the profile does not
read.

Decision 0019 §2 lists what a profile may say and the list is not short. What
this decision found is that the list was written from the shape of a bank's
*statement* and the first shipped profile reads a card *export*, which states
three more things about every row and states one of them in a place the schema
could not reach.

Three gaps, and only the first two are defects.

- **A far side asserted in a free-text column.** `row.far_side` exists, is
  documented for exactly this case, and is consulted by `classify` **before** the
  question is raised — `subject.far_side.is_own_account()` resolves to
  `Classification::OwnAccountMovement` with `Basis::Derived`. And the shipped
  profile does not map it, because it cannot: the only shape the schema offered
  was a map total over the column's vocabulary, and the column carrying the claim
  is the description line, whose vocabulary is every string the owner ever paid
  anybody. A total map over it rejects every row whose description its author had
  not met.
- **A row's status.** There is no key for the column by which a source says
  whether a movement is final, pending, declined or reversed. A profile therefore
  cannot decline to read a row the source itself marks as not a completed
  movement, and such a row is transcribed and committed like any other. It
  records money that did not move. This is a correctness defect and it is the
  only one here.
- **Three more columns a card export classifies by**, for which `row` has no
  place at all. §4 weighs each; two of them are answered "nothing", and saying
  where the evidence comes from instead is part of the answer.

The three are one decision because the same question settles all of them: **what
may a profile say about a claim its source made, and who acts on it.**

## Decision

### 1. A far side stated in a free-text column is read, and the column chooses the shape

`row.far_side` gains a second branch, and which of the two a profile may write is
a property of the column rather than of the author's taste.

| The source states the far side in | The profile writes | A cell the profile did not foresee |
|---|---|---|
| a column of its own — an operation-type or status column, whose vocabulary is closed | `tokens`, a total map | rejects the row and names the word |
| a free-text column — the description or purpose line, in which the institution prints one fixed sentence of its own for a movement between the owner's accounts | `own_account_words`, the sentences it prints | is `unstated`, which is what the source said |

**The second is not the leniency decision 0019's third invariant refuses**, and
the difference is exact rather than rhetorical. Leniency says what becomes of a
cell the engine *could not read*. There is no such cell here: every cell is
read, and a cell that is not one of these sentences is a cell in which the source
made no claim about the far side — which is precisely what `unstated` means. The
totality argument does not transfer either, because it rests on the column having
a vocabulary to be total over, and a description line has none and never will.

**What makes the branch admissible is an asymmetry, and it is worth stating as a
rule.** The list can only ever produce `own_account`; there is deliberately no way
to write a sentence meaning `unstated`, because absence already means that. So
nothing in this branch can be missing in a way that conceals anything. A sentence
misspelt, or gone stale because the institution reworded its own line, costs a
question that gets asked — never one that stops being asked. That is the safe
direction, and it is the opposite of what a wrong entry costs, which is §2.

**Matched by equality after trimming, never as a substring.** A substring test is
a predicate and the second review invariant admits none; and the case it would
cost is concrete — a counterparty whose printed name happens to contain the
institution's own sentence would be filed as an account of the owner's, silently.

**The shipped profile's list is left empty, and that is the decision and not an
omission.** The sentence a real institution prints is a value out of the owner's
own document, nobody has authorised it into this repository, and the agent that
built this capability has not seen the file. A list with a wrong entry is worse
than an absent one: an absent one asks a question that is answered once, and a
wrong one marks as internal a movement that is not, forever, without saying so.
So the capability is built, the tests are written against sentences invented for
them (`crates/iaam-ingest/src/profile/engine.rs`), and
`crates/iaam-ingest/profiles/tbank-operations-csv.json` carries **no** `far_side`
block. A JSON profile has no comment key — the schema is closed and an unknown
key is refused — so the marked place for it is here and in
`docs/import-boundary.md` §10, and filling it is one block, one version bump and
one test, by somebody who has read a real document.

Nothing in this touches decision 0019. Transcribing what a source asserts in its
own words is what a profile is for, and `direction` already uses the same shape.

### 2. An asserted far side is the only thing a profile can say that removes a question

Every other mapping a profile gets wrong produces a row that is refused, or a
fact that is visibly wrong. `far_side: own_account` is different in kind: a row
carrying it is resolved with `Basis::Derived`, records an own-account movement,
and **raises no question at all**. `iaam-rdya` put it exactly — this one is easier
to reach for, because setting it makes questions go away.

Two things follow.

**The import boundary says what a converter may assert.** `docs/import-boundary.md`
§4 already forbids an *agent* to decide whose account the far side is. It said
nothing about a converter, which is the thing that actually writes the field, and
§10 now does: the observation channel's fields divide into what a source printed,
which a converter relays, and what somebody concluded, which it does not — and
`far_side` is the field on which that line is worth the most.

**The plan does not yet distinguish an asserted far side from a derived one, and
it should.** This was checked and the finding is negative:

- `classify` returns a `Basis` and `PlannedSession`'s `assess` discards it —
  `ClassificationResult::Resolved { classification, .. }`.
- `PlannedFact` carries `records_as`, which is the journal's word for the event
  kind. It is not nothing: within a document-read session an
  `own_account_movement` can only have come from an asserted far side or from a
  standing rule of the owner's, since no answer of his produces that
  classification. But it names the *shape* of the fact and not the *evidence*,
  and the two origins collapse into one word.
- The damage an over-asserting converter does is measured by what was **not**
  asked, and the plan's `open_questions` list is exactly where that is invisible:
  a silenced row appears among `resolved` with no marker at all.

The fix is small and it is named here so that it is not re-derived: `assess`
keeps the `Basis` it already computes, `PlannedFact` gains one field saying that
the source asserted the far side, and `PlannedFactDto` publishes it. It is not
made in this change because `crates/iaam-app/src/scenarios/import_session.rs` and
`crates/iaam-server/src/dto.rs` are held by other work in this wave, and a field
added to a plan in two places at once is worse than a field added late.

### 3. A row the source has not completed is refused, and the profile only transcribes

`row.status` is a token map from the source's own words to three of iaam's:
`completed`, `pending`, `declined`. A row the map calls anything but `completed`
is **refused by name**, with its column and its word, and the other rows of the
document are read.

Two shapes were weighed, as `iaam-2hq0` asked.

- **A refusal written in the profile** — a key saying which words to decline.
  Rejected. It is a predicate wearing a token map's clothes: it says what the
  engine should *do*, which is the one kind of statement decision 0019's second
  invariant exists to keep out of a profile, and it would be the first key in the
  file whose meaning is "skip".
- **A transcription the session then refuses.** Rejected for a different reason,
  and it is the one that nearly won: it keeps the profile purely descriptive at
  the cost of a fact that must then be read somewhere, and "somewhere" is a new
  field on `ObservedRow`, a new value in the published observation shape, and a
  new outcome the session has to know about — for a fact nothing downstream ever
  wants. A status that survived into the journal would be a fact about the
  source's own workflow sitting beside facts about the owner's money.

**What was picked is neither, and it is the arrangement 0019 already describes.**
The profile transcribes and stops, exactly as the direction map transcribes a
direction; the *engine* decides what follows, once, for every profile. That is
already where 0019 puts "whether a cell is readable at all" and "what happens to
a row that cannot be read", and §4 of that decision gives the precedent in the
same words: a trailing totals line is read as a row, fails to be one, and is
rejected by name.

The vocabulary is three words and the third is not a fourth.

- `completed` is the only word an observation is made from.
- `pending` is money the source has not moved yet and expects to. Refusing it now
  is what keeps the journal from holding one movement twice: the source prints
  the row again, completed, in a later export, under the same identifier or at a
  different locator.
- `declined` is money the source states never moved at all.
- **There is no word for a reversal**, and a source's reversal word therefore has
  nothing to map to and rejects the row. That is deliberate: a reversal is a
  movement of its own, with its own row and its own sign, not a status on another
  movement — and a profile that filed it under `declined` would say the money
  never moved when it moved twice.

The status is read **before every other cell of the row**. Two things follow, and
both are the reason for the order: the refusal names `status` rather than
whichever other cell of a row nobody should be reading happens also to be
malformed, and a declined row does not argue — through `unresolved_accounts` and
the queue item decision 0024 built on it — that an account the directory does not
know is an account the owner wants.

**One cost is named rather than hidden.** The refusal reuses `Rejection`, so the
document response reports the row as `unreadable`, and a declined row was read
perfectly well. The fields are precise — `field: "status"`, the column, the word,
and a sentence saying what the word means for the owner — but the state word is
wrong, and a third outcome beside `held` and `unreadable` is the right repair. It
is not made here for §2's reason: the response's shape is `dto.rs`'s.

### 4. Three more columns, and what each is worth

**A merchant classification code — a named field, and it is designed but not
built.** It is the strongest classification evidence in such a document and the
only one that is not an institution's private vocabulary: the code is assigned by
the payment network, so a rule written on it holds across institutions, where a
rule written on a source's own category holds for one bank until it renames
something. It is worth a **named** field rather than an entry in a general map of
transcribed columns, because a general map's key is one profile author's spelling
and a rule matching on it would be scoped to that author's choice — which is
exactly the property that makes this code worth having. It is transcribed as
text and never as a number: it is an identifier printed with leading zeros, and a
number loses them.

It is not built in this change, and the reason is mechanical rather than a
judgement about its value. A field on `ObservedRow` is a field in every struct
literal of `ObservedRow`, and those live in `crates/iaam-app/src/actions.rs` and
`crates/iaam-app/src/scenarios/import_session.rs`; and for a rule to read it —
which `iaam-3nqt` and `RuleMatcher::source_category` say is the whole test of
whether a transcribed field does anything — `RuleMatcherDto` in
`crates/iaam-server/src/dto.rs` must carry it too. All three are held by other
work in this wave. Decision 0019 already deferred a neighbouring field, the far
account's own identifier, on the ground that it is "a change to the observation
channel's published shape" and belongs with the parity work; this is the same
ground and the same answer.

**A second, owner-set category column — nothing, for now.** The institution's app
lets the owner file a row under a category of his own beside the one it assigns
by default, and the export carries both. iaam has exactly one field for "what the
source filed this row under", `source_category`, and a profile names a column for
it — so a profile may name **either** column, and an owner who files rows in his
institution's app can point it at his own. That is a locator choice, which is
what a profile is for, it is visible in the file, and it costs one version bump.
What he cannot have is both at once, and having both is a sibling field on the
observation channel: §4's first paragraph, same reason, same wave. The evidence
in the meantime comes from whichever column he chose plus his own rules, which
are re-runnable over rows already recorded and a category frozen at import is
not.

**The source's own "counts in analytics" flag — nothing, and not later.** The
institution states per row whether *it* treats the movement as a real expense.
That is a decision about the institution's own reporting perimeter, and iaam has
a perimeter of its own that is the owner's (decision 0014). Transcribing the flag
would hand him a rule vocabulary whose meaning is "the bank did not count this in
the bank's report", and a rule written on it silently imports another product's
reporting boundary into his — the one thing a perimeter must not be. On
own-account movements it says "no", which is the case that makes it look useful,
and there the far side already says the same thing in words and says it as an
assertion about the far side rather than as a conclusion about a report.

### 5. What a movement between the owner's own accounts becomes, on both legs

Such a movement prints as two rows in one document, one on each account, opposite
in sign, moments apart. `iaam-b0r0` asked what a commit does with the pair. The
answer is not obvious and half of it is a cost.

Each row resolves independently to `Classification::OwnAccountMovement`. With a
direction — which the shipped profile has, from the amount's sign — each becomes
`EventKind::OwnAccountMovement` with **one** signed leg on its own account. So:

- **`Event::validate_transfer` is never reached.** It validates `CashTransfer`,
  which requires two legs and two named accounts; an own-account movement is not
  a transfer, names no far account and has one leg. The two rows are two
  independent events, not one two-legged fact.
- **Cash is right.** One event posts the outflow on one account and the other
  posts the inflow on the other, so across a contour holding both they net to
  zero and no phantom external flow appears.
- **`resolve_transfer_relationships` has nothing to ask**, and that is achieved by
  the pair being invisible to the matcher rather than by its being matched:
  `transfer_pairing::leg_of_event` offers the matcher `CashOut` and `CashIn` and
  nothing else, so neither leg is ever proposed, and neither shows up in
  `Proposals::unmatched` either.
- **And the pair lands in `indeterminate`.** `FlowLog` counts an own-account
  movement neither as spending nor as an internal reallocation: the far side is
  unnamed, so the report will not say whether the money crossed the contour
  boundary. Two rows the owner would have called one internal transfer therefore
  become two indeterminate quantities in his money-flow report, permanently, and
  because the matcher does not see them there is no route by which he can say
  what they were.

That is the real trade this mapping makes, and it should be read beside the
questions it removes: three questions per counterparty, against one pair of
indeterminate quantities per internal transfer. It is the better side today —
`indeterminate` is an honest answer and a question answered wrongly is not — but
the repair is clear and belongs in a bead of its own: `leg_of_event` should offer
own-account movements to the matcher, so that the two legs can be proposed and
confirmed into the `CashTransfer` they always were.

## What was rejected

**A catch-all or a default arm on the far-side map.** §1 gets the same effect
where it is safe by inverting the shape rather than by widening the map: the
list states only the asserting sentences, so there is no arm for a word to fall
into and no word whose absence hides anything.

**A substring or regular-expression match against the description.** §1. It is
decision 0019's most tempting rejected feature by another route, and here its
failure mode is the exact one this whole field is dangerous for.

**Filling the shipped profile's list with a plausible-looking phrase.** §1. A
guess that happens to be right is indistinguishable from a guess that is wrong
until a movement that is not internal has been filed as one.

**A key in the profile saying which status words to decline.** §3.

**A status carried on the observation.** §3. Nothing downstream wants it, and a
fact about the source's workflow does not belong beside facts about the owner's
money.

**A `reversed` status token.** §3.

**A general map of transcribed columns**, keyed by names a profile author
chooses. §4. It looks like the extensible answer and it makes every rule written
on it depend on one author's spelling — which destroys the one property that made
the merchant code worth transcribing.

**Transcribing the source's own analytics flag.** §4, and this one is not
deferred.

## Consequences

- **A card export's own statement about a row it did not complete is finally
  readable**, and the row is refused instead of committed. The first profile to
  carry a `status` block will reject rows a previous version of itself imported;
  those are facts recording money that did not move, and the remedy is the
  ordinary one — retract the import and read the kept document again.
- **A profile can now be wrong in a way that removes a question**, which nothing
  in the format could do before. §2 is the mitigation and half of it is
  outstanding.
- **The shipped profile is unchanged.** Nothing about this decision makes the
  first real import quieter until somebody who has read a real document fills in
  §1's list and §3's status map. That is the correct order.
- **Rows read by a profile with a status block get a smaller journal and a
  louder response.** An operator comparing the row count of his file with the
  row count of his session will now see a difference, and the response names
  every row and its word.

## What this does not settle

- **The plan's visibility of an asserted far side.** §2 names the change and the
  files. It is `iaam-rdya`'s remaining half.
- **A third document-row outcome beside `held` and `unreadable`.** §3. A declined
  row is read and refused, not unreadable, and the response word is wrong.
- **The merchant classification code.** §4. It is a change to the observation
  channel's published shape and belongs with the parity work, exactly as decision
  0019 says of the far account's identifier.
- **A second column of the owner's own categories.** §4.
- **Whether an own-account movement should be offered to the transfer matcher.**
  §5. It is the difference between two indeterminate quantities and one internal
  transfer, and it is not a profile question at all.
