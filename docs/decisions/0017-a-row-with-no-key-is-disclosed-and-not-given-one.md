# 0017. A row with no key is disclosed, and not given one

Date: 2026-09-04 · Status: proposed · Beads: `iaam-1k9t`, `iaam-56vq`

## Context

The same statement must not be able to enter the journal twice without anyone
noticing. Two holes in the import-session path let it.

### A row that carries no key was never a duplicate

`plan_session` decided what would commit as a `duplicate` by looking at one
thing: whether the journal already held the row's `idempotency_key`. A row that
named none was a fresh fact every time it was fed. Feed one statement into two
sessions and commit both, and both write, both come back with positive verdicts,
and the journal holds every movement twice.

The CSV path does not have this hole. `csv_source::parse` stamps a row that named
no key with a key derived from the document digest and the row's own locator, and
its doc comment says why: rows in a file are ordered and located, that locator is
an identity, and a caller holding only the parsed operations can no longer see
either. The protection was a property of **one entry point** rather than of the
import.

There was a second, quieter disagreement inside the same test. The journal's own
duplicate test — `find_duplicate` — asks for the source's operation identifier
**first**, scoped by the source, and only then for the idempotency key, scoped by
the owner. The plan asked the second alone. So a row carrying a
`source_operation_id` and no key was published as a fact the commit would append,
and then answered `duplicate` by that very commit. An assessment that exists to
say what the commit will do was saying something else.

### An undeclared session collides with nothing

`open_session` refuses a second open for a declaration whose session already
holds rows: «a statement half imported, and only the caller knows whether the
file in its hand is that statement or another one». `standing_session` finds that
session by the declaration, and a call that declares **neither** a source nor an
import matches nothing. Two undeclared sessions can therefore hold one statement
at the same time, and each can commit it.

## Decision

### 1. The plan asks the journal's own two key levels, in the journal's order

`RecordedIdentities` is built once per plan out of the journal load the plan
already makes, and asks what `find_duplicate` asks: the source operation
identifier scoped by the source, then the idempotency key scoped by the owner.
`CommitDelta::duplicates` is computed from both.

The scoping is what makes the answer travel between sessions. A source is
`SourceId::declared` on the owner, the account and the channel; `session_origin`
derives an undeclared session's from those three and nothing else. Two sessions
holding one statement for one account therefore derive **one** source, and a row
identified by its source's own identifier is recognised across both.

### 2. No key is derived for a row that names none

Three derivations were weighed and all three refused.

- **A key over the row's contents.** Two genuine payments of one amount on one
  day to one place are an ordinary thing, and §10.6 forbids merging them. Such a
  key merges them, silently, in the direction that loses a movement that really
  happened — a wrong answer nobody can see, which is worse than the duplicate it
  prevents.
- **A key over the session and the row's position in it.** The session identifier
  differs in every session, so two sessions holding one statement derive two
  different keys and the check answers «fresh» to exactly the case that motivated
  it. It protects against re-feeding one session, which the store's own `row_key`
  already does, and against nothing else. This is stated explicitly because it is
  the derivation that looks sufficient and is not.
- **A key over the document and the locator**, as the CSV path stamps. Sound, and
  already had: a row that states a locator carries it as `source_operation_id`,
  which is level one and is now compared. What is left over is the row that
  states no locator either, and for that row the document is not a digest of
  anything — it is a name a caller typed, and two months of statements saved
  under one name collide at every row.

### 3. What is left is said out loud

`CommitDelta::resembles_recorded` lists every planned **fact** whose canonical
fingerprint the journal already holds. The fingerprint is not new: `normalize`
stamps `Provenance::raw_hash` with `dedup::fingerprint`, the canonical form of
the account, the kind and the dates, deliberately excluding both submission
identifiers. This is §10.6's level five — `DedupLevel::Probabilistic`, «looks
like a duplicate, but there is no evidence» — which has been specified and
implemented since §10.6 was written and had reached no caller.

Three properties hold it in place.

- **It is never folded into `duplicates`.** That list is the hard finding: the
  journal holds the row's key, the commit appends nothing. This is the soft one,
  and the commit **does** append it — so it is also counted in `facts` and in
  `fact_totals`, because that is what will happen. Folding a guess into a finding
  makes the finding a guess.
- **It is computed inside `plan_session`**, in the same pass as everything else,
  for the reason the whole planner is one function: a second walk over the rows
  describes a different import from the one that runs.
- **It compares a candidate against the journal, never against its neighbours.**
  A document that prints two identical rows is itself the evidence that two
  things happened.

`Readiness::RequiresOwnerDecision` gains `rows_resembling_recorded`. It does not
refuse the commit, and that is the same choice made for unconfirmed transfer
candidates: a refusal would fire on two genuine identical payments, refuse honest
imports, and teach its reader to wave the flag through. What the word does is
make «I committed without looking» something the owner cannot say afterwards.

### 4. An undeclared open is not refused

`standing_session` keeps `(None, None) => None`.

**There is no import for such a session to be half-way through.** The refusal
above is about one *declared* import: the store hands the same session back for
the same declaration, so a second open mixes a second file's rows into a
statement somebody is part-way through answering questions about. The store opens
a fresh session for every undeclared open, and documents that it must — «there is
nothing to recognise it by». Nothing is mixed. What can go wrong is a different
thing: two sessions, two commits, one statement in the journal twice.

**No honest refusal is available at that moment.** An undeclared session does
have a stable identity, but not when `open_session` runs: it is keyed on an
account, the rows have not arrived, and the declaration is what would have named
one. The only refusal that could be written — «you have another undeclared
session open, holding rows» — names nothing the two have in common. It would fire
between an export of one institution and an export of another, and a free session
is opened without a declaration *precisely* so that an institution-wide export is
one session and not four. A refusal that is wrong every time the owner does the
thing the mechanism exists for is worse than the hole it closes.

**The condition is caught where it becomes true.** Two open sessions holding one
statement is not yet a defect; committing both is. Once the first has committed,
the second's rows are measured against a journal that holds them — as
`duplicates` where they carry a key, across the two sessions because the derived
source is the same, and as `resembles_recorded` where they carry none. That is
later than a refusal, and it is the first moment at which anything true can be
said.

## Consequences

- An import of a statement that overlaps one already in the journal is now noisy
  where it used to be silent, and correctly so. A client that sends neither an
  idempotency key nor a source operation identifier will meet
  `requires_owner_decision` on every overlapping row. The remedy is to send an
  identity, which is what the `idempotency_key` field's own documentation has
  always asked for.
- Re-importing a corrected CSV names every unchanged row as resembling. That is
  true: the derived key is over the document digest, so a corrected file is a new
  document and every row of it is new. The plan says so before the commit rather
  than after.
- **Nothing refuses.** A caller that commits without reading the assessment can
  still put one movement in the journal twice. A flag on the commit call, in the
  shape of `accept_control_mismatch`, was weighed and not taken: the finding is
  level five, it is true of ordinary repeated payments, and a refusal that is
  wrong on ordinary data is one the owner learns to bypass. If the assessment
  turns out to be read by nobody, that is the move to reconsider, and it is a
  strictly later one.
- `Verdict::PossibleDuplicate` — «the fact was recorded and resembles one already
  in the journal» — is still emitted by no path. Its published meaning is now
  exactly what `resembles_recorded` reports, one layer down; making the commit
  itself answer in that word would change every write path in the system and is
  filed separately.
