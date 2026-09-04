# 0018. A figure names the population it was folded over

Date: 2026-09-04 · Status: proposed · Beads: `iaam-5put`

## Context

Every report and the journal read exactly one population: what the journal
holds. Rows sitting in an uncommitted import session are outside every figure
the system publishes, and there was no way to ask what the answer would look
like if they were in it.

That is a real gap, and it has a shape worth stating precisely. An import
session exists so that nothing is written until somebody has looked at it — the
assessment says, per row, what a commit would record. But the assessment answers
*about the import*: totals per account and currency, a control reconciliation, a
readiness verdict. The question the owner actually asks before committing is
about **his money**: does the month come out right, is the balance what the
statement says, does the flow report still add up. Those are report questions,
and the reports could not be asked them.

The naive fix is the dangerous one. A report that folded held rows by default —
or a parameter whose absence quietly widened the population — would publish a
figure that mixes confirmed facts with rows nobody has ruled on, and nothing in
the answer would say which. A number whose provenance cannot be read off the
answer that carries it is worse than no number: it is a number a reader will act
on.

There is a second trap, and the codebase has already paid for it once. The
obvious way to build "what would this session add" is a second fold over the
stored observations. The doc comment on `assess_import_session` records what that
cost: a preview written beside the import drifted from the import, and produced
positive verdicts for rows that were absent from the report the owner was shown.
`ImportSessionContentsDto.row_count` refuses the same thing for the same reason —
two readings of one session that can disagree. On a *report* such a drift would
be invisible: there is no verdict to be wrong, only a figure that is quietly not
the figure the commit produces.

## Decision

### 1. One parameter, named in the request and echoed in the answer

The four report routes take `held`:

- **absent** — the journal alone. This is the default and it does not move;
- **`all`** — the journal, plus every import session of the owner's that is
  still open;
- **a comma-separated list of import session identifiers** — the journal, plus
  those sessions.

Every report answer carries `held_rows` — always, including when nothing was
asked for. It states what was requested (`none`, `all`, `named`), one entry per
session the scope resolved to, and the count of held rows that produced no fact.

`requested` is on the block because the session list alone cannot say it. An
empty `sessions` under `all` means the owner is holding nothing; the same empty
list under `none` means nobody asked. This is `MarketPriceSeriesDto`'s
`complete_through` argument exactly: the field exists so that an empty answer can
be told from an answer to a different question.

**One parameter and not two.** The rejected shape was a word naming the
population beside a separate list of sessions — `population=journal_and_held`
plus `sessions=…`. It reads well and it has a state a caller composes by
accident: the list filled in and the word left at its default. That request's
plain reading is "include these" and its behaviour is "the journal alone", which
is the exact defect this feature exists to remove, arriving through the door the
feature installed. A shape that cannot express the mistake beats a shape that
refuses it.

**`all` is not a name being resolved.** §3.2 of `docs/api/conventions.md` forbids
accepting a *name of a thing the owner named* as input. `all` names no thing; it
is a quantifier over a set, resolved when the report runs, and it cannot collide
with an identifier because a session is a UUID and `all` is not one. Every
identifier a caller sends here was copied out of an earlier response, which is
the property §3.2 exists to protect. The difference between `all` and spelling
the identifiers out is real and is the caller's to choose: `all` is "everything I
am still holding, whatever that is now", the list is "these".

An empty value is refused rather than read as the default. A caller that wrote
the parameter meant something by it, and "you asked for nothing" is not a reading
of `held=`.

### 2. There is no value that answers over held rows *instead of* the journal

The owner's request named two scopes: "everything confirmed, plus these held
sessions", and "everything held". The second is implemented as **the journal
plus every open session**, not as the held rows alone.

Reading it the other way was considered and refused on two grounds:

- **It is not a figure of the kind these routes publish.** A balance folded from
  one import alone has no opening and no history: it is a movement from an
  unknown start, which the balances answer already has a word for and would have
  to print for every account. What such a caller wants is a delta, and calling it
  a balance is the mislabelling `CashOpening` exists to prevent.
- **It is already answered, by the planner, in the right vocabulary.** The
  session's own assessment publishes `commit_delta.fact_totals` — what these rows
  come to, per account and currency, folded in the core. A second answer to that
  question, computed at a different altitude and printed in a report's shape, is
  a second place the two can disagree in front of the owner.

### 3. Which surfaces take it: the four reports, and not the journal read

`GET /v1/reports/flow`, `/balances`, `/assets` and `/returns` take `held` and
carry `held_rows`. `GET /v1/journal/events` takes neither, and that is a
decision rather than an omission.

Argued from what a caller can be wrong about:

- **A report publishes a figure; the journal page does not.** The journal
  scenario's own module doc says it "computes no total and derives no figure, so
  an agent forbidden its own arithmetic has something to quote". A figure is a
  thing whose population a reader cannot see and must be told. A row is a thing a
  reader is looking straight at.
- **A held row has no identity to print.** `JournalEventReadDto` leads with
  `event`, and a planned event's identifier is minted by `plan_session` and
  differs on the next call. Publishing one would hand a caller an identifier that
  addresses nothing, in a response whose entire purpose is "is this the row I
  submitted". `PlannedSession`'s candidates are private for exactly this reason,
  and the reporting path folds them without publishing one.
- **The page is a cursor, and a held row cannot sit in one.** The page resumes
  from a position in `(effective_date, sequence)`; a planned event has no
  sequence, because the store assigns it in the transaction that inserts it. Any
  interleaving invented here would be a second ordering rule beside
  `compare_for_replay`.
- **The question is already answerable, better, one route away.** A caller that
  wants the held rows of a session reads
  `GET /v1/import-sessions/{session}/assessment`: every row, what each would
  become, why each one would not. Both directions already exist — a committed
  fact carries `import_session` back to the act — so nothing is missing.

What a caller *could* be wrong about is the one thing the journal page already
says plainly: reading it filtered by a held session returns no rows, and that is
true. The session has committed nothing.

### 4. A session that has already committed

Naming a committed session is **accepted**, folds nothing, and says so:
`contribution: "already_in_journal"` beside `state: "committed"`.

Folding it would double its rows: they are in the journal, and the ordinary read
already holds them. Relying on the planner's duplicate detection to notice that
is not enough — the check is by idempotency key, and a row that carried none
would be folded a second time — so the rule is stated by session state and not
inferred per row.

Refusing was considered and rejected. Committing a session between two reads
would then turn a request that worked into a request that fails, while the figure
it produces is identical either way; a caller polling a report through a commit
should see the number stay put, not see a 422 appear. And the codebase's own
precedent points this way: `population.outside` names the accounts a report left
out rather than refusing to answer, because naming an omission is what lets the
reader see it.

An **abandoned** session is treated the same way and named differently:
`contribution: "abandoned"`. The distinction is load-bearing and is why this is a
word rather than an absence — "its rows are already counted" and "its rows will
never be counted" are opposite facts, and an absent field spells them the same.
The second means the answer is the journal's alone, which the caller who named
the session needs to be told.

### 5. Nothing folded over held rows is persisted

The returns path saves a projection snapshot so that the next report can advance
from it. A snapshot folded over held rows would persist money nobody has
committed, and every later report would advance from it without ever having been
asked to. So when anything was folded, the snapshot is neither saved nor loaded.

Not loading is the half a reader will question, since `advance` checks a prefix
fingerprint and would fall back to a full recomputation on its own. It is refused
anyway, because the fallback is incidental: a held row is interleaved into the
prefix **by date** — an import of last month's statement lands before the
snapshot boundary, not after it — and that is precisely the case `advance`'s own
doc names as producing "plausible but incorrect balances". A report that is
correct only because a check happened to fire is a report waiting to be wrong.

### 6. The facts come from the planner that commits

The events folded are `plan_session`'s, taken through
`PlannedSession::would_append`, which is filled on the line that splits planned
facts from duplicates. There is no second pass over the stored observations, and
there must never be one: that is `assess_import_session`'s recorded lesson read
from the reporting side.

They are appended to the same event slice the journal read produced, **before**
`resolve`, so that everything downstream — the effective set, the balances, the
price board, the perimeter assessment, the reconciliation ledger, the money-flow
fold — reads one population. Folding some of them over one set and some over
another is the state this feature exists to make impossible. Ordering is not a
concern the caller has to manage: `resolve` sorts for replay itself.

Duplicates are not folded. A row whose idempotency key the journal already holds
adds nothing at commit, and folding it beside the journal would count that money
twice. The count is published, because a caller comparing the figure against the
statement the rows came from needs to know how much of it was already there.

### 7. The answer counts what it could not include

`held_rows.retained_unrecorded`, and the same count per session.

A row with an unanswered question or a reading that failed becomes no fact at
all. So any figure "including held rows" is systematically short, and short
*precisely where attention is owed* — the rows nobody has ruled on are the ones
whose amounts are least predictable. An answer that did not publish this count
would manufacture confident wrong arithmetic. It is a field and not a caveat in
prose, and it is not optional.

Three counts stand beside it, and each is a different fact:

- `settled_without_fact` — rows read, understood, and correctly producing
  nothing. The figures are **not** short by these. Folded into
  `retained_unrecorded` the pair would send a reader looking for money that was
  never there.
- `already_in_journal` — §6.
- `beyond_the_report_date` — planned facts the report's own date bound left out.
  A held row dated after the report date is outside the answer exactly as a
  journal row dated after it is; it is counted because the caller asked for this
  session and this is the part of it he did not get.

### 8. What refuses

- **A session named twice** — refused, `422`, naming the identifier.
  Deduplicating silently would accept a request whose plain reading is "count
  this import twice" and answer something else.
- **A value that is neither `all` nor a list of identifiers**, and the empty
  value — refused, `422`, and the refusal publishes the vocabulary.
- **A session the owner does not hold** — not found. An identifier is not an
  access right, and this is a request built on something that was never handed
  out.
- **Reading a report over held rows without the owner's token** — not refused.
  This is a read, it writes nothing, and `plan_session` writes nothing; the
  read-only scope is the floor for every report and this changes none of it.

## Consequences

**What a reader gains.** The question "what does my month look like if I take
this import" has an answer, in the same shapes and the same routes as every other
report, and the answer says what it was computed from. Because the events come
from the planner that commits, the report read before committing is the report
that comes back after — which is asserted directly, by comparing one against the
other across a commit.

**What a reader loses, and what it costs.**

1. **A report over `all` plans every open session.** `plan_session` reads the
   whole journal to build its duplicate keys, so the cost is one journal read per
   open session, on a read path that previously did one. Nothing caches it. An
   owner holding a dozen open sessions and asking for `all` will feel it, and the
   remedy is his: commit or abandon them.
2. **A figure over held rows is not reproducible by identifier alone.** Answering
   the same request a minute later can differ, because a session can gain rows or
   have a question answered. `revision` is what makes that visible: it is the
   plan's own stamp, the same one the commit call checks, so two answers over one
   session are comparable when their revisions match. It is the third coordinate
   of an answer, beside `contour_version` and `retirement_revision`.
3. **`held_rows` prints bare session identifiers.** It carries no title, because
   a session has none — the owner names an import by its label, and a session may
   declare no source at all. This is not §3.6's gap arriving on a new type: there
   is no name to print, and the identifier is the whole of the session's
   identity, exactly as `ActionSubjectDto::Event`'s is.
4. **The journal read still cannot show a held row.** §3. A caller that wants
   them goes to the session's assessment, which is one call and shows more.

**Not breaking for a published client.** `held` is optional and absent means what
the routes did before. `held_rows` is a new field on four responses; a client
that ignores it reads exactly the figures it read yesterday. No field changed
shape, moved or went away, and no default changed.

**What would falsify this.** If a second consumer wants a *stored* answer over
held rows — a saved snapshot, a cached report, anything a later request advances
from — then §5's bound is the thing in the way, and it has to be re-argued rather
than quietly widened: what would be needed is a coordinate that names the plan a
persisted state was folded over, and a rule for what happens when that plan
changes. And if the journal read ever publishes a row that is not in the journal,
§3 is wrong and the identifier problem it names has to be solved first, not
worked around.
