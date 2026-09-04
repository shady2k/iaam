# The money's shape and the perimeter

The process is in `SKILL.md`, and this file is what it points to when the
question is about the owner's money itself: whose it is, whether the product
still exists, what a spend was for, and which account or which instrument a
string names. The boundary from there holds over all of it — the perimeter is
drawn by the owner, and a contour, an account, a category or an instrument is
never something an agent decides or creates on his behalf.

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

The owner closes a term deposit and asks for it to stop showing up in what he
holds.

**The obvious move is the wrong one.** A new contour version without that account
does remove it from the asset report, and destroys two answers on the way: a
report resolves one contour composition and applies it to every event, never
looking membership up by an event's date. Under the narrower composition the
closing movement has one end outside the perimeter, so the deposit's principal is
reported as money arriving from outside; and the interest, inside an account the
composition no longer names, is not folded at all — absent, not misclassified. A
month he has already read changes underneath him.

**What to do instead** is record a retirement on the account: the date the
product ceased. The account stays inside the contour, which keeps the interest an
earning and the movement that emptied it internal, and the asset report stops
carrying its row.

A contour says **whose money is in the figures**. A retirement says **whether the
product is still there**. The second is never answered with the first.

### What the retirement changes, and what it does not

From the date the product ceased:

- the asset snapshot stops publishing that account's row, and its membership of
  its cash class — **but only where every one of its figures is zero**;
- every report's `population` goes on naming the account, its `standing`
  unchanged, with the date in `covered[].retirement`;
- `population.retirement_revision` advances. That field is the second coordinate
  of an answer, beside `contour_version`: two asset snapshots over one contour
  version are answers to the same question when their retirement revisions match.

It changes nothing else. No figure moves — only an all-zero row may be dropped.
No classification changes, ever. A snapshot taken while the product was still
open is untouched. The balances answer keeps the account's row, because that
answer is what the journal holds per account and is what a statement is
reconciled against. Nothing hides a retired account from the account list or from
the outstanding-work queue. If the owner asks which of his products still exist,
read any report's `population` and keep the entries with no `retirement`.

### A retirement never hides money

Where a retired account's figures are not all zero, the row stands, and the
report's `confidence` carries `retired_account_not_empty` naming the account.
Report that as what it is: he says the product ceased, and the journal still shows
something on it.

The usual cause is that the deposit's principal predates the months that were
imported: the account's cash figure is then movement from an unknown start rather
than a balance, and the row is right to stand.

**The caveat names its own remedies, in the order to consider them: read
`closed_by` rather than this paragraph.** The first is the reconstructed opening
(see «What to assert for a reconstructed opening» in `importing.md`), because
that is the ordinary cause; the retirement then removes the row on its own. The
same three calls, each with the body it wants, are also an outstanding-work item
of kind `retired_account_not_empty`, which disappears once the account's figures
are zero, however they got there.

**If instead the queue carries `retirement_not_assessed`, nobody could work out
whether the retirement took effect.** His effective journal does not fold, so the
item that would have carried the answer is absent because it could not be
computed — not because there is nothing outstanding. The asset snapshot refuses
for the same reason. Do not report the retirement as done and do not report it as
outstanding; report that it could not be checked, quote what the item says
refused, and put the correction it names to him.

One thing the structure cannot warn you about: the same account usually carries
`running_cash_sum` as well, and **its** remedy is not this one's. An owner-balance
assertion is checked against the fold rather than added to it, so recording one
here relabels a figure of minus the principal as a balance instead of removing
it.

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
versioned rules over a row's identity, the source's own category, its description
and its date. **The source's category is evidence, not a verdict.** Never invent
a category, a rule, or the interval a rule is valid over. A row no rule matches
stays explicitly undecomposed, and the report says so — never put it in a
catch-all.

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
prints it. Do not send the third. It resolves only so that documents written
before the other two existed keep parsing — a title is a string the owner may
change tomorrow, and two of his accounts may carry one title, which is refused
rather than guessed at.

The order is a rule about ties, not a preference: the search stops at the first
vocabulary that recognises anything, so an identifier is never diluted by an
account whose title happens to agree with it.

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
ISIN changes through a corporate action: last year's report arrives with the old
code, the exchange's export with the new one, and both must converge on one
instrument. There is no "current" answer to which security stands behind a code.

Two different refusals follow from this, and they must not be confused:

| Refusal | What happened | What to do |
|---|---|---|
| the code is unknown | there is no such security in the catalogue | create the instrument — that is the owner's work; the agent is forbidden to write to the catalogue |
| the code is known, but not on this date | the code exists, but its interval of validity does not cover the document's date | check the **document's date**: it is more likely to be wrong than the security is |

The second case is almost always corrupted data rather than a gap in the
catalogue: a new code in a document dated before the change means the document,
or its date, was assembled wrongly.

The instrument catalogue is **shared across all owners**, so a corrupted record
corrupts data beyond your owner's. Writing to it with an agent token is
forbidden — a restriction of rights, not an absence of the capability.

An instrument has three currencies, and they differ: the denomination currency,
the settlement currency and the quote currency. On replacement bonds they
diverge. The report currency is not among them — it is a property of the
report, not of the security.

An instrument's kind may be unset. That is an honest "unknown", not an error:
the valuation of such a position is marked incomplete, and the system will not
substitute something plausible for the kind.
