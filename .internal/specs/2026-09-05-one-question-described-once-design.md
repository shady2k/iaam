# One question described once: what the queue and the assessment may say, what the profile may read, and what the owner never hears

Bead: `iaam-1wg6` · Prior: `iaam-hnod`, `iaam-3qsq`, `iaam-fmih`, `iaam-m2oi` ·
Decisions: 0026, 0028, 0031, 0032, 0034, 0035 · Date: 2026-09-05

## Where this came from

A live session against a real instance, run by an external agent under the
owner's token, and the owner's own reading of what he was shown. Six defects, of
which one contradicts the contract, two can put a wrong fact in the journal, two
ask him for what the source already told us, and one is the register the last
wave was supposed to fix.

**Nothing of the session's data is in this document.** The findings are shapes;
the illustrations are invented; the source's column names are the source's
schema and stay as the source prints them, per the language rule.

The common cause of the first three: **a surface asserts what the instance
computes.** Where the answer is already derivable, a sentence written by hand is
a second answer that can disagree with the first, and here two of them do.

## 1. Whether the answer becomes a rule is computed, and both surfaces say what it computed

**The defect.** Two published sentences describe the same act with opposite
persistence, and neither is conditional:

- the queue's item for a classification question states, flatly, that the answer
  is written as a rule and a row matching it settles by itself next time
  (`crates/iaam-app/src/actions.rs:3544`);
- the assessment's one-decision group proposal states, flatly, that no standing
  decision is kept (`crates/iaam-app/src/scenarios/import_session.rs:6848`).

**Neither is stale. Both are unconditional about something conditional.**
`Generalisation` (`import_session.rs:1087`) already distinguishes three outcomes:
a rule was written; a rule was possible and none was written **because the
answerer may not generalise** — which is what `iaam-hnod` settled for an agent's
token; and no rule can be built from this row at all, because a matcher that asks
nothing matches nothing. The queue's sentence is false in the last two cases, the
group's sentence is false in the first, and neither tells the reader which case
he is in.

**The decision.** One value, computed in one place, answering *what this answer
will decide beyond this session*, with the three outcomes above plus «not yet
answered». Both sentences are derived from it, and neither states persistence on
its own again. The queue can carry it because the caller's authority is already
known where the item is built — every resolution already publishes the authority
it demands.

**The group's sentence keeps its own half.** How many of this session's rows one
answer settles is the group's own fact and stays. What it stops doing is
answering the persistence question in the same breath, which is where it went
wrong: the two are independent, and the text welds them.

**Observed cost.** The agent stopped mid-flow to reconcile the two texts, then
explained tokens and addresses to the owner in order to justify one over the
other — which is also §6's defect, produced by this one.

## 2. Two legs of one movement are one item in the queue

**The defect.** The assessment pairs the two rows a single document printed for
one movement between the owner's own accounts and raises **one** question about
the pair (`GroupBasis::OneMovement`, `AnswerReach::ThisRow`, the far row settled
as `SecondLegOfOneMovement`). The queue publishes the same two rows as two
independent items, not adjacent, with nothing saying they are one movement —
`crates/iaam-app/src/actions.rs` never consults the pairing at all.

An agent working the queue — which the skill and the discovery document both send
it to first — answers both legs separately. The journal then records one movement
twice, or one leg is answered and the other left as an orphan.

**The decision.** A paired leg does not get its own item. The pair is one item,
because one answer already settles both: two items grade one decision as two
pieces of work, and they leave open the very act — answering the legs
separately — that this exists to prevent. This is the shape decisions 0032 and
0034 already give a set answered once.

**A pair is a hypothesis and stays refusable.** `mirror.rs` is explicit that two
unrelated payments of one amount on one day exist and that «no, these are two
different things» must remain sayable. The single item publishes that answer
among its alternatives; an item that could not be refused would be worse than two
items.

## 3. A leg with no counterpart says so, and says why when it knows

**The defect.** `mirrored()` returns the pairs it found. A row that has the shape
of a leg and no counterpart is simply absent from that result, and absence is
published nowhere. At the per-row surface it is indistinguishable from any other
row, so the alternatives offered are the ordinary ones — and answering «money I
sent to my own account» with a guessed far side records a movement whose other
half does not exist, while answering «I paid somebody» files an internal move as
spending.

**The decision.** Such a row publishes that this document holds no counterpart
for it, and the question put to the owner is that question — the other half is
not in this statement, so where did it go — rather than the ordinary five.

**Two things the wording must get right.**

- **Absent from this document is not absent in the world.** The far half may be
  in another statement, or on an account the owner did not put in his group. The
  published sentence says «not in this document» and never «does not exist».
- **Where the instance knows why, it says why.** A document covering one account
  cannot contain the far half of any movement between two of them —
  `mirror.rs` pairs within one document by construction, and the
  cross-institution matcher is deliberately a different mechanism with a
  different window. The instance already knows which accounts a document asked
  for, so «this document covers one account, so the other half was never in it»
  is available and is a different conversation from a bare «no counterpart»: the
  first tells him what to do next.

The answer this steers to already exists — an own-account movement with the far
side unnamed, delivered under `iaam-fmih` and decisions 0012 and 0013 — so the
question leads somewhere rather than into a wall.

## 4. The profile reads the column that holds the owner's own decision, and the code that generalises

**The defect.** `crates/iaam-ingest/profiles/tbank-operations-csv.json` reads
seven of the export's sixteen columns. Two of the nine it ignores carry the
answer the instance goes on to ask the owner for:

- **`Ваша категория`** — not the source's guess but the owner's own decision,
  already made and recorded at the institution. Asking him again for what he
  already told his bank is the worst question this system can ask.
- **`MCC`** — a standardised code. As the ground for a rule it covers a whole
  kind of spending where the printed description covers one merchant string. It
  is empty on some rows, so it cannot be the only ground.

Neither string occurs anywhere in the repository.

**The decision.** Both are transcribed, and a rule may ask what the source filed
a row under — which is decision 0026's shape, already built for
`source_category`.

**Transcribed, never interpreted.** Decision 0028 governs: the profile writes
down what the source claims and the engine decides. A category the owner set at
his bank is his decision *in the bank's vocabulary*, not in his categories here,
so it grounds a question asked **once per distinct value** — «what you and your
bank call this, what is it called here?» — and never a conclusion drawn per row.
That is the reduction that matters: a handful of questions where there are now as
many as there are rows.

**`Учёт в аналитике` is deliberately not used.** It was proposed and the owner
refused it: it says what the institution leaves out of its own analytics, which
is not a statement about what the money was.

## 5. The status column is transcribed, and a row may be refused on it

**The defect.** `Статус` is not read and the profile has no row filter of any
kind. A row the institution marked as anything other than completed is imported
exactly like a completed one, and becomes a fact in the journal.

**The decision.** The status is transcribed like any other cell, and a row may be
refused on what it says. **No vocabulary of statuses is hardcoded**: this
repository does not know what an institution prints there, and a list invented
here would be wrong for the next institution and stale for this one. The profile
says which values it accepts, the engine refuses the rest as what the source
stated, and an unrecognised value is disclosed rather than silently accepted —
the shape `docs/decisions/0017` already gives a row with no key.

## 6. What he never hears is a test, not a list

**The defect.** The last wave wrote `docs/agent-skill/SKILL.md`'s «What he never
hears», and an assistant that had read it still told the owner about addresses,
about how a call is made, about the names of fields in an answer, and about what
its credential is allowed to do.

Half of that breaks a rule that is already there — «Nor how you found out». The
other half is not in the list, and **a list reads as exhaustive**: everything it
does not name looks permitted.

**The decision.** The section leads with a test and keeps the list as examples of
it:

> What is his is his money and his decisions. Everything you went through to
> reach them is yours. If you could not say it to a person keeping his books by
> hand, it is machinery.

The examples gain what was missing: an address, the way a call is made, the name
of a field or of any part of an answer, and what a credential may or may not do.
The last needs care — its *effect* on him is legitimate and often required («this
will be remembered» against «this needs your confirmation to stand»), and it is
the mechanism that is not. §1 is what makes that sentence sayable without
explaining tokens.

**A prohibition is published with its replacement.** The owner named it himself:
saying «I'll ask iaam» is fine. Written beside the rule rather than left to be
inferred, because a prohibition with nothing to say instead is broken by whoever
still has to say something.

**The guard's shapes bind this section too**: no route path, no method, no status
code, no payload field name may appear in the text, so each is named by what it
is rather than by an example of it.

## Not in scope

- `iaam-rys2` — a queue item for an account already carrying indeterminate
  movements. It waits on an act that completes such a movement; this spec is
  about the question asked before any fact is written.
- Cross-institution pairing. §3's «this document» is the boundary on purpose.
- Whether answering should be owner-only at all. `iaam-hnod` settled that, and
  §1 publishes what it settled rather than reopening it.

## Success criteria

1. No published sentence states whether an answer becomes a rule; every one
   derives it, and a contract test proves the queue's and the assessment's agree
   for the same question under the same authority.
2. The queue publishes one item for a paired movement, and that item can be
   answered «these are two different things».
3. A leg the document holds no counterpart for says so, says «in this document»,
   and where the document covered one account, says that.
4. `Ваша категория` and `MCC` are transcribed, and the category question is
   asked once per distinct value rather than once per row.
5. A row whose status the profile does not accept is refused as what the source
   stated, with no status vocabulary written into this repository.
6. `SKILL.md`'s section leads with the test, names the four missing examples, and
   publishes what to say instead. `make check` passes, the four guard refusals
   included.
