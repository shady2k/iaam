# Reading the reports

The process is in `SKILL.md`, and this file is what it points to before any
figure is quoted back to the owner. The overriding rule from there holds over
every line below: arithmetic of your own is forbidden, and a number that is not
in the API's answer is an error even when it is correct.

## Two channels of fact, and what makes a confirmation independent

Facts arrive through two independent channels: a broker report that is parsed,
and operations received from the broker's API. Only agreement **between** the two
raises a dimension's status to `accepted_independent`. Two reports read the same
way are not a second source — the same error is reproduced, and the fact is not
confirmed, so never report a reloaded statement as confirmation. Both channels
write into one append-only journal.

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

This is the trap the shape closes: after a first import most figures look
ordinary and one may look impossible, and they are equally unfounded. Reporting
the ordinary-looking ones as balances while flagging only the strange one
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
`opening_not_asserted` — not `discrepant`. The two are opposite instructions:
`discrepant` means the figures disagree and one of them is wrong;
`opening_not_asserted` means there is no baseline to hold the figure against, and
nothing the owner stated is being contradicted. Never report the second as an
error he made. What lifts it is an opening assertion reaching back to the start
of the recorded history, or the import of the history before it.

**A balance can be checked without being known.** Between two balances a source
stated, the system can say whether the recorded movements account for the
distance even where nothing anchors the start. Such an outcome carries `compared`
= `change_since_stated_balance` and the date it is measured from. A `matched`
there is a statement about the interval, not the holding: report it as "the
movements since that date add up", never as "the balance is confirmed". A
`discrepant` there means the two stated balances and the movements between them
do not join. It is a discrepancy and not a correction: a later statement does not
overwrite an earlier one, and correcting a recorded assertion is an explicit act
with its own operation.

**Never reconstruct what the system compared.** Every reconciliation outcome
carries a `basis`: how many of the account's events were folded into the observed
figure, the first and last dates folded, and what the fold started from. Read it
before reporting anything about the outcome. The window is the account's recorded
history reaching into the interval, not the interval that was asked about, and a
balance folded over one imported month is not the evidence one folded over four
years is. Do not add up the owner's operations yourself to work out what the
number was made of.

A negative cash figure is reported as a fact and never refused or hidden. It is
not by itself an error: a margin account is legitimately negative, and a card can
carry a technical overdraft. On an account where the owner would not expect one
it is a sign to check — most often of the missing anchor above, since spending
money that arrived before the journal began produces a negative sum out of
nothing wrong.

**Read the classification, not the sign.** A negative figure arrives with the
dates it opened and closed on and with the system's reading of why: a temporary
settlement deficit, which is ordinary and settles; financing from outside the
perimeter, which the system knows it does not reconstruct; or unclassified,
which means the reason is unknown and not that money is missing.

The last two carry a consequence agents misreport. For that account, and **only**
that account, the period's tax and financial reports are refused: the system will
not compute what it cannot ground. The balance is still there and still stated.
The refusal is about a calculation, not about the figure, and not about the rest
of the portfolio — every other account in the scope is calculated as usual.
Saying "the report failed" or "the balance is unavailable" describes neither.

## A report answers about a population, and names it

Every report — balances, flow, returns — carries a `population` block: the
accounts inside the scope it was computed over, and the accounts the system
knows about that were outside it.

Read it before reading any figure. The report's own quality fields —
`data_quality`, uncovered positions, unproven bases — are about defects **inside**
the calculation, and can every one be clean while the wrong accounts were
selected. Only the population says whose money was counted.

`population.known_account_coverage` is the summary:

- `whole` — every account the system knows of is inside the report.
- `bounded` — accounts are outside it, and the owner has ruled on every one of
  them.
- `undecided` — accounts are outside it that he has not ruled on at all.

**`undecided` is not a milder `bounded`.** "Accounts are outside this report and
nobody has decided whether they belong" is a different sentence from "accounts
are outside this report on purpose", and only the second makes the figures an
answer about a boundary the owner chose. Each entry in `population.outside`
carries the distinction per account, with title and institution so he can be
asked about it by name:

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

No field can fix this: the system never sees what a client chose not to send, and
a document it reads itself still says nothing about an account that document
never mentions. The one place it compares both sides is a channel it fetched
itself, where it records the shortfall as a coverage gap naming the refused rows
and the dimensions they would have moved.

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
defect: tell him the date, the instrument, the account and the kind of payment —
a coupon or a return of principal — and ask him to load the statement for that
period.

**There is nothing to reconcile with** — no conclusion is possible because the
evidence is missing. **This is not a claim that money went missing**, and where
the journal simply begins later it is not a defect at all.

Several equally unprovable payments for one account-and-instrument pair collapse
into a single problem carrying a count and date bounds, because one action
repairs the cause. Do not expand it back into a list of dates for the owner.

**Never call `provisional` an error.** It means no independent confirmation has
arrived yet, which is an ordinary state of a correct journal.

**And never wait for a reconciliation verdict.** Three of the eleven codes —
`accepted`, `discrepancy` and `needs_reconciliation` — are published and produced
by nothing, which each one's own sentence says. They are not a backlog: a verdict
answers one write, while reconciliation is a property of an account, a dimension
and an interval, folded when a report is read and moved by later evidence. The
eight codes you will actually see are the ones about a row.

Read each of the three where it is answered, and never from a row's verdict:

- **`accepted`** — confirmation is in the data quality block, as
  `accepted_internal` or `accepted_independent`. The absence of `accepted` from
  a row's verdict says nothing about it either way.
- **`discrepancy`** — a batch that disagrees with the control section its own
  source printed is named figure by figure, with both numbers and the
  difference, in the assessment an import session publishes; read it before you
  commit, and before you override the disagreement. A disagreement the journal
  holds is reported by the data quality block as `discrepant` and by the action
  queue as `discrepancy_unresolved`, which carries what settles it.
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

The **completeness boundary** is carried by the answer, not by a row, which is
what lets an answer with no rows still say something: an empty series whose
boundary is a date means the instance has this series and nothing to report in
the interval you asked about, and an empty series whose boundary is absent means
it holds nothing for the series at all. The second is not evidence that no rate
existed.

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
