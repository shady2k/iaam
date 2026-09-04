# Importing: rows, questions and sessions

The process is in `SKILL.md`, and this file is what it points to once a document
has been conveyed and there are rows to dispose of. Two rules from there hold
over every section below and are not repeated in each: you may carry the owner's
document to his instance and you may not interpret it, and a question about his
money is answered by him and never by you.

## A row you cannot classify is submitted as such

Every operation kind states a conclusion: two assert which way the money went,
and the third demands the account on the other side. A bank row printed as an
amount and a word meaning "internal to this institution" is none of those, and
submitting it as one is the guess that recorded a withdrawal as a deposit.

There is a shape for the row itself. It states what the source stated and nothing
more: the account the statement is for, the source's own direction word including
the one that resolves to no direction, the amount **with the sign the source
printed**, the party the source named if it named one, the word the source used
for the operation, and the identifiers of the document and the row. Where the
source said nothing, the shape says so explicitly; absence is a statement, not a
default.

**Use it whenever you have not concluded, and do not use it when you have.** A
row you can read is submitted as what it is, and should be — the shape is not a
safer default, it is the truthful answer to a different question. Submitting an
observation for a row whose direction the source plainly gave throws away
evidence and puts a question to the owner that the statement already answered.

What comes back for a row the system cannot settle is a verdict saying so, and
**nothing is recorded**: the balance does not move on a guess. The system settles
what it can without asking — a counterparty it recognises as one of the owner's
own accounts is an internal transfer, and a row matching a rule the owner has
already written is classified by that rule. Only what neither settles becomes a
question.

### When the source says whose account the far side is

Some statements file a row as a movement between the owner's own accounts and say
nothing else: no direction, no counterparty. That claim has a field of its own on
the observation shape, taking two values — the source asserted the far side is
one of the owner's accounts, or it said nothing about it. It is **stronger** than
the direction word meaning «internal to this institution», which is equally true
of a payment to a stranger who banks there, and **weaker** than naming the
account, which such a row does not do.

Three things follow, and the third is the one to get right.

- **Set it only where the export says so in words.** It is a transcription, like
  the direction. Deciding that a counterparty is one of the owner's accounts is
  a conclusion, reached against his own directory of accounts, and you must not
  reach it for him. A row where you set this because it seemed likely is a
  movement that will never appear as spending and never leave the perimeter.
- **It carries no direction, and none is inferred from it.** A row that asserts
  it and states no direction is recorded as a movement between the owner's own
  accounts with the far side unnamed, posting nothing, and **no question is
  raised**. A row that also states a direction posts one leg — and still not as
  money leaving the perimeter.
- **It does not decide which of the owner's accounts.** That is settled later,
  by the far side's own statement, or not at all.

### A row that is settled by producing nothing

Two payment instruments over one underlying account are one account, with the
second identifier recorded as an alias. Money moving between them changes no
balance and has no second leg, so the honest record is **no fact at all**.

Where the identifier the source printed for the far side resolves to the very
account the row is on, that is determined without asking anything and reported in
its own words: the row is `settled` when you feed it, it appears in the plan's
list of rows settled without a fact, and its commit verdict is `no_fact` with the
determination's code beside it.

**Do not read that as a failure and do not retry it.** It is not `quarantined`,
which means a fact could not be written; nothing was written because nothing
should have been. It does explain a batch total short of the statement's own
turnover with nothing wrong.

## A question is a thing, not a sentence

The question that comes back is a durable resource with an identifier, and it
outlives the response that carried it. If you lose the response, the question is
still there and can be found again; the answer is a later call naming the
question, which is what makes it possible for the owner to take a day over it.

Every question publishes the answers it admits, and an answer it does not admit
is refused rather than interpreted. **Never answer one yourself.** Which way the
money went and whose account was on the other side are facts about the owner's
affairs; you may show him the question and the alternatives, and relay what he
says.

**Name the row by the day the source dated it and the amount the source printed,
with the sign it printed.** That is what a person recognises a line on a
statement by: several rows of one month can carry the same word, name nobody, and
be described identically in every other respect, and an owner matching questions
to rows by counting down a list will eventually be off by one. A wrong answer is
not caught later — it settles the row, it may become a standing rule, and nothing
asks again. The row number beside the question is what you send back; the day and
the sum are what he reads.

**Every alternative says what answering it does to his money-flow report, and
you relay that too.** Two of these words are one keystroke apart and land in
different figures of his year: money that came in from outside is not the same
line as money of his own moving between his own accounts, and a return a
counterparty made is subtracted from what he spent rather than added to what he
received. Read him the effect beside the word. Do not compress the seven into one
sentence of your own, and do not decide that some of them are obvious.

The words and their effects travel with the question on every surface that
publishes one, the session's own assessment included. If you find yourself
hunting for them elsewhere, you are reading something stale.

Where an answer names one of the owner's own accounts, the accounts it may name
travel with the question — each with his own title for it and the institution
holding it. Show him those and send back the identifier of the one he picks. You
never look an account up to answer a question, and you never compose an
identifier: the one you send came with the question you are answering. In a
session's assessment the list is published once for the whole assessment, not per
question; what to drop from it for the row in front of you is below.

**An answer you relay settles the row and nothing beyond it.** Disposing of the
line in front of you is import mechanics and is yours to do. Turning that
disposal into a standing classification rule decides rows nobody has looked at
yet, including months not yet imported and rows that will never be shown to
anyone, because a row a rule matches is never asked about. That half is the
owner's, and under your credential the system does only the first. Nothing is
refused: the answer comes back saying in a word that no rule stands and carrying
the rule it would have made, the row is settled, and the import goes on.

Read that word. A rule may have been **impossible** — the row named no
counterparty, carried no description and printed no word of its own, and a
condition that asks nothing matches nothing. Or a rule was perfectly possible and
simply is **not yours to write**. Only the second is worth telling him about, and
only the second comes with the rule attached: the same counterparty will be asked
about again next month, once per import, until he records the decision with his
own credential, and the answer you relayed already holds what he would record.

You do not have to remember which ones are waiting. **Every rule an answer could
not write is an item in the outstanding-work queue**, one per question, kind
`adopt_classification_rule`, carrying the rule already filled in as the body of
the call that writes it. It is a recommendation rather than required work — the
row it came from is settled and no report is short of anything — and it names the
owner as the one who may send it, which you cannot. Read that item to him. It
disappears once a standing rule of his settles a row like that one, however he
worded it.

The condition a proposal asks about is **one thing**: the counterparty the row
named, or failing that the word the source used for the operation, or failing
both the description. Not all three at once — a condition joining every field the
row printed recognises that row and almost nothing else. Tell him what the
condition asks, because it is the part he may want to change: a wider one settles
more rows next month and can settle one wrongly, and only he can weigh that. A
rule he does write is kept in his own vocabulary, and he can see, change and
retire it afterwards.

### One decision, many lines

A statement names one shop on thirty lines and every one is the same question.
Each unanswered question lists the other rows of that session raising **the same
decision**, so you never work the grouping out by reading the prompts. Empty
means the decision is asked once.

Two rows are the same decision only when the source also said the money ran the
same way. Your answer states a direction and the journal records it, so carrying
an answer meant for a payment onto a line where the money arrived files an
arrival as a departure — in the very figure his report is read for. The system
will not group those two, and neither should you.

An answer can be told to reach all of them. Say so on the answer, and every
question still open in that session which is the same decision is settled with
it, in one call. The response names the other rows it settled; telling him which
row he decided and which were decided with it is your job, not the response's.

**Read what that does and does not claim.** It settles rows already in one
session — rows he reads in the assessment before anything is committed, and which
he can abandon whole. It writes no standing rule and says nothing about next
month; that is still his to write. If a single row of the ones it would reach
cannot take the answer, nothing is written at all and the refusal names that row.
The commonest cause is an answer naming one of his own accounts that one of those
rows is itself on, and it means what it says: that row was never the same
decision.

### What a first import can settle without asking him about every line

A statement he has never imported has no rules of his behind it, so nearly every
line naming a party becomes a question. Most have the same answer, and the
document says so: an institution files hundreds of counterparties under a dozen
of its own words.

The assessment publishes them in `offered_rules`. Per word the institution filed
still-unanswered rows under, it gives the count, the rows, and the question to
put to him in two parts. Read both to him: the second is where the risk is,
because one answer settles every line filed that way, this month's and every
later one.

What it publishes is the **condition** and never the outcome. What the lines have
in common is a fact about his document; what they *are* is his decision, and it
is one only he can send. Do not fill it in, and do not read the institution's own
word to him as though it were an answer — a bank calling something a transfer is
a hint he may map or override, not a ruling on what his money did.

A document whose reader transcribes no such word offers nothing here, and the
list is simply empty. That is the truthful answer, not a failure: the reader
names that column or it does not exist.

**A word whose rows are not one thing is withheld, and it says so.** Institutions
file more than one kind of movement under a single word, and a rule on such a
word would settle rows that are not the same decision — quietly, and for every
month after this one. So the assessment does not offer a rule on it. It names it
in `withheld_offers` instead, with what the word covers and one sentence saying
why it was held back, and offers nothing that could be sent as it stands.

That is what makes the other list safe. **A word missing from `offered_rules` is
not an oversight**, and you need not audit the two lists against each other: what
is offered is offered because its rows are one decision, and what is withheld is
named. Read the withheld ones to him as what they are — a word his bank uses for
more than one thing — and work them through with an answer that reaches every
like row of the session, which is the mechanism two sections above.

**Each open question carries its row as values, not only as a sentence.**
`printed` beside the question holds the account the row is on, the amount with
the sign the source printed, the currency, the date, the direction, the
counterparty and the word the source filed it under. Take every value you act on
from there. **Do not recover them from the prose**: that wording is prose and is
rewritten, so anything parsing it is already stale — and parsing a format nobody
guaranteed is the act «Where an import begins: carry the document, do not read
it» forbids in `SKILL.md`.

**And the accounts an answer may name are published once for the whole
assessment.** `interpretation.answer_accounts` is the list, with his own title for
each and where it is held; it does not change from question to question, so
fetching it per question buys nothing. What does change you can derive: an
account is not the other side of itself, so drop the one `printed.account` names
before showing him the rest.

## An import can be held open before it is committed

Rows can also be accumulated in an **import session** instead of being recorded
one at a time. A session is opened, fed rows from one or more sources,
questioned, answered, and then either committed or abandoned. It is not a
database transaction: answering a question can take the owner days, nothing is
held open in the machine meanwhile, and what is durable is the session itself.

Everything else follows from two properties:

- **Nothing in a session is in the journal, and nothing in the journal is
  provisional.** A session's rows are not facts yet. They become facts at
  commit, all at once, and at no other moment.
- **Abandoning a session leaves the journal exactly as it was.** There is nothing
  to retract, because nothing was recorded. This is the difference between
  changing your mind before a commit and correcting a fact afterwards — the
  first costs nothing, and the second is a retraction that every report the
  owner has already read will stop counting.

A session refuses to commit while any of its questions is unanswered. That
refusal is the point of it: committing with a question open records exactly the
guess the question exists to prevent.

**A session you opened and did not end is not finished, and you do not have to
remember it.** A session holding rows that has been neither committed nor
abandoned is an item in the outstanding-work queue, kind
`import_session_unfinished`, one per session, naming both calls that end one. It
is required work rather than a recommendation, because the rows it holds are in
no journal and therefore in no report, with nothing on any figure saying so.

Read that item rather than concluding from a quiet queue that the import landed —
that conclusion is how the same statement gets imported twice. The item and the
session listing both say how much is held and how much is unanswered, so neither
costs a request per session. Abandoning closes the item as completely as
committing: both end the session, which is what the item is about.

A report can still be asked what the answer would look like with a session's
rows in it, and it will not do so unless asked. Every report takes a `held`
parameter — absent means the journal alone, `all` means the journal plus every
open session, and otherwise it is the session identifiers the answer is to
include. The answer carries `held_rows` back: which sessions it folded, which
reading of each one it folded, and how many held rows produced no fact at all.

Read that last count before quoting any figure computed this way. A row whose
question is unanswered becomes nothing, so such a figure is short by exactly the
rows nobody has ruled on. Quote the count beside the number, never instead of it.

Naming a session that has already committed is allowed and changes nothing: its
rows are in the journal, so the answer says `already_in_journal` and counts them
once. Naming one that was abandoned changes nothing either, and says so.

## How an amount is stated

**Amounts are always positive.** The sign is carried by the kind of operation:
a contribution and a withdrawal are different kinds, not one sum with two
signs. Amounts travel as strings rather than numbers, because a JSON number
loses precision and an amount in the journal is a fact.

**An amount's scale must not exceed the currency's minor unit.** A surplus
digit after the separator is refused, not rounded: rounding at the input
substitutes a convenient number for a fact.

## A transfer between the owner's own accounts is one row, not two

A transfer operation names the account the money left and the account it
arrived at, and it is **submitted once, from the sending side**. The system
writes both movements from that single row: the sending account is debited and
the receiving account is credited, in one fact that holds both accounts.

There is deliberately no way to state the receiving half on its own. When two
banks each print the movement — an outgoing row in one statement, an incoming row
in the other — a row per printed side records **two transfers**, not the two
halves of one, and both accounts move by twice the sum. Import the sending side
and drop the receiving row.

Three properties follow, each got wrong before by a model that still produced
plausible output:

- **The amount is positive, like every other amount.** A negative amount is
  refused, not read as "the outgoing leg". Direction is carried by the two
  accounts, so the sign has nothing left to say.
- **The two accounts must differ.** A transfer to itself moves nothing and is
  refused on the destination field.
- **A transfer is not a deposit plus a withdrawal.** Those two say the money
  crossed the boundary of the owner's accounts, and a report counts them as
  money entering and leaving. A transfer says it stayed inside and merely
  moved. Recording a transfer as a pair overstates both what came in and what
  went out, in the same month.

If you cannot tell whether the other side is one of the owner's own accounts,
that is exactly the row you submit as an observation and let the owner answer.

## Idempotency keys

Always send an idempotency key if you can construct one. Repeating a request
with the same key returns `duplicate` and the identifier of the first event —
that is the right answer, not an error. Without a key, sending again creates a
second event: two identical purchases on one day are a legitimate situation,
and the system has no right to merge them.

**A key names a fact, not a slot, and this is where agents lose an afternoon.**
The key is matched before anything in the body is looked at. So a row you
**corrected** and resent under the key you used the first time is answered
`duplicate` and writes nothing: the journal keeps the wrong number, and the
answer looks like success. Re-sending is not a retraction — nothing on the
import path retracts anything, so it is a no-op rather than a
retract-and-add.

A fact that turned out wrong is **corrected, never resent** — see «A mistake is
retracted, not erased» in `correcting.md`. Advising him to "send it again with
the right numbers" leaves the journal exactly as it was.

Keys are scoped to the **owner**, not to the account or the import. Two
unrelated statements whose rows are both keyed `row-1` are one fact as far as
this is concerned, and the second is silently discarded. Build a key from the
document and the row within it, never from the row alone.

## What to assert for a reconstructed opening

A position-opening operation has an optional block of assertions — what the owner
asserts about a position that existed before the journal began. The fields and
their permitted values are in the contract.

An absent block means the owner asserted nothing. That is a legitimate state, not
a gap in the request: do not fill it with guesses. Every field defaults to its
most ignorant value, and the default does not derive confidence from the
neighbouring fields being filled in.

What is asserted here reaches the reconciliation of postings. Without an
acquisition date there is nothing to draw the ownership boundary with, and
postings on such a security land in `material_issues` as unverifiable instead
of being checked. Ask the owner for the date if he remembers it; if he does
not, leave "unknown" rather than substituting the start of the journal.
