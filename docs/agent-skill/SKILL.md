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
boundary is drawn by the owner. It does not correct or retract what is already
recorded — that is the owner's act, and his credential is what the system will
accept for it. And it does not hold the owner's statements — the owner loads
them himself, and the agent sees exactly what the owner has shown it.

From this follows the thing that is easiest to violate out of the best
intentions: a missing value is asked of the owner, not filled in. A guess that
has reached the journal is indistinguishable from a fact — every report will
read it as one, and only the owner, who knows what actually happened, can
retract it.

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

The owner's answer is kept as one of his classification rules, so the same
counterparty is not asked about a second time. That is also why the answer
matters beyond the one row: it is a decision recorded in his own vocabulary, and
he can see it, change it, and retire it afterwards like any other rule.

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

## What a contour is

A contour is the set of accounts the owner considers "his portfolio". The
boundary is drawn by the owner, not by an institution. Moving money between two
accounts **inside** the contour does not change the return: it is shifting it
from one pocket to another. A transfer from an account outside the contour to
an account inside it is a contribution.

A contour has a **version**. A report always returns the version it computed
against. Two figures computed against different contour versions must not be
compared.

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

Three things follow, and each of them is a way an agent gets this wrong.

**Correction is the owner's act.** Ask him; do not attempt it. The system will
refuse an agent's credential for it, and that refusal is a limit of rights, not
an absence of the capability. What the agent may properly do is find what went
wrong, tell the owner exactly which facts are affected, and prepare the request
for him to send.

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

## An instrument's external code resolves as of a date

An instrument is named by an **external code**, not by an identifier. An ISIN,
a ticker, a MOEX `SECID`, a FIGI and a broker's internal code all serve. The
place of custody is named by a name from the owner's directory.

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

## Idempotency keys

Always send an idempotency key if you can construct one. Repeating a request
with the same key returns `duplicate` and the identifier of the first event —
that is the right answer, not an error. Without a key, sending again creates a
second event: two identical purchases on one day are a legitimate situation,
and the system has no right to merge them.

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
see what was left out. Completeness of a calculation and completeness of its
population are two statements, and only the second one says whose money was
counted.

`population.completeness` is the summary:

- `whole` — every account the system knows of is inside the report.
- `bounded` — accounts are outside it, and each of them sits in a scope the
  owner drew.
- `undecided` — accounts are outside it that no scope claims at all.

**`undecided` is not a milder `bounded`.** "Four accounts are outside this report
and nobody has decided whether they belong" is a different sentence from "four
accounts are outside this report on purpose", and only the second makes the
figures an answer about a boundary the owner chose. Each entry in
`population.outside` carries the same distinction per account, as
`outside_placed_elsewhere` or `outside_undecided`, with the account's title so
the owner can be asked about it by name.

So a report whose `population.completeness` is `undecided` is reported as what
it is: an answer about part of the owner's money, with the undecided accounts
named. Never as "the portfolio returned X".

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

The ten verdict codes and the "is the fact recorded" rule are fixed in
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
