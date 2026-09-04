# 0014. A closed product and a reporting perimeter are two axes

Date: 2026-09-04 · Status: proposed · Beads: `iaam-gua5`

## Context

A statement carried, for one term-deposit product, two interest accruals and
then a closing row moving the whole balance to another account of the owner's.
After that the product does not exist.

Three things are wanted at once, and the system could deliver any two:

1. the interest counted as what the capital earned, not as money arriving from
   outside;
2. the returned principal counted as a movement between the owner's own
   accounts;
3. the closed product gone from population and asset-class reports.

**Keeping the account in the contour** gives the first two. `classify` sees a
`CashTransfer` between two members and answers `Internal`; the `Income` on the
deposit is `WithinAccount` on a member and answers `Internal`, so it reaches
`earned_by_capital`. What it also gives is a row for the account in every asset
snapshot for ever, with every figure zero — a shell in the class total, in the
account list and in the whole.

**Dropping the account from the next contour version** removes the shell and
destroys both of the others. `resolve_contour` hands **one** definition to every
event, and nothing looks membership up by an event's date, so the closing
`CashTransfer { from: term, to: main }` becomes `(false, true)` → `ExternalIn`
and its principal lands in `came_in`, while each earlier `Income` on the deposit
is `WithinAccount` on a non-member and classifies `Irrelevant` — it never
reaches `earned_by_capital` at all. Not misclassified: absent.

That second behaviour is **not a defect in `classify`**. Asking for a contour
version legitimately means "restate that history through this perimeter", and
every report states the version it used. Its narrowness is filed separately as
`iaam-m9xp` and is not what this decision is about.

What the model was missing is a distinction, not a fix. **The perimeter a
calculation folds over and the inventory of products the owner still has are two
questions**, and the system had only the first.

## Decision

### 1. A retirement is its own declaration

```rust
pub struct AccountRetirement {
    pub account: AccountId,
    pub effective_on: Date,
}
```

The owner's statement that one product ceased to exist, on a date in his own
history. Two fields, and each absence is deliberate:

- **No reason.** A scope exclusion carries one because "outside every contour"
  is a judgement a year later cannot reconstruct. "This product no longer
  exists" is a fact, and the date is the whole of it.
- **No kind, term or rate.** Those belong to a deposit contract (epic E3.5). A
  contract states what a product *is*; this states that it *ended*.
- **No moment of recording.** The revision below carries "when" in the only
  sense a report can compare.

It is given its own declaration rather than a value added to `CashAssetClass` or
to a scope disposition, which is the doctrine `crates/iaam-store/src/reference.rs`
already states: nothing may branch on the class label, and a feature that needs
behaviour is given its own declaration rather than growing that one.
`negative_balance_expectation` is the existing precedent, and this is the second.

**Nothing derives it.** Not a balance that reached zero, not an account that
stopped moving, not a label, and — when deposit contracts arrive — not a
contract's scheduled end date. Each of those is wrong on the first deposit closed
early.

### 2. It carries a revision, and the history is append-only

```rust
pub struct RetirementRevision(pub u32);
```

One monotone sequence per owner over every retirement he has declared, minted by
every accepted call, stated by every report as
`population.retirement_revision`. `0` is "he has declared none" and is a real
coordinate.

The storage is an append-only history: one row per statement, keyed by
`(owner, revision)`, with `effective_on` NULL for a withdrawal, and triggers
refusing `UPDATE` and `DELETE` — the pair `contour_versions` and
`contour_accounts` already carry, for the reason they carry it. The statements
in force at revision *R* are, per account, the row with the greatest revision not
above *R*.

The alternative shapes and why they lose:

- **An unversioned `retired: bool` or a bare date column.** A retirement changes
  what an asset snapshot prints, so an unversioned flag changes what an
  already-published snapshot says — precisely what the contour tables'
  immutability triggers exist to prevent, arriving on a second axis.
- **A full copy of the set per revision, as a contour version does.** It buys
  nothing here. Contour membership is edited as a set and the whole set is the
  meaningful unit; retirements are declared one product at a time, so one row per
  statement reconstructs any revision exactly and costs a row rather than a copy.

### 3. It never reaches `contour::classify`

The retired account **stays a contour member**. The closing transfer stays
internal, the interest stays an earning, whenever the report is run and however
long after the product ceased.

This is the load-bearing bound. A retirement that changed classification would
be the retroactivity above arriving through another door, and it would destroy
the two answers the declaration exists to preserve. It is written on
`iaam_core::retirement`'s module doc, on the store migration, on the operation
key, on the route and on the port — not only here — and
`retiring_the_closed_deposit_empties_the_asset_report_and_moves_no_figure`
compares a flow report against itself, before and after, so that a single moved
quantity fails.

### 4. It never removes an account from a report's population

`PopulationAccount` gains `retirement: Option<Date>`. A retirement is **a second
axis, not a fifth `AccountStanding`**: a closed term deposit is normally
`covered` *and* retired, and any model that had to answer both questions with one
word would get one of them wrong.

An account the calculation still folds is one the population must still name. The
population is the report's own name table — `docs/api/conventions.md` §3.5 sends
a client there for every account named anywhere in the response — so dropping the
retired ones would leave it incomplete for exactly the accounts whose rows are
missing, which is the one reader who needs the entry.

A retirement adds no caveat and does not make a whole population partial. The
account is covered, its money is in the figures, and the retirement says only
that no more will arrive.

### 5. The asset snapshot drops a retired row only where it holds nothing

From the effective date on, `assets::asset_snapshot` leaves out a retired
account's row — and with it the account's class membership, and a class it was
the only member of — **when every one of its cash figures and positions is
zero**.

**Two facts, and neither alone does anything.** The retirement alone would hide
money; a zero row alone is an account the owner still has and still wants listed.

**The safety property is that suppression cannot move a number.** Only an
all-zero row may be dropped, and such a row adds zero to its class total and zero
to the whole. A retirement therefore removes a line and never a figure.

Where a figure is not zero the row stands, the class membership stands, and the
register carries `retired_account_not_empty` for the account, naming both sides
of the disagreement as remedies: withdraw the statement, or rule on the journal.
The two halves are exact complements — a retired account either has no row, or
has one and a caveat — so neither a suppression that swallowed a balance nor a
row nobody could explain is reachable.

Snapshots and reports for dates before the effective date are unchanged, and the
balances answer is untouched at every date: it states what the journal holds per
account, and a reader reconciling it against a statement needs the account there.

### 6. What refuses, and what does not

- **Retiring an account twice** — refused. A second statement with a different
  date would silently move the boundary under every snapshot already taken
  between the two dates. Restating it is two acts, withdraw then declare, and
  each is a revision a reader can see.
- **Withdrawing when nothing stands** — refused. Every accepted call mints a
  revision, and a revision that changed nothing is a coordinate that means
  nothing.
- **An effective date after today** — refused. A product that has not ceased has
  not ceased, and accepting the statement would arm a change to the asset
  snapshot that begins on a day nobody revisits.
- **An account that does not exist, or is somebody else's** — not found. An
  identifier is not an access right.
- **Retiring an account that still holds a position or a balance** — **not**
  refused, and this is the decision a reader will want argued. The fold already
  handles it unconditionally, so a refusal buys nothing. What a refusal could
  read is "what the journal holds today", which is not the question a retirement
  answers: the two disagree exactly where the system's knowledge is short — a
  deposit whose principal predates the imported interval sums to movement from an
  unknown start and is not a balance at all — so the refusal would block the
  owner from stating a true fact because an import has not happened yet.

### 7. Nothing hides a retired account by default

Neither `GET /v1/accounts` nor the outstanding-work queue changes. Hiding by
default would change what a published list returns for a client that never asked
for the change, and the queue's questions about a retired account are still worth
asking: its money is still in every report over a period the product existed in,
so a statement it is short of still changes a reported number.

## Consequences

**What a reader gains.** The three things at the top, together, for the first
time. And a coordinate: two asset snapshots over one contour version are
comparable when their `retirement_revision` matches, and when it does not, the
reader knows what to look at.

**What a reader loses.** Four things, stated plainly.

1. **A retirement alone does not remove the shell.** The journal must also show
   the account holding nothing. For a deposit whose principal predates the
   imported interval that means recording the §10.7 reconstructed opening first;
   until then the row stands with its caveat. This is the honest behaviour — the
   principal really is a hole in the owner's cash total — but it is not the
   one-call fix the feature looks like.
2. **A published report can now differ from one taken a moment earlier over the
   same contour version.** The coordinate makes that visible rather than
   preventing it. Nothing pins a report to a past retirement revision: the read
   side takes no coordinate, and adding one is a further decision.
3. **There is still no inventory of only currently-existing products.** It was
   not built, deliberately: it is a distinct concept, and quietly conflating it
   with the population — which must go on naming every account the fold covered
   — is the failure this decision spends most of its length avoiding. A caller
   that wants one reads any report's population and filters on `retirement`.
4. **One shape change that is not a value change.** A class whose figure was
   `Mixed` only because the dropped row contributed a zero movement now reads as
   a plain balance, and a currency may gain an entry in the snapshot's `total`
   where it had none. Both follow from the same arithmetic: the qualification
   exists so that a flow is never added to a stock, and the flow that has gone
   was zero.

**The rule for the deposit contract that has not been built.** Epic E3.5 will
give a deposit contract a *scheduled* end date; this declaration carries the
*actual* one. Two things saying "this deposit ended" is how they come to
disagree, and the rule that settles it is already in force one domain over: a
bond has a payment schedule and actual payments, and the posting match compares
one against the other, with a verdict for a payment that was due and did not
arrive. **The schedule predicts; the journal records; a plan never overrides a
fact.** A deposit closed early is that shape exactly — the contract's end date is
a prediction the retirement falsifies — so nothing may read a contract to
conclude that a product ceased, and nothing may refuse a retirement because a
contract says the term has not run out.

**Not breaking for a published client.** Every schema change is additive:
`population.retirement_revision` and `population.covered[].retirement` are new
fields on responses, a twelfth caveat kind joins a set clients already switch on
open-endedly, and the two routes are new. No field changed shape, moved or went
away. A client that ignores all of it sees exactly one behavioural difference,
and only after the owner declares a retirement: an asset snapshot stops carrying
a row whose every figure was zero.

**What would falsify this.** If a second consumer wants retirement to change
what a figure *is* rather than whether a line is printed — a return calculation
that stops at the date, a reconciliation that stops expecting statements — then
retirement has become a rule and not a rendering, and §3's bound has to be
re-argued rather than quietly widened.
