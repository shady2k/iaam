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
   link addresses the health resource.
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
boundary is drawn by the owner. And it does not hold the owner's statements —
the owner loads them himself, and the agent sees exactly what the owner has
shown it.

From this follows the thing that is easiest to violate out of the best
intentions: a missing value is asked of the owner, not filled in. A guess that
has reached the journal is indistinguishable from a fact.

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
source, the observation moment, the quality and the completeness boundary. Such
a row can be quoted to the owner verbatim.

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
