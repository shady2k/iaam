---
name: iaam
description: IAAM personal accounting over a contour the owner draws. Works with two input channels, per-row verdicts, multi-dimensional reconciliation, categorised spending and income, and pre-tax return. Use it when asked about the portfolio, return, contributions, value, where the money went, spending by category, or data quality.
---

# IAAM — personal accounting

## Bootstrap

This file explains **meaning**. It deliberately names no route, no method and no
status code, and a guard refuses it if one appears. What a running instance
serves is answered by the instance; a document that answers instead goes stale
silently, and this one did — it told agents that implemented routes were
unimplemented, and work the system could do went undone.

Three steps, in order:

1. `/.well-known/api-catalog` (RFC 9727) returns a linkset (RFC 9264). Its
   `service-desc` link addresses the machine-readable contract; its `status`
   link addresses the health resource. Its `related` links address the rest of
   the way in: the outstanding-work queue, the scopes a report is computed over,
   and the four questions this system answers about money — each of those four
   tagged with the `goal` name that the queue's items and every report's
   confidence register also use, so a caveat naming a goal leads to the resource
   that answers it. Take addresses from that document, never from this file,
   which deliberately holds none.
2. The document behind `service-desc` is the contract: the routes, methods,
   request and response schemas and status codes this instance actually serves,
   generated from its handlers. Read it instead of asking a human which routes
   exist, which fields are required, or what a refusal will look like.
3. The actions operation declared in that contract answers what **this**
   instance needs next, computed from its own state. Each item carries the
   operation to call, its address resolved from the same contract, the fields
   already decided, and the fields still missing together with who must supply
   each. Work that queue; do not reconstruct an order of setup from memory.

A credential is not obtained through the API. It is issued at the console by
whoever runs the instance and handed to you; no call produces one. If a call is
refused for want of one, say so — there is no other route to try.

Everything below is what those three steps cannot tell you.

## The overriding rule

**Arithmetic of your own is forbidden.** Every number in your answer must be
present verbatim in the API's answer. Do not add amounts together, do not
compute percentages, do not convert currencies, do not estimate a return
"roughly".
A number that is not in the API's answer is an error — even when it is correct.

If the API refused to compute a quantity, the answer says exactly that: the
system cannot compute it, and here is why. Replacing a refusal with an estimate
of your own is the most expensive mistake that can be made here.

## The agent is an external client

The agent is not part of the system and has no access to its storage. It does
not write to the journal directly: a record is the outcome of passing ingest,
not a separate action. It does not create accounts or contours: the portfolio's
boundary is drawn by the owner. It does not rule on what is already recorded —
retracting a fact the owner holds is his act, and his credential is what the
system will accept for it. The one exception proves the rule rather than
softening it: an agent may take back an import it declared itself, while nothing
has been built on it, because that is not a ruling on the owner's history but a
return to the state before the agent acted. And it does not hold the owner's
statements — the owner loads them himself, and the agent sees exactly what the
owner has shown it.

From this follows the thing that is easiest to violate out of the best
intentions: a missing value is asked of the owner, not filled in. A guess that
has reached the journal is indistinguishable from a fact — every report will
read it as one, and only the owner, who knows what actually happened, can
retract it.

## Where an import begins, and what you are not holding

An import has a step before its first call: somebody turns a bank's export into
rows this system can read. That step is not in this API, it is not yours, and it
is the step the outstanding-work queue asks for without naming.

The owner runs a converter of his own against his own file. It knows the
export's columns, and it knows the two things an export never states: which
printed name is an account of his at another institution, and which positive row
is a merchant giving money back rather than money arriving. You are handed
neither the file nor those answers, so you cannot reproduce that step and must
not try to — reading his export to work them out is the thing the design forbids
outright, not a shortcut with a cost.

What you are handed is what he pastes and what the API answers. On rows he
pastes, submit what the source stated rather than a conclusion you reached for
it — the shape for that is the next section. Where he has a converter, give him
the command and work from the summary he brings back.

`docs/import-boundary.md` is the map: which channel writes what, who runs each,
what his converter is responsible for knowing, and where that line is drawn
wrongly today. Read it before extending an import, not before running one.

## A row you cannot classify is submitted as such

The rule above — a missing value is asked of the owner, not filled in — used to
have no way of being obeyed at intake. Every operation kind stated a conclusion:
two of them asserted which way the money went, and the third demanded the
account on the other side. A bank row printed as an amount and a word meaning
"internal to this institution" is none of those, and the only shapes on offer
forced a guess. One was made, a withdrawal was recorded as a deposit, and it had
to be retracted afterwards.

There is now a shape for the row itself. It states what the source stated and
nothing more: the account the statement is for, the source's own direction word
including the one that resolves to no direction, the amount **with the sign the
source printed**, the party the source named if it named one, the word the
source used for the operation, and the identifiers of the document and the row.
Where the source said nothing, the shape says so explicitly; absence is a
statement, not a default.

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

Some statements file a row as a movement between the owner's own accounts and
say nothing else about it: no direction, no counterparty. That claim has a field
of its own on the observation shape, beside the direction word and beside the
counterparty, and it takes two values — the source asserted the far side is one
of the owner's accounts, or it said nothing about it.

It is **stronger** than the direction word that means «internal to this
institution», which is equally true of a payment to a stranger who banks there,
and **weaker** than naming the account, which such a row does not do.

Three things follow, and the third is the one to get right.

- **Set it only where the export says so in words.** It is a transcription, like
  the direction. Deciding that a counterparty is one of the owner's accounts is
  a conclusion, it is reached against his directory on the server, and you must
  not reach it for him. A row where you set this because it seemed likely is a
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
account the row is on, that determination is made without asking anything, and
it is reported in its own words: the row is `settled` when you feed it, it
appears in the plan's list of rows settled without a fact, and its commit
verdict is `no_fact` with the determination's code beside it.

**Do not read that as a failure and do not retry it.** It is not `quarantined`,
which means a fact could not be written; nothing was written because nothing
should have been. The one thing it does explain is a batch total that is short
of the statement's own turnover with nothing wrong.

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

**Show him the sentence, not a summary of it.** The wording names the row by the
day the source dated it and the amount the source printed, with the sign the
source printed. That is there because it is what a person recognises a line on a
statement by: several rows of one month can carry the same word, name nobody,
and be described identically in every other respect, and an owner matching
questions to rows by counting down a list will eventually be off by one. A wrong
answer is not caught later — it settles the row, it may become a standing rule,
and nothing asks again. The row number beside the question is what you send
back; the day and the sum are what he reads.

**Every alternative says what answering it does to his money-flow report, and
you relay that too.** Two of these words are one keystroke apart and land in
different figures of his year: money that came in from outside is not the same
line as money of his own moving between his own accounts, and a return a
counterparty made is subtracted from what he spent rather than added to what he
received. Read him the effect beside the word. Do not compress the seven into
one sentence of your own, and do not decide that some of them are obvious: the
whole reason the effect is published per alternative is that the choice between
two of them is not a wording preference.

Where an answer names one of the owner's own accounts, the question carries the
accounts it may name, each with his own title for it and the institution holding
it. Show him those and send back the identifier of the one he picks. You never
look an account up to answer a question, and you never compose an identifier: the
one you send was in the question you are answering.

**An answer you relay settles the row and nothing beyond it.** Answering has two
halves. Disposing of the line in front of you is import mechanics, and it is
yours to do. Turning that disposal into a standing classification rule decides
rows nobody has looked at yet — including months not yet imported, and including
rows that will never be shown to anyone, because a row a rule matches is never
asked about. That second half is the owner's, it is the same act as writing a
rule directly, and under your credential the system now does only the first.
Nothing is refused and nothing goes wrong: the answer comes back saying in a
word that no rule stands, and carrying the rule it would have made, the row is
settled, and the import goes on.

That word is worth reading, because it distinguishes two things an absent rule
used to hide. A rule may have been impossible — the row named no counterparty,
carried no description and printed no word of its own, and a condition that asks
nothing matches nothing, so there is no rule for anyone to write. Or a rule was
perfectly possible and simply is not yours to write. Only the second is worth
telling the owner about, and only the second comes with the rule attached.

Say it, and hand him the remedy with it. The same counterparty will be asked
about again next month, once per import, until he records the decision with his
own credential — and the answer you relayed already holds exactly what he would
record, so he can adopt it as it stands instead of reconstructing it from the
row. When you notice a question you have relayed before, that is the thing to
say: which rule is waiting to be adopted, not a second guess at the answer.

You do not have to remember which ones are waiting. **Every rule an answer could
not write is an item in the outstanding-work queue**, one per question, kind
`adopt_classification_rule`, carrying the rule already filled in as the body of
the call that writes it. It is a recommendation rather than required work — the
row it came from is settled and no report is short of anything — and it names the
owner as the one who may send it, which you cannot. Read that item to him. The
item disappears once a standing rule of his settles a row like that one, whether
he sent the proposal as it stood or narrowed it first, so the queue is a list of
decisions still open rather than a list you have to keep yourself.

The condition a proposal asks about is **one thing**: the counterparty the row
named, or failing that the word the source used for the operation, or failing
both the description. Not all three at once — a condition joining every field the
row printed recognises that row and almost nothing else, which is a standing
decision that decides nothing. Tell him what the condition asks, because it is
the part he may want to change: a wider one settles more rows next month and can
settle one wrongly, and only he can weigh that.

A rule the owner does write is kept in his own vocabulary, and he can see it,
change it, and retire it afterwards like any other.

## An import can be held open before it is committed

Rows can also be accumulated in an **import session** instead of being recorded
one at a time. A session is opened, fed rows from one or more sources,
questioned, answered, and then either committed or abandoned.

It is not a database transaction, and the difference matters: answering a
question can take the owner days, and nothing is held open in the machine
meanwhile. What is durable is the session itself.

Two properties are worth stating plainly, because everything else follows from
them:

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
remember it.** A session that holds rows and has been neither committed nor
abandoned is an item in the outstanding-work queue, kind
`import_session_unfinished`, one per session, naming both of the calls that end
one. It is required work rather than a recommendation, because the rows it
holds are in no journal and therefore in no report, with nothing on any figure
saying so.

Read that item rather than concluding from a quiet queue that the import
landed. That conclusion is the mistake this item exists to make impossible: the
rows sat where nobody could see them, and the next act was to import the same
statement a second time. The queue's item and the session listing both say how
much is held and how much is still unanswered, so neither costs you a request
per session.

Abandoning closes the item as completely as committing. They are different acts
— one records the rows, the other says they were never facts — and both end the
session, which is what the item is about.

## What a contour is

A contour is the set of accounts the owner considers "his portfolio". The
boundary is drawn by the owner, not by an institution. Moving money between two
accounts **inside** the contour does not change the return: it is shifting it
from one pocket to another. A transfer from an account outside the contour to
an account inside it is a contribution.

A contour has a **version**. A report always returns the version it computed
against. Two figures computed against different contour versions must not be
compared.

## A closed product is retired, never dropped from the contour

The owner closes a term deposit. The bank prints two interest accruals and then a
row moving the whole balance to another of his accounts, and the product stops
existing. He asks for it to stop showing up in what he holds.

**The obvious move is the wrong one.** Making a new contour version without that
account does remove it from the asset report — and it destroys two answers on the
way. A report resolves one contour composition and applies it to every event; it
never looks membership up by an event's date. So under the narrower composition
the closing movement now has one end outside the perimeter, and the deposit's
principal is reported as money arriving from outside; and the interest, which is
a movement inside an account the composition no longer names, is not folded at
all. Not misclassified — absent. A month the owner has already read changes
underneath him, and the two figures he most cared about are the ones that change.

**What to do instead** is record a retirement on the account: the date the
product ceased. The account stays inside the contour, which is what keeps the
interest an earning and the movement that emptied it internal, and the asset
report stops carrying its row.

Read the two as what they are. A contour says **whose money is in the figures**.
A retirement says **whether the product is still there**. They are different
questions about one account, and the second is never answered with the first.

### What the retirement changes, and what it does not

From the date the product ceased:

- the asset snapshot stops publishing that account's row, and its membership of
  its cash class — **but only where every one of its figures is zero**;
- every report's `population` goes on naming the account, its `standing`
  unchanged, with the date in `covered[].retirement`;
- `population.retirement_revision` advances. That field is the second coordinate
  of an answer, beside `contour_version`: two asset snapshots over one contour
  version are answers to the same question when their retirement revisions match.

It changes nothing else. No figure moves — only an all-zero row may be dropped,
and such a row adds zero to every total. No classification changes, ever. A
snapshot taken while the product was still open is untouched. The balances answer
keeps the account's row, because that answer is what the journal holds per
account and is what a statement is reconciled against.

Nothing hides a retired account from the account list or from the outstanding-work
queue. Its money is still in every report over a period the product existed in,
so a question about it still changes a reported number. If the owner asks which of
his products still exist, read any report's `population` and keep the entries with
no `retirement`.

### A retirement never hides money

Where a retired account's figures are not all zero, the row stands, and the
report's `confidence` carries `retired_account_not_empty` naming the account.
Report that as what it is: he says the product ceased, and the journal still shows
something on it.

The usual cause is that the deposit's principal predates the months that were
imported. The account's cash figure is then movement from an unknown start rather
than a balance, the recorded movements do not sum to zero, and the row is right to
stand — the missing principal is a real hole in his cash total. What closes it is
recording the reconstructed opening (see «What to assert for a reconstructed
opening»); the retirement then removes the row on its own.

**Never propose ruling the account outside every contour to tidy this up.** That
is the scope decision, it says his money does not belong in the report at all, and
it takes the interest and the closing movement with it.

### What is refused

- a second retirement over one that already stands. Withdraw the first, then
  record the new one, so the change is a revision a reader can see rather than a
  date moved silently under snapshots already taken;
- withdrawing when nothing stands. Every accepted call advances the revision, and
  a revision that changed nothing would be a coordinate that means nothing;
- a date later than today;
- an account the owner does not hold.

Retiring an account that still holds money is **not** refused. It is his
statement about his product, and the report says where it disagrees rather than
refusing his word.

## What a category is, and what it cannot change

A contour says which accounts are the owner's. A **category** says what his
money was for. They answer different questions over the same journal, and
confusing them is the mistake this section exists to prevent.

A category is the owner's explanation, derived when a report is built from
versioned rules over a row's identity, the source's own category, its
description and its date. **The source's category is evidence, not a verdict.**
Never invent a category, a rule, or the interval a rule is valid over: that is
the owner's judgement, and a guess that has reached a report is
indistinguishable from his decision. A row no rule matches stays explicitly
undecomposed, and the report says so — never put it in a catch-all.

Categories reach spending, refunds and kinds of income; a transfer carries none
at all. A refund is subtracted from the category the money was spent in rather
than reported as income — a returned appliance is not an earning. Cashback and
interest on a balance are income, and they are reported under the owner's own
income category exactly as an outflow is reported under his spending one.
Whether a transfer crossed the contour's boundary is answered by the accounts it
touched, and that question is already settled without a category.

**Changing only a category assignment cannot change the return.** Not what was
contributed, not what was withdrawn, not the contour's value, not
`xirr_pre_tax`. Those are computed from the kind of each event, the accounts it
touched and the contour's membership, and no category, category group or income
flag enters any of them.

Say it in those words, because the neighbouring sentence is false. Changing an
event's **kind**, the accounts it touches, or the contour's membership does
change all of them. A row reclassified from a payment out to a transfer is a
different fact, not a different explanation of the same one — and it is the kind
of change that must go back through the channel the fact arrived by.

## Two channels of fact, and what makes a confirmation independent

IAAM has two independent channels through which facts arrive: parsing a broker
report, and operations received from the broker's API. Parsing API answers
lives separately from the report parsers, and that is not duplication but the
condition of independence: two reports parsed by one and the same parser do not
become a second source — the parser's error is reproduced, the fact is not
confirmed. Agreement between two different channels raises the dimension's
status to `accepted_independent`.

Both channels write facts into one append-only journal.

## A mistake is retracted, not erased

Append-only does not mean irrevocable, and an agent that reads only the first
half of that sentence will tell the owner his wrongly imported month cannot be
undone. It can. **A correction is a new fact that retracts an earlier one.** The
retracted fact stays in the journal — it is still true that it was once
recorded, and how the picture changed is itself part of the record — and every
report is computed from what is in force rather than from everything ever
written. A replacement goes further: it retracts and states what should have
stood instead.

Four things follow, and each of them is a way an agent gets this wrong.

**Correcting the owner's history is his act.** Naming a fact of his and saying
it should stop counting is a judgement about what he knows; ask him, and do not
attempt it. The system refuses an agent's credential for it, and that refusal is
a limit of rights, not an absence of the capability. What the agent may properly
do is find what went wrong, tell the owner exactly which facts are affected, and
prepare the request for him to send.

**Declare a source on everything you submit, corporate actions and offers
included.** A submission that declares nothing is recorded under an identity
nobody was ever told the name of, and nothing can name it again: the facts land,
and the only handle left on them is one event at a time. A declaration is the
account, the way the rows arrived, and a label naming this batch — a statement
period, an export file name, a run identifier. Two submissions under one label
are one import; two labels are two imports. It costs one object in the request,
and it is the whole difference between an import you can take back and one you
cannot. Corporate actions and offers had no way to declare it at all until
recently; they do now.

**Undoing your own import is yours.** An import you declared and committed —
your account, your channel, your label — you may retract, and you should, the
moment a control total tells you it was wrong. That is not a ruling on the
owner's history: it returns the journal to the state before you acted, and
nothing the owner decided is reversed by it. You still acknowledge that the rows
will stop counting, and the retraction is a journal fact like any other, so he
can see what you did.

The bound is narrow and it is checked, not trusted. It is your import, under the
label you submitted it under; every row of it is still in force; and nothing has
been built on it — no row reversed or replaced by anyone, and no balance of the
owner's reconciled against the interval those rows fall in. Anything wider is
his, and a refusal will say which of those conditions failed. Retracting twice
is refused too: the second attempt reports that the rows are already reversed,
which is also the answer to "did the first one land".

**Retracting does not free a repeat import.** Duplicate detection reads the
whole journal, and a retracted fact is still in it, so re-importing the same
rows with the mapping corrected returns `duplicate` and writes nothing. It is
the replacement, not the reversal, that re-states a row where it belonged.
Advising the owner to "just import it again" after a retraction wastes his
afternoon.

**Diagnose before proposing.** The journal can be read back per row, so the
facts a correction would affect are knowable before anything is retracted rather
than after. Name them to the owner. A correction proposed from a report's
aggregate is a guess about which rows are wrong.

**An assertion by the owner is not an independent source.** A balance he names
is what is reconciled against, not a second proof: agreement gives
`accepted_internal`, and such a quantity must not be called independently
confirmed. The difference is not cosmetic — the owner usually remembers the
balance from the same application the statement came from.

## An account is named by an identifier, and every channel reads the same ones

Wherever a row you submit names one of the owner's accounts — the account the
row is on, the far side of a transfer between two of his own, the account a
batch is declared for — the string is read the same way, whichever channel
carried the row and whether it arrived as a document or as a request body.
Three vocabularies, tried in that order:

1. **iaam's own identifier for the account**, exactly as a response printed it.
2. **The identifier the account's source prints** for it: what the institution
   writes on the statement, together with the cards and other identifiers the
   owner has attached to that same account, each read as of the row's own date.
3. **The owner's title** for the account.

Send the first where you have it, and the second where the file you are reading
prints it. Do not send the third. It resolves, and it resolves only so that
documents written before the other two existed keep parsing — a title is a
string the owner may change tomorrow, and two of his accounts may carry one
title, which is refused rather than guessed at. A file that worked last month
can stop working on a rename; an identifier cannot.

The order is not a preference, it is a rule about ties: the search stops at the
first vocabulary that recognises anything, so an identifier is never diluted by
an account whose title happens to agree with it.

A string naming none of his accounts is refused in a sentence that says which
vocabularies the field would have taken; a string naming two is refused with
both accounts in it, so he can see the collision he has to clear. That sentence
is the same across every channel — learn it from one refusal and you have
learned it for all of them.

## An instrument's external code resolves as of a date

An instrument is named by an **external code**, not by an identifier. An ISIN,
a ticker, a MOEX `SECID`, a FIGI and a broker's internal code all serve. The
place of custody is named by the owner's own title for it, and nothing else
names one: no source prints an identity for a depository, so there is no second
vocabulary there to prefer.

A code resolves **as of the operation's date**, and this is not a formality. An
ISIN changes through a corporate action: the report for last year arrives with
the old code, the exchange's export with the new one, and both are obliged to
converge on one instrument. So there is no "current" answer to the question of
which security stands behind a code — the answer exists only as of a date.

Two different refusals follow from this, and they must not be confused:

| Refusal | What happened | What to do |
|---|---|---|
| the code is unknown | there is no such security in the catalogue | create the instrument — that is the owner's work; the agent is forbidden to write to the catalogue |
| the code is known, but not on this date | the code exists, but its interval of validity does not cover the document's date | check the **document's date**: it is more likely to be wrong than the security is |

The second case is almost always a sign of corrupted data rather than of a gap
in the catalogue. A new code in a document dated before the change means that
the document, or its date, was assembled wrongly.

The instrument catalogue is **shared across all owners**: a bond issue is one
and the same for everyone, and a corrupted record will corrupt data beyond your
owner's. Writing to it with an agent token is therefore forbidden, and that is
a restriction of rights, not an absence of the capability in the system.

An instrument has three currencies, and they differ: the denomination currency,
the settlement currency and the quote currency. On replacement bonds they
diverge. The report currency is not among them — it is a property of the
report, not of the security.

An instrument's kind may be unset. That is an honest "unknown", not an error:
the valuation of such a position is marked incomplete, and the system will not
substitute something plausible for the kind.

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

There is deliberately no way to state the receiving half on its own, and the
consequence is the mistake that costs an import. When two banks each print the
movement — an outgoing row in one statement, an incoming row in the other — a
row per printed side records **two transfers**, not the two halves of one. Both
accounts then move by twice the sum, and it multiplies again with every export
that overlaps. Import the sending side and drop the receiving row.

Three properties follow, and each of them has been got wrong by a model that
still produced plausible output:

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

A fact that turned out wrong is **corrected, never resent.** The correction is
a replacement: it retracts the recorded fact and states what should have stood
instead. It is the owner's act, not yours — find the affected events, tell him
what is wrong, and prepare the request. Advising him to "send it again with the
right numbers" wastes the afternoon and leaves the journal exactly as it was.

Keys are scoped to the **owner**, not to the account or the import. Two
unrelated statements whose rows are both keyed `row-1` are one fact as far as
this is concerned, and the second is silently discarded. Build a key from the
document and the row within it, never from the row alone.

## What to assert for a reconstructed opening

A position-opening operation has an optional block of assertions — what the
owner asserts about a position that existed before the journal began. The set
of fields and their permitted values are described in the contract; what
matters here is something else.

An absent block means the owner asserted nothing. That is a legitimate state,
not a gap in the request: do not fill it with guesses. By default every field
stands at its most ignorant value. The default preserves ignorance; it does not
derive confidence from the fact that the neighbouring fields are filled in.

What is asserted here reaches the reconciliation of postings. Without an
acquisition date there is nothing to draw the ownership boundary with, and
postings on such a security land in `material_issues` as unverifiable instead
of being checked. Ask the owner for the date if he remembers it; if he does
not, leave "unknown" rather than substituting the start of the journal.

## A cash figure is not a balance until something anchors it

The journal begins when it begins. Before its first record the system knows
nothing about what was on an account, so a cash figure it computes is the sum of
the movements it has seen — a **movement from an unasserted start**, which is
not a balance and must not be reported as one. Only an assertion by the owner
about the state before the first movement anchors that sum.

**The figure names itself, so there is nothing to remember.** Every cash figure
in every report is an object with a `kind`, and the number is spelled for the
kind it is. `balance` carries a `balance`; `movement_since_unknown_start`
carries a `movement`; there is no field called `amount` on any of them. A
report cannot be skimmed for cash figures and turned into holdings, because the
holdings figure is only there when the anchor is.

This is the trap the shape closes: after a first import most of the figures look
ordinary and one may look impossible. They are equally unfounded. The
ordinary-looking ones merely passed a plausibility check the reader happened to
have, and reporting them as balances while flagging only the strange one
endorses four errors to catch one.

**A total does not average the two away.** On the asset snapshot, a class total
folds several accounts, and the owner may have anchored some of them and not
others. Such a total is `mixed`: it carries a `balance` and a `movement` and no
sum of them, because a stock added to a flow is neither. Report the two, or
report the one that answers the question asked; do not add them and call the
result a balance. For the same reason a currency whose cash is not entirely
anchored has **no entry** in the snapshot's `total` at all — no whole exists to
state, and both halves above it still say everything they know.

**Reconciliation says the same thing in its own words.** A source's balance
assertion over a fold nothing anchors is `not_comparable` with the reason
`opening_not_asserted` — not `discrepant`. The distinction matters because the
two are opposite instructions: `discrepant` means the figures disagree and one
of them is wrong; `opening_not_asserted` means there is no baseline to hold the
figure against, and nothing the owner stated is being contradicted. Never
report the second as an error he made. What lifts it is an opening assertion
reaching back to the start of the recorded history, or the import of the
history before it.

**A balance can be checked without being known.** Where nothing anchors the
start of a history, the system cannot say what an account holds — but between
two balances a source stated it can say whether the recorded movements account
for the distance. Such an outcome carries `compared` =
`change_since_stated_balance` and the date it is measured from. A `matched`
there is a statement about the interval, not about the holding: report it as
"the movements since that date add up", never as "the balance is confirmed".
A `discrepant` there is the strongest finding reconciliation makes over an
unanchored history, and it means the two stated balances and the movements
between them do not join. It is a discrepancy and not a correction: a later
statement does not overwrite an earlier one, and correcting a recorded
assertion is an explicit act with its own operation.

**Never reconstruct what the system compared.** Every reconciliation outcome
carries a `basis`: how many of the account's events were folded into the
observed figure, the first and last dates folded, and what the fold started
from. Read it before reporting anything about the outcome. The window is the
account's recorded history reaching into the interval, not the interval that
was asked about, and a balance folded over one imported month is not the
evidence a balance folded over four years is. Do not add up the owner's
operations yourself to work out what the number was made of — that is the work
this field exists to end.

A negative cash figure is reported as a fact and is never refused or hidden. It
is not by itself an error: a margin account is legitimately negative, and a card
can carry a technical overdraft. On an account where the owner would not expect
one it is a sign to check — most often a sign of exactly the missing anchor
above, since spending money that arrived before the journal began produces a
negative sum out of nothing wrong.

**Read the classification, not the sign.** A negative figure arrives with the
dates it opened and closed on and with the system's reading of why: a temporary
settlement deficit, which is ordinary and settles; financing from outside the
perimeter, which the system knows it does not reconstruct; or unclassified,
which means the reason is unknown and not that money is missing.

The last two carry a consequence, and it is the one an agent misreports. For
that account, and **only** that account, the period's tax and financial reports
are refused: the system will not compute what it cannot ground. The balance is
still there and still stated. So the refusal is about a calculation, not about
the figure, and it is not about the rest of the portfolio — every other account
in the scope is calculated as usual. Saying "the report failed" or "the balance
is unavailable" describes neither.

## A report answers about a population, and names it

Every report — balances, flow, returns — carries a `population` block: the
accounts inside the scope it was computed over, and the accounts the system
knows about that were outside it.

Read it before reading any figure. The report's own quality fields —
`data_quality`, uncovered positions, unproven bases — are about defects **inside**
the calculation. They can every one be clean while the wrong accounts were
selected, because selection happens before the calculation and nothing in it can
see what was left out. Completeness of a calculation and coverage of its
population are two statements, and only the second one says whose money was
counted.

`population.known_account_coverage` is the summary:

- `whole` — every account the system knows of is inside the report.
- `bounded` — accounts are outside it, and the owner has ruled on every one of
  them.
- `undecided` — accounts are outside it that he has not ruled on at all.

**`undecided` is not a milder `bounded`.** "Four accounts are outside this report
and nobody has decided whether they belong" is a different sentence from "four
accounts are outside this report on purpose", and only the second makes the
figures an answer about a boundary the owner chose. Each entry in
`population.outside` carries the distinction per account, with the account's
title and institution so the owner can be asked about it by name:

- `outside_by_decision` — he ruled the account outside every scope and gave a
  reason. Report it as a boundary he drew, and do not ask him again.
- `outside_placed_elsewhere` — the account sits in another scope of his. He said
  where it belongs; he did **not** say it does not belong here, so this is
  weaker than the line above and must not be reported as the same thing.
- `outside_undecided` — no scope claims it and he has ruled nothing. Nobody has
  decided whether its money belongs in these figures.

Each entry also carries `retirement` where the owner has said the product ceased.
It is a **second axis and not a fifth standing**: a closed term deposit is
normally `covered` *and* retired, because it stays inside the contour so that the
interest it paid keeps counting as an earning. Never report a retirement as an
exclusion, and never report a retired account as one the figures left out.

A deliberate exclusion never makes the population `whole`. `whole` says the
figures cover every account the system knows of; money he ruled out is still
money he has, and the honest report is "these figures cover the part he chose".

So a report whose `population.known_account_coverage` is `undecided` is reported
as what it is: an answer about part of the owner's money, with the undecided
accounts named. Never as "the portfolio returned X".

### `whole` is not "everything he has"

**Read the field's name, and report the value it actually carries.** The
denominator is the accounts this instance has been told about — `covered` and
`outside` together, published in full, by title and institution. An account of
the owner's that was never created here is in neither list, and it is not
reported as missing: it is invisible to the fold, not omitted by it.

This is not a defect that a field could fix. The system never sees a source
document. An import sends it the rows a client chose to send, so a statement of
what the document held would be that client's word republished as the system's
knowledge — and a client that silently dropped three accounts is the same client
that would supply the total. The one place the system does compare both sides is
a channel it fetched itself, and there it records the shortfall as a fact of its
own: a coverage gap, naming the refused rows and the dimensions they would have
moved.

So the check belongs to whoever holds the source, and it is a comparison, not a
lookup:

- Before reporting coverage, read `covered` and `outside` against the accounts
  the source actually holds. Seven accounts in an export and four in the two
  lists is a report that will answer `whole` and mean four.
- An account in the source that is in neither list has never been created here.
  Say so as that — "the system holds no account for this one" — and offer to
  create it. It is a different sentence from any of the three `outside`
  standings, and the report cannot make it for you.
- Never report `known_account_coverage: whole` as "this covers everything he
  has". It covers everything the system was told about, which is the claim the
  field's name makes and the only one it can support.

## How to read the return report

The report returns what was contributed and withdrawn over the whole history,
the contour's value as of the report date, the pre-tax return, the rules
applied and a `data_quality` block.

**The report's period is the whole history.** A return over an arbitrary
interval is not computed at this stage: it would need the value at the start of
the interval, and that is known only as of the report date.

**Call `xirr_pre_tax` the pre-tax return.** Not "the return", not "how much was
earned". Taxes are not yet computed in the system, and the difference can reach
13–15 % of the result.

## What an unconfirmed posting does and does not mean

The report distinguishes two things that look alike and must never be reported
alike.

**A payment was not confirmed** — the owner did hold the security that day, the
waiting period has expired, and no crediting fact is in the journal. That is a
defect: tell the owner the date, the instrument, the account and the kind of
payment — a coupon or a return of principal — and ask him to load the statement
for that period.

**There is nothing to reconcile with** — no conclusion is possible because the
evidence is missing. **This is not a claim that money went missing.** Where the
reason is simply that the journal begins later, it is not a defect at all:
there is nothing to load.

Several equally unprovable payments for one account-and-instrument pair collapse
into a single problem carrying a count and date bounds, because a cause at the
level of the source is repaired by one action. Do not expand it back into a list
and do not report each date to the owner separately.

**Never call `provisional` an error.** It means no independent confirmation has
arrived yet, which is an ordinary state of a correct journal.

**And never wait for a reconciliation verdict.** Three of the eleven codes —
`accepted`, `discrepancy` and `needs_reconciliation` — are published and produced
by nothing, which each one's own published sentence now says. The three are not a
backlog: they are the reconciliation ones, and the boundary is the point. A
verdict answers one write, while reconciliation is a property of an account, a
dimension and an interval, folded when a report is read and moved by evidence
that arrives later. The eight codes you will actually see are the ones about a
row.

Read each of the three where it is answered, and never from a row's verdict:

- **`accepted`** — confirmation is in the data quality block, as
  `accepted_internal` or `accepted_independent`. The absence of `accepted` from
  a row's verdict says nothing about it either way.
- **`discrepancy`** — a batch that disagrees with the control section its own
  source printed is named figure by figure, with both numbers and the
  difference, in the assessment an import session publishes; read it before you
  commit, and read it before you override the disagreement, because overriding
  is a sentence you are only entitled to write after reading them. A
  disagreement the journal holds is reported by the data quality block as
  `discrepant` and by the action queue as `discrepancy_unresolved`, which
  carries what settles it.
- **`needs_reconciliation`** — nothing is ever declined because the owner has
  not named a balance. Rows are recorded, and the need for the figure is derived
  from them afterwards. Ask for it from the action queue's
  `provide_control_assertion`, which names the account, the interval and which
  end of it the balance is wanted at — and answer the opening point before the
  closing one, because a closing balance compared against a sum accumulated from
  an unasserted start reports a discrepancy that is only the missing opening
  balance.

## A fact can be quoted, a derived value cannot

The key rate, an FX rate and a price as of a date are served as reference
facts: every row carries the value, the date or the interval's boundaries, the
source, the observation moment and the quality. Such a row can be quoted to the
owner verbatim.

The **completeness boundary** is carried by the answer, not by a row, because it
is one fact about the whole answer rather than a property each row could differ
in. That is what lets an answer with no rows still say something: an empty series
whose boundary is a date means the instance has this series and nothing to report
in the interval you asked about, and an empty series whose boundary is absent
means it holds nothing for the series at all. Those are different situations and
must not be reported alike — the second is not evidence that no rate existed.

Adding them up, recomputing them and deriving a return from them is not
allowed. Any derived quantity is taken from the report whole — otherwise it
becomes your arithmetic rather than the system's answer.

For prices, distinguish three things: the effective quotation basis, the basis
exactly as the source recorded it, and the machine status of how well that
basis is proven. Their agreement is not a given, and a divergence between them
means the source contradicts itself. For the key rate, an interval's boundary
may be marked as inferred: the source gave only trading days, and the exact
effective date fell between them.

When the API refuses because of request frequency, lower the frequency rather
than repeating immediately.

## What here is checked

The eleven verdict codes and the "is the fact recorded" rule are fixed in
`crates/iaam-ingest/src/verdict.rs` and its tests. The grounds of independence,
the statuses by dimension, the four shares of `nav_coverage`, the recomputation
of history and the §11 perimeter are checked by the `iaam-core` tests in
`reconciliation_grounds.rs`, `reconciliation_ledger.rs`, `data_quality.rs`,
`perimeter.rs` and the golden scenarios.

What is not here is checked by its absence:
`scripts/check-agent-skill.sh` refuses if a route, a method or an HTTP status
code appears in this file. A claim about availability that cannot be written
down cannot go stale.

## What the system does not do

It does not compute taxes, does not compute TWR, a value series or a return
over an arbitrary sub-interval, does not implement the economics of shorts,
margin and derivatives, and does not recover a lost encryption key from a
single database. The price and the FX rates for a calculation must be supplied
by the input data or by the owner.

What the system can do **now** is a question for the system itself, not for
this file: the contract and the action queue answer it.
