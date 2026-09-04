# 0011. The other two reconciliation verdicts, and why they differ

Date: 2026-09-04 · Status: proposed · Bead: `iaam-7b4t`

## Context

Decision 0009 found that `Verdict::Accepted` is published and constructed by
nothing, and settled it: the code stays, and its published meaning says that no
path emits it. Its consequences named two more codes as open work — `discrepancy`
and `needs_reconciliation` — and warned that their case looked different:
"theirs is a gap rather than an impossibility."

That warning was worth writing down and turns out to be wrong. This decision is
0009's sequel, and its first job is to say so.

### The finding that decides it

**Three of the ten verdicts are unproduced, and all three are the reconciliation
ones.** Every construction of `Accepted`, `Discrepancy` and `NeedsReconciliation`
in the tree is a test fixture — two in `crates/iaam-ingest/src/verdict.rs`, two
in `crates/iaam-server/tests/contract.rs`. Every other mention is a match arm
that reads or serialises a value nothing creates. The seven codes production does
emit are the ones about a row: parsed or not, recorded or not, a duplicate or
not, classifiable or not, inside the perimeter or not.

That the unproduced set is exactly the reconciliation set is the evidence. Three
independent omissions would be a backlog. One boundary is a design, and 0009
already named it: a verdict answers a write, and reconciliation answers a read.

### `discrepancy` is 0009's case, and its payload proves it

The variant is typed `{ event, account, dimension, detail }`. Strip the event and
the free-text detail and what remains — `{ account, dimension }` — is the
read-time claim exactly. `iaam_core::returns::MaterialIssue::Discrepancy` carries
those same two fields and nothing else, raised in the data quality block wherever
`ReconciliationLedger::status_for` folds out `DimensionStatus::Discrepant`. The
verdict is its twin with a row identifier bolted on.

What the verdict lacks is the interval. A reconciliation status is a property of
an account, a dimension **and an interval**; without one, "reconciliation does not
match" names no period to not match over. The design's own §10.4 row for this
code lists the interval among what it must carry, and the type does not have it.
Everything that genuinely states a discrepancy in this system does:
`ReconciliationStatus` carries a period, and the `discrepancy_unresolved` queue
item is keyed by account and period.

The bead's case for making the code reachable was the commit override —
`accept_control_mismatch: true` in `crates/iaam-app/src/scenarios/import_session.rs`
writes rows the source's own figures contradict, and the system knows it at the
moment it writes them. That much is true, and it is a real fact. But it is a
different fact from the one this variant is typed for: what the commit knows is
that **a batch disagrees with the control section one document printed**, per
account, currency and control figure. Account and currency, not account and
dimension; a `ControlFigure`, not a `Dimension`; a batch and a document, not an
interval of an account's history.

And the second half of the bead's premise — "something could honestly report it,
and today nothing does" — **is false.** Three things report it, and the commit
path's own doc comment already says so: "the disagreement becomes a permanent,
readable fact that reconciliation will report as `discrepant` for as long as it
stands."

- **The import session's assessment.** `ControlReconciliation` names every
  disagreeing figure with the claimed value, the observed value and the
  difference, and is published in the plan the caller reads before committing.
  It stays readable after the commit, because it is folded from the session's own
  rows rather than from the journal.
- **The refusal.** Without the flag, the commit refuses and lists every
  disagreement with both numbers. The flag is a sentence the caller had to write
  after reading them.
- **The journal, and then the queue.** Committing writes the control figures in
  as the assertions they are, beside the rows they contradict. The ledger folds
  them, the data quality block reports `discrepant`, and the action queue
  publishes `discrepancy_unresolved` — carrying claimed, observed and delta, and
  the two operations that settle it.

None of the three is a property of one row. `CommitOutcome` already separates
`control_assertions` from `verdicts` for precisely this reason, and says why:
"`verdicts` is a verdict **per row** and a caller reads it by position." A
control mismatch belongs to no row. Asked which row of the batch is the
discrepancy, the honest answer is all of them and none.

### `needs_reconciliation` fails harder: emitting it would be false

The variant is typed `{ account, dimension }` and its published meaning was
"Nothing was recorded: there is no owner remainder for the dimension to
reconcile against."

**No write in this system is ever declined for want of an owner remainder.** A
row whose account has no recorded balance is recorded, as `provisional`. The
need for a remainder is then derived *from* the recorded facts:
`actions_from_state` reads the account's activity, computes the period the facts
span, and emits `provide_control_assertion` for it — the opening point first, and
the closing point only once the opening one is answered.

So the ordering settles it. The need is discovered **after** the write this code
would have to be the answer to, from the very facts that write recorded. There is
no moment at which a per-row verdict could say "nothing was recorded because no
remainder exists", because something always was recorded and the remainder is
wanted on account of it.

This is worse than `accepted`'s problem, and the difference matters.
`accepted` states a read-time property in a write-time answer: true of a moment,
stale by the time it is parsed. `needs_reconciliation` states something that is
never true at all. `is_recorded` puts it on the "nothing was recorded" side of
its line, and that placement is right about the sentence and wrong about every
situation the sentence would describe.

What the code was reaching for is real, and the queue carries it better in every
respect. The verdict would announce a need per row, with no interval, no
operation, and no way to be restated once the response was read. The queue item
announces it once per account and interval, carries the operation that answers it
with the account, the dates and the balance point already filled in, is graded
required, is deduplicated by a stable identity, and disappears when the figure
arrives.

### The line this holds

Wave K: an agent that settles rows correctly must not be worse off than one that
guesses. Wave L: an identity a client must name is one the system gave it, and a
state the system reports has an act that resolves it.

Both of these codes name states. Both states have acts —
`discrepancy_unresolved` and `provide_control_assertion` — and both acts are in
the queue, with their operations. Neither verdict has an act, because a verdict
is returned once and there is no call that restates it. Publishing a state in a
channel that cannot carry its resolution is `accepted`'s mistake in the other
direction: the right information in a place a caller cannot act on it.

## Decision

**All three reconciliation verdicts stay in the published vocabulary, and each
one's published meaning says that no path emits it and where the answer is
actually reported.**

- `accepted` → the data quality block, as `accepted_internal` or
  `accepted_independent`. Unchanged from 0009.
- `discrepancy` → the import session's assessment for a batch against its own
  source, figure by figure with both numbers and the difference; the data quality
  block as `discrepant` and the queue's `discrepancy_unresolved` for a
  disagreement the journal holds.
- `needs_reconciliation` → the queue's `provide_control_assertion`, naming the
  account, the interval and which end of it the balance is wanted at.

The contract test that held 0009's sentence now holds all three, and holds each
against a destination as well as against the admission: "nothing emits this" on
its own turns a false promise into a dead end. It continues to check that codes
production *does* emit are not described this way, so the caveat cannot spread
across the vocabulary unnoticed.

### Why not implement either

For `discrepancy`, because the fact a commit override knows is not the fact the
variant is typed for, and building the variant would mean either changing its
shape — which is a breaking change to a published schema — or filling
`{ account, dimension }` from a comparison that is keyed by account, currency and
control figure. That second option is the dangerous one: it would produce a
value that reads exactly like `MaterialIssue::Discrepancy` and means something
else, on a channel that cannot be re-read to correct it. A client that stored it
would hold a per-row claim about an account's reconciliation that the system had
never made.

For `needs_reconciliation`, because there is no write to attach it to. Making it
reachable would mean declining rows for want of a balance the owner has not been
asked for yet — refusing to record what happened because nobody has said what the
total should be. That is the failure `commit_session` already rejects in its own
words: "a system that could not record what happened because a bank's arithmetic
was wrong is a system that cannot record what happened."

### Why not remove them

0009's argument, unchanged and now covering three codes rather than one. §10.4
names all three; deleting values a design document mandates is the owner's
decision, not an agent's; and the gain is zero, because every branch was
unreachable and no client can ever have taken one. What a client is owed is not a
shorter list but a truthful one.

If the owner prefers removal, this decision is the argument for it as much as
against it. The migration is small and mechanical: drop the three variants, the
dead match arms in `verdict_from_recorded`'s neighbours, `VerdictDto::from_domain`
and `event_id_from_verdict`, and the test fixtures. The wire codes go with them,
because the schema is expanded from the same list.

**This decision is not breaking.** No variant is removed, none is added, no
serialised value changes, and no response gains or loses a field. Three schema
descriptions and several doc comments change.

### What a client that branches on either code should do

Nothing to its code; the branches are dead and always have been. One thing to
its reading, and it differs by code.

A client waiting for `discrepancy` on a row must read the import session's
assessment before committing — that is where a batch's disagreement with its own
source is named, figure by figure — and the data quality block afterwards, where
a disagreement the journal holds is reported per account, dimension and interval.

A client waiting for `needs_reconciliation` must read the action queue. It is
not merely the better channel; it is the only one, and it says more: which
account, which interval, which end of it, and the operation that answers.

## Consequences

- `crates/iaam-ingest/src/verdict.rs` states the two reasons separately on the
  type, because collapsing them loses the sharper one. The paragraph that
  recorded both codes as open work is gone.
- `crates/iaam-server/tests/contract.rs` holds all three published sentences and
  each one's destination, and still holds the negative against three codes
  production emits.
- `docs/agent-skill/SKILL.md` tells an agent not to wait for any of the three,
  and where to look instead, beside the existing rule not to call `provisional`
  an error.
- A test comment in `crates/iaam-core/src/reconciliation/check.rs` pointed a
  reader at `needs_reconciliation` as the answer to an incomparable claim. It now
  points at the outcome the function actually returns.
- The verdict vocabulary is now described exactly: seven codes about a row, which
  production emits, and three about reconciliation, which nothing does. A reader
  who notices that shape has understood the boundary rather than found a backlog.
