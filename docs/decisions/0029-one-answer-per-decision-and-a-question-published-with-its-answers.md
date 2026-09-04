# 0029. One answer per decision, and a question published with the answers it admits

Date: 2026-09-04 · Status: proposed · Beads: `iaam-qn6d`, `iaam-q5og`, `iaam-ulib`

## Context

The owner ran his first real import of one card statement and got roughly three
questions per distinct counterparty — hundreds of them, two thirds of them
literal repeats. His question was the right one: **why is it asking me anything
at all? isn't the statement enough?**

Three separate defects produce that, and only the first is about what the
system knows.

### The question that is asked about every row

A statement cannot distinguish money paid to an outside party from money moved
to an account of his at another institution; only he knows which counterparties
are his. `Question::IsTransferInternal` is correctly conceived and is asked of
**every** row anyway.

`classify` tries the owner's rules, then asks whether the far side is a known own
account, and otherwise returns `Ambiguous`; `question_for` maps any named
counterparty with a known direction to `IsTransferInternal`. On a first import
there are no rules and no far side is known, so every row carrying a party name
becomes «is this one of your accounts?», and the answer is no for all but a
handful. The caution is spent on the case that never needed it.

Two things are inert while that happens. **The source's own category is
transcribed and read by nothing**: the shipped profile maps `source_category`,
decision 0026 made it writable as a rule condition, and `classify` consults it
only through the owner's rules — which on a first import do not exist. And
**there is no way to bootstrap**: a standing rule comes only from answering a
question, and an agent's answer writes none, so the only route from «no rules» to
«rules» runs through answering every question by hand.

### The question that is asked once per row

`answer_question` resolves the stored question's row and settles that row. The
generalisation behind it is gated by `may_generalise`, which is
`scope.may_administer()` — the owner. An agent working a session must therefore
make one call per row, answering a question it has already answered, with nothing
available to it for saying «and every other row like this one».

Three consequences, and none of them is about speed. The owner is read a question
he has already answered, which is the state `iaam-8ano` and the whole visibility
line were filed to end. The caller cannot tell from the queue or the session that
the repeats *are* repeats, because the assessment publishes questions in row
order and grouping them is work every caller invents separately. And a mistake is
made many times: one wrong answer to one row is a row, and the same wrong answer
across every row of a counterparty is a pattern in his reports.

### The question published without its answers

`OpenQuestionDto` was `row`, `question`, `prompt` and nothing else. Two other
shapes in the same file carry `alternatives`, and so does a third. So a question
was published in four places and one of them omitted the thing that makes it
answerable — the one place an agent reads to work through a session. Observed: an
agent went hunting across several routes, guessing at one that does not exist,
before finding the alternatives on a different response. Its own words:
«Маршрут не угадал», «Альтернативы в assessment не публикуются».

That is `iaam-1tij`'s shape one level down. Not a call no item can offer, but a
question no reader can answer from where it is published — while
`AnswerShape::consequence` holds exactly the material an agent needs to put the
question to the owner under decision 0027.

## Decision

### 1. Nothing concludes that an unrecognised party is a stranger

**Rejected: a default for an unrecognised party, applied inside `classify`.**

It is the most tempting of the three candidates and it is the one that would put
a guess in the journal. The agent skill's rule is that a missing value is asked
of the owner and not filled in, «a guess that has reached the journal is
indistinguishable from a fact — every report will read it as one». An
`ExternalFlow` concluded because nobody recognised the party is exactly that: the
journal records `CashOut`, every figure counts it as money that left the
perimeter, and nothing anywhere says the conclusion was reached by default. The
row that is wrong this way is precisely the row that matters — an account of his
at another institution, quietly reported as spending, in the one figure a
money-flow report is read for.

**The «stated default» was weighed on its merits and is not merely refused.** The
precedent offered for it is real: `report::assets` publishes an unstated
`cash_class` group rather than asking, and `None` sorts first there deliberately,
«the one the owner may still want to act on». But that is a **report** publishing
what it was not told. Its analogue here would be a fact that records «no party
was recognised» — and the journal has no such shade. `Basis::Derived` and
`Basis::Rule` do not reach a recorded fact at all; `Assessment::Settled` drops
the basis on the way. Making the default stated end to end therefore means a new
distinction on `EventKind` or on `Provenance`, a store migration, and a reader
for it in every report that would have to treat it differently — for a
distinction no report currently draws. That is a decision of its own, and it is
not this one. What is refused here is the version that is cheap: concluding
without recording that it concluded.

**Rejected: asking only where the answer changes something.** «A party appearing
once, against a category the source already filed as a purchase, is not a
plausible account of his» is a heuristic, and a heuristic that suppresses a
question is a guess with a confidence threshold on it. The observation is true
and the conclusion drawn from it here is different: a party appearing once is not
*less likely* to be his account, it is *less costly* to ask about. Cost is what
§3 addresses.

**Rejected, again and for decision 0019 §6's reason: letting the profile
classify.** A map from an institution's category to one of his classifications is
frozen into every fact at the moment of import and correctable only by retracting
the import, where a rule of his is editable, retirable and re-runnable over rows
already recorded. Nothing in this decision moves that line, and §2 exists
precisely to make the rule easy to write instead of moving it.

### 2. The session offers the standing decisions its own document affords

`Interpretation` gains `offered_rules`: one entry per distinct word the source
filed a still-unanswered row under, carrying the condition a rule on that word
would ask, the rows it would settle, and the question to put to the owner.

**It offers a condition and never an outcome.** What the rows have in common is a
fact about the document — this many of them were filed by the institution under
this word. What they *are* is his. An offer that filled in the outcome would be
0019 §6's forbidden map, written at the session instead of in the profile and no
better for being written later.

**Why the source's category and not the counterparty `matcher_for` would pick.**
Both are conditions he could adopt, and they differ in how many decisions they
cost him. One decision per merchant is hundreds of decisions, and the merchants
are the part that changes next month. The words an institution files its own rows
under are a closed list it controls, printed on every row, and there are a dozen
of them. `matcher_for` is right to prefer the counterparty for the rule minted
from **one** answer — that rule is a claim about the party he just decided about
— and this is a different question: which single condition would settle the most
of what is still open. Decision 0008's «one field» is unchanged; this picks a
different one, for a different question, and says which.

**Why the assessment and not the action queue.** The queue is his standing list
of outstanding work across the whole system, and a word one document printed is
not outstanding work until he decides it is. Queued, a first import would put a
dozen items in the queue beside its `import_session_unfinished` item — the wall
of items in place of the wall of questions. The assessment is the per-session
surface, it is already what a caller reads before answering and before
committing, and the offer is computed from the same rows the plan is computed
from. The queue keeps the case it already has: `adopt_classification_rule`, one
per answered question, which is the *after* to this *before*.

**Only rows with an open question are counted.** A row a rule of his already
settles is not evidence that he wants another rule, and counting it would make an
offer grow every month while settling nothing new.

**The offer is worded in decision 0027's shape**, and it is the first
owner-facing prose written since that decision, so it is written as an
`OwnerQuestion` — `ask` and `consequence` as two values — rather than in the
one-string shape of `OpenQuestion::prompt` beside it. The consequence is the half
that had to be earned: one answer stands for every line filed that way, this
month's and every later one, which is the whole reason the offer exists **and**
the whole of its risk. «This saves you questions» would have been the kind of
consequence 0027 refuses — true of the offer rather than of his choice between
one answer and another.

### 3. An answer may reach every like row of its own session, and no further

`answer_question` takes an `AnswerReach`. `ThisRow` is the default and is what the
call has always done. `EveryLikeRowInThisSession` records the same answer against
every question still open **in this session** that is the same decision.

**`may_generalise` is untouched, and this is not a way round it.** The two acts
are different in what they claim and in what can be seen before they take effect.
A standing rule classifies rows nobody has looked at, in months not yet imported,
including rows that will never be shown to anyone because a matched row is never
asked about — it is the owner's, it stays his, and `POST /v1/classification-rules`
is still where it is written. Settling rows inside one session touches only rows
the caller has already submitted, which the owner reads in that session's
assessment before the commit writes anything, and which he can abandon whole,
leaving the journal exactly as it was. Nothing survives the session; the reach
makes no claim about next month.

**«The same decision» is `QuestionSubject`, which is the question paired with the
direction the source stated for the row.** The question alone is not the identity,
and the failure is concrete: `question_for` builds `IsTransferInternal` for a
named counterparty in **either** direction, deliberately, because the owner may
contradict the source and the alternatives point both ways. Two rows naming one
shop, one arriving and one leaving, therefore raise byte-identical questions. An
`Answer` states a direction of its own and `ObservedRow::resolve_with` records
**that** one — so one answer carried across both would file money that arrived as
money that left, in the one figure the report is read for. The pair keeps them
two decisions. The other three questions need no such ingredient: two of them are
asked only where the direction is settled and settled the opposite way, so they
differ by variant, and the fourth is asked only where the source stated none.

**The reach is stated by the caller and never assumed.** Making the wider
behaviour automatic was the candidate the bead called «the honest middle», and it
is refused for the bead's own third consequence: a mistake made many times. An
automatic fan-out turns a call that settled one row into a call that settles fifty
without the caller choosing, and there is no moment at which anyone decided to
apply one answer to fifty rows. An explicit reach makes that a decision — and
`OpenQuestion::alike` makes it an informed one, so choosing it is not a guess
about how many rows are alike.

**A question is still raised per row, and this is decision 0012 standing.**
Raising one question per subject was the other candidate. It is refused: a
question names its row, `iaam-3ewp` established that what a person recognises a
row by is its date and its amount, and a question over thirty rows has no row to
mark. It would also take away the one identifier the answering call takes. So the
repeats stay visible as repeats — which is what `alike` publishes — and the
saving is in the answering, not in the asking.

**The wider reach is refused whole rather than in part.** Every row it would
touch is checked before anything is written, and a row the answer cannot be
recorded for refuses the call and is named. The case that arises is an answer
naming one of his own accounts which another of those rows is itself on;
`resolve` refuses a transfer to itself, and that row was never the same decision
however alike its question looked. Settling the rest and dropping that one would
be an import that files what it can and says nothing about what it could not.

**The order of the writes is chosen for recovery.** The rows the answer reaches
are answered before the row it was asked about. Every one of them is the owner's
own fact, so `iaam-77hk`'s «his fact before the derived one» is satisfied
whichever comes first among them, and the rule is still written last. What
decides the order is that answering a question twice is refused: a call that
failed half-way must be repeatable, and it is repeatable only if the question the
caller addresses is the last one written. On a repeat the session is read again
and the rows already settled are no longer open.

**One rule at most, whatever the reach**, minted from the row the caller
addressed. `matcher_for` would build the same matcher from every one of those
rows, so a rule per settled row would be one decision recorded many times.

### 4. Every surface that publishes a question publishes what may be said to it

`OpenQuestionDto` gains `alternatives` and `alike`. There are now five shapes
saying «here is a question» — the ingest verdict, the held row, the document row,
the question route and the session's assessment — and all of them render
`AnswerAlternativeDto`, whose `consequence` is read from `AnswerShape` and
written in one place.

**The stored list, not a recomputed one, and one function reads it.** A
recomputed list is what *this build* would offer for a question an older build
asked, and the answer route still measures a word against the stored list — so
the drift would surface as a caller being told to send something the server then
rejects. `stored_alternatives` is that one reader.

**The account candidates are deliberately not added.** Two answers name one of
his accounts and the question route already carries the candidates with their
titles (`iaam-boj4`). Repeating that list under every open question would publish
the owner's whole directory once per question in one response, and a single
shared list on the interpretation would be a fourth place publishing his
accounts. `needs_account` on the alternative says when they are needed, and the
per-question route is where they are.

**What the guard can and cannot hold.** The tests assert that the shapes agree
word for word and consequence for consequence, and that every word in the
vocabulary says what it decides — the second is the non-vacuity, because the
first is made of one question's three words and would not notice a word added and
left mute. Neither can hold that a sixth publisher will agree: a guard over the
set of publishers has to enumerate them, and an enumeration is the thing that
goes stale. What makes a sixth one right is that it reads `stored_alternatives`
or `AnswerShape::consequence`, and that is written where it will be read.

## Consequences

**On a first import of the statement that opened this bead**, the count goes from
one question per row to: a handful of offers, one per word the institution files
its rows under; then, for whatever those do not cover, one call per distinct
counterparty-and-direction instead of one per row.

**It is not zero, and it must not be.** No standing decision exists until he
makes one, and nothing here concludes on his behalf — §1 is the whole of that.
The first import is where he tells the system what his money does; the second is
where it already knows.

**A client that ignores every new field reads exactly what it read before.**
`settles` is absent by default and means the row it addresses; `alternatives` is
additive; `alike`, `offered_rules` and `also_settled` are omitted when empty.

**The revision fingerprint is unchanged.** `alike`, `alternatives` and
`offered_rules` are all derived from the questions and observations the
fingerprint already folds, so a session that changed in one of them changed in
something already hashed. Adding them would be hashing one fact twice.

## What this does not settle

- **`OpenQuestion::prompt` is still one string**, and decision 0027 argues that a
  question put to a person should be two so the consequence cannot be trimmed
  away. Splitting it is a change to four publishers of one string, and
  `iaam-3ewp` gave a reason for the string being one: it is the ingest verdict's
  question, the session's prompt and the queue item's reason at once. That is a
  bead of its own, and this decision writes new prose in the newer shape rather
  than converting the old prose in passing.
- **An agent settling fifty rows leaves fifty `adopt_classification_rule`
  items**, one per question, all proposing the same rule. That is the state
  before this decision as well — fifty answers already made fifty items — but the
  reach makes it reachable in one call, so it is now easy to produce. Whether the
  queue folds items proposing an identical rule into one is not decided here.
- **Whether an offer should also be raised on the counterparty a session repeats
  most** is left open. The category is the one that turns hundreds into a
  handful; a counterparty offer would be a second list with the same shape and a
  much longer tail, and it is the list `alike` already lets a caller settle
  without a standing rule at all.
