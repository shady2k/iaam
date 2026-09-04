# 0038. A question stops being open where the reading says so, not where the store does

Date: 2026-09-05 · Status: proposed · Bead: `iaam-m2oi`

## Context

A session records a question at intake and stores it. `ImportQuestionView` carries
`answered_at`, and `is_open` was the whole of every reader's test: the assessment
filtered the published questions by it, the commit refused by counting it, the
half-imported refusal named a question by it, the session's `unanswered` figure
summed it, and the answering call's wider reach selected by it.

That test answers a different question from the one all five were asking. It says
**the owner has not answered this**, and every one of them meant **something still
needs him to**.

The two came apart the moment the assessment started offering him a standing rule.
An offered rule (`iaam-qn6d`, decision 0029) is one condition that covers many of
the session's still-open rows, and the sentence put to him said, in the words a
caller reads out: «one answer here settles all N of them at once». Adopting it
calls `POST /v1/classification-rules`, which writes the rule and recomputes the
journal's history — and touches no import question, because it has never heard of
one.

So on the next reading of the session:

- `resolution_of` re-assessed each of those rows against the new rule and produced
  a planned fact for it;
- the stored questions kept their empty `answered_at`;
- the assessment named one row in `resolved` **and** in `open_questions`, which
  decision 0032 §1 says is impossible by construction;
- and the commit went on refusing, over questions nothing needed answered.

The one act meant to replace hundreds of answers left the session exactly as
blocked as before and told him the opposite. That is worse than never offering the
rule: he had made a standing decision about every row of that shape, this month's
and every later month's, on the strength of a sentence that was false, and the
wall of questions was still in front of him.

The same defect had a second, older instance nobody had generalised.
Decision 0031's mirror pass settles a row whose movement another row of the
session already records, and it had to be filtered out of the published questions
by a test written into `open_questions` and into no other reader. The commit
happened not to notice because it counts what `open_questions` returns; nothing
else did.

## Decision

### 1. One fold decides it, and it is over the reading

`QuestionSettlements` is the single answer to «is this question still waiting on
the owner». It is folded once over the session's rows as they were finally read —
the mirror pass included — and it says of each row either what settled it or
nothing:

- the row settled into a fact, and `FactBasis` says on what: a standing rule of
  his, his account directory, the source's own assertion, the caller's own
  conclusion, or his answer;
- the row settled into no fact at all, and `NoFactReason` says why;
- neither, and the question is genuinely his.

A question is waiting when `is_open` **and** the reading did not settle its row.
Both halves are load-bearing. `is_open` alone is the defect above. The settlement
alone would read a stored answer the row cannot express — one whose `resolve_with`
rejects — as a question still waiting, and the commit would refuse for ever with
no call that could clear it.

Every reader goes through it: `open_questions`, and therefore the commit refusal
and the assessment; the half-imported refusal's count and the question it offers;
the reach of an answer that settles every like row; the published question and the
`unanswered` figure beside it. `SessionContents::has_open_questions` is **removed**
rather than corrected — nothing holding only the stored rows can answer this, and
a method whose only possible implementation is the wrong one is the wrong answer
with the right name on it.

### 2. The mirror pass asks the same predicate, one step earlier

Decision 0031's pass decides which of a pair's two sides is already a fact, and it
asked `is_open` to do it. That was wrong for the same reason and it was hidden by
the same refusal: a row whose printed counterparty the owner's directory has since
come to place resolves into a complete transfer while its stored question keeps its
empty `answered_at`, so the pass saw «neither side settled», published the pair as a
decision he still had to make, and left both rows carrying the whole movement. The
commit refused over the two stale questions, so it never happened.

After §1 the commit does not refuse over them. So the pass reads
`ReadRow::settlement` — the same predicate `QuestionSettlements` folds, asked before
the pass rather than after it — and one row records the movement while the other
records nothing, which is what decision 0031 says. Without that, this decision would
have turned a hidden inconsistency into a movement recorded twice.

### 3. It is computed at read time and never stored

The alternative was to retire the question in the store when a rule settles it,
which would make «not open» a fact rather than a computation each reader must
repeat identically. It is refused, for three reasons in the order they bind.

**A stored verdict goes stale, and this system already argues that.**
`account_named_by_document_completion` makes the case at length for the account a
document asked for: the record is the transcription, the verdict on it is
recomputed, so an account created since the reading drops out without the document
being read again. The same holds here with more force, because the inputs move in
both directions — he writes a rule and a question stops waiting, he retires it and
the question is waiting again, he creates an account and his directory settles a
row nothing else could.

**Retirement would need a compensating write across a port boundary.**
`retire_rule` lives in the classification scenario, behind
`ClassificationRuleStore`, and knows nothing about import sessions. Retiring a
rule that had retired questions would have to un-retire them, in a table another
port owns, with no transaction spanning the two — which is precisely the shape
`iaam-77hk` was filed on and precisely the failure it left behind: one of the two
writes lands and the owner holds a state no call can reach. Computed, retirement
costs nothing at all: the next reading finds no rule, the row assesses as
ambiguous again, and the question is waiting again.

**The mirror pass already works this way.** `MirroredRows` is derived and never
stored, on this reasoning, and a second mechanism beside it — one stored, one
computed — would be two answers to «what settled this row» that can disagree in
front of the owner about the same row.

What read-time computation costs is a reading of the session on paths that used to
read one column: the published question, the `unanswered` figure, the
half-imported refusal, the wider reach. `SessionReading` is what they share, and it
stops short of the journal deliberately — what a row settles as needs his accounts,
his transfer statements and his rules, and not `load_events_through`.

### 4. What he sees is the vocabulary that already exists, not a new one

`QuestionSettlement` has two arms and they are `FactBasis` and `NoFactReason`,
which are the two vocabularies this module already publishes on
`PlannedFact::settled_by` and `SettledRow::reason`. Its `code` and `describe`
delegate. So the word a question gives for why it stopped waiting is the same word
the assessment gives for the row it is about, and a caller reading both reads one
determination twice rather than two that can differ.

Decision 0031 needed a word for a row settled by something other than its own
answer and extended `NoFactReason` with `second_leg_of_one_movement` rather than
inventing one. This extends the same two by naming them together, and adds no
third.

`ImportQuestionDto` publishes it as `settled_without_answer`, absent when the
question is still his and absent when he answered it. Which of those two is said by
`answered_at`, and this field deliberately does not repeat it: the two ways a
question stops waiting are «he spoke» and «something else settled it», and only the
second needs explaining. **Settled is not answered**, and the difference is what he
is entitled to see — he made one decision about a condition and this row is one of
the rows that decision reached.

Answering such a question is still permitted and still means what it meant.
`resolution_of` reads the stored answer before it consults the rules at all, so his
word about one row overrules his standing rule about every row like it. What the
published question no longer says is that he must speak.

### 5. The offer's sentence says what the code now does, and two other things it
never did

The clause «settles all N of them at once» is true once §1 holds, and the comment
above `offered_rule_question` names this decision so a later change to either is
checked against the other. Two further falsehoods in the same sentence are
corrected while it is open:

- It promised the decision covered «every later line the same **institution**
  files under this word». Decision 0026 §4 refuses to scope a category condition
  to a source and argues it at length; the rule fires on any row any source files
  under exactly that word. The sentence described a narrower standing decision
  than the one he was making, in the direction that costs him.
- It implied the rows move whatever he answers. They are one `RowShape`, and
  `ObservedRow::resolve` refuses a fee that arrived and income that left, so an
  outcome that does not fit settles **none** of them — all or none, never some,
  and he is told so.

One caveat is added and it is a fact rather than a table. Where the group's rows
state no direction, four of the five outcomes the offer names carry one themselves
and `Classification::ExternalFlow` carries none, so a rule stating it classifies
every row of the group and leaves each of them at `Question::UnresolvedDirection`
— still waiting, after the act that was supposed to end the waiting. Naming that
one answer is the difference between an offer that keeps its promise and one that
keeps four fifths of it in silence. Listing which outcome suits which shape would
be a structure encoded as prose, which `docs/api/conventions.md` §5 refuses.

## Consequences

A first import of a statement full of one word is now one decision rather than
hundreds: he adopts the offer, the rows it covers stop waiting, the assessment
publishes them as resolved with `settled_by: rule`, and the commit proceeds.

A caller that branched on `answered_at` to decide what is outstanding must branch
on the absence of both `answered_at` and `settled_without_answer`, or read
`unanswered` on the session, which counts exactly that. The published question
gained a field and lost none, so a caller that ignores it is no worse off than
before — it merely offers him work already done.

The paths that publish a question, count what is outstanding, refuse a second
import over a session already open, or settle every like row now read the session
rather than one column. Each of them already read the session; what is new is the
resolver beside it, and none of them reads the journal.

The action queue's `import_session_unfinished` item still counts from the store's
own `SELECT COUNT(*) … answered_at IS NULL`, which after this decision is the
number of questions with no answer recorded and no longer the number waiting on
him. Correcting it needs either a per-session reading in a listing built to avoid
one, or a count the store cannot compute because classification is not its
business. It is left as it is, named here so it is not mistaken for an oversight.
