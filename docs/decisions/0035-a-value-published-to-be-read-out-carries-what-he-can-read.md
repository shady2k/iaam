# 0035. A value published to be read out to him carries what he can read

Date: 2026-09-05 · Status: proposed · Beads: `iaam-6jsj`, `iaam-f6y4`

## Context

An agent worked a real import through the session channel and relayed it to the
owner. What he was read out was account identifiers, the wire words an answer is
sent as, the words this system names a row's state by, and — in one place — this
project's own decision numbers. His reply was the one he has now made three
times: the agent is again pouring terms, internal names and identifiers over him.

Decision 0027 governs the fields he **fills in**, and it holds. Everything
published in order to be **read out** to him was governed by nothing.

### The clearest instance is not the caller's fault

`PrintedRowDto.account` is a bare `Uuid`. The one list of accounts published
beside it, `interpretation.answer_accounts`, deliberately excludes the account
the row is on — its own documentation calls it «the one entry that is wrong for
this question», and that exclusion is the whole reason the list is published once
for the assessment instead of once per question (decision 0032).

So the title of the account a question is about could not be obtained from the
assessment at all. A caller either printed the identifier or made a second call
per question. It printed the identifier, and it was right to: those were the only
strings the surface gave it. This is 0027's context repeating itself one surface
along.

The same shape elsewhere on the same path. A pair of rows that are one movement
was named by a uuid this system minted for its own bookkeeping and by nothing
else, so a caller could not say *which* two rows without scanning the list for a
match — while `alike`, the larger relation, has published its rows outright since
it existed. And a bead identifier reached an API error message.

### The second complaint is the same defect wearing a number

A caller wanting to show the owner the raw lines behind a set of questions
matched each question's `row` against the lines of his file. The first few
agreed, then they diverged by no constant offset. It concluded the engine
reorders the document, then that the file had newlines inside quoted fields, and
lost a long stretch of work before stopping. Both hypotheses were wrong, and
nothing published said so.

What is true, and none of it was published where a reader meets these numbers:

- **`row` is the session's counter.** It is assigned as one more than the highest
  the session has issued and is idempotent by row key, so it is the row's
  position among what the session **took**, in submission order, stable across a
  re-reading. It is not a line number.
- **`locator` is the file line.** It is counted in newline bytes up to the byte
  the record begins at, so a newline inside a quoted field does not shift it.
- **Nothing is reordered.** The reading's response is sorted by locator, on the
  stated ground that a caller comparing it with the file it sent should not have
  to sort; the session lists rows in submission order. The two drift because **a
  refused record occupies a file line and takes no session row**, so from the
  first refusal the offset is the number of refusals so far — which reads as a
  different order rather than as a shift.
- **The bridge is already published, where nothing points at it.**
  `SourceDocumentRowDto` carries `locator` and `row` on one object for every row
  the session took.

### One rule or two

They are one rule, and it is 0027's own, widened by one step.

0027 recorded the owner's register — «каждый вопрос пользователю должен задаваться
так, чтобы он понял, без наших внутренних терминов» — and applied it to a field
he fills in. `docs/api/conventions.md` §3 had already recorded half of the other
side: wherever the API prints the identifier of a thing he named,
it prints his name for it beside it. Neither statement covers a closed
vocabulary's word, and neither covers a number that counts something other than
what a reader assumes it counts.

The single rule is:

> **Nothing this API publishes in order to be read out to him is stated only in
> this system's own vocabulary.**

Its two faces are §3.3's asymmetry and not two rules. What he must **supply**
carries the question to put to him (0027), because a pointer is mute. What is
**published** carries the words he reads, because an identifier is opaque and a
code word is ours. Made symmetric in either direction the pair loses one of them:
a name accepted as input reaches the one place that must have no ambiguity, and a
name withheld from output asks him to rule on something he cannot read.

The difference from 0027 that matters in practice: 0027's obligation can only be
satisfied by **writing** a sentence, which is why it is satisfiable three ways
and why one of the three is a register of questions still under review. This
one is almost always satisfied by **publishing a value the system already
holds** beside the one it was publishing. There is nothing to compose and
nothing to review, which is why it is a shape rule and lands here rather than in
a vocabulary.

## Decision

### 1. An account published for a person to read carries his words for it

`PrintedRow` and `PrintedRowDto` carry `title` and `institution` beside
`account`, in the shape `docs/api/conventions.md` §3.5 already tabulates —
`AccountScopeDto`'s exactly, and `AccountCandidateDto`'s three fields under two
of their names. No second shape is invented, and `docs/api/conventions.md` §3.5
gains the row.

**The identifier stays and is not replaced.** It is what the answering call
takes, and §3.2 refuses a name as input; the title is beside it, never instead of
it.

**`title` is optional here where `AccountCandidateDto`'s is required, and the
optionality is the decision.** A row may name an account by identifier that the
owner's directory has never held — that is exactly what `account_resolution`'s
`missing` list is for — and a question about such a row is still a question. §3.5
gave that same case as the reason `BatchTotalDto` prints a bare identifier, and
that reasoning is now visibly too strong: it justifies not making the name
**required**, and it never justified withholding the name where there is one. An
optional title publishes the name where one exists and the absence where none
does, which is strictly more than a bare identifier and is true in both cases.

**It is not the identifier rendered as a name.** `AccountNames::title` falls back
to the identifier so that a refusal an operator reads is never empty; a published
title that can come out as a uuid is the defect itself, so the directory grew a
second reader that answers `None`.

**Built where the plan is built.** The pair is read out of the directory the
plan already loaded — the one reading every row of the session was resolved
against — and copied by the transport, never joined by it. That is §3.4, and its
reason is that a second reading could name one account two ways in one response.

**`answer_accounts` is not the join table, and this is why the two beads are one
change.** That list is published only where some open question admits an answer
that names an account, so a session whose questions are all «was this a fee?»
publishes none of it; and it is the owner's whole directory rather than this
session's accounts, so it holds nothing at all for a row on an account the
directory does not hold.

### 2. A pair names the other row, not only the shared identifier

`OpenQuestion::pair` and `OpenQuestionDto.pair` carry the partner's row beside
the identifier.

The identifier states that two questions are one decision and does not state
which two. A caller that can say «rows 4 and 9 are the two sides of one movement»
can put the decision once, which is the whole purpose of publishing the pair; one
holding a uuid had to scan the list first. `alike` — the larger, looser relation
— has published its rows outright since it existed, so the relation that could
name its partner exactly was the one that made the caller search for it.

An object and not a second flat field, under `docs/api/conventions.md` §5: the
identifier and the row are one statement about one relation, and two optional
fields would let a response carry half of it.

### 3. A closed vocabulary's word carries its sentence, and our numbers stay ours

`AnswerShape::consequence` is the shape this obligation takes and it is not new
(decision 0012): one static sentence per word, published from one function, so
that two publishers of one word cannot come to disagree about what it means.

**Checked rather than assumed, on the vocabularies this decision's surfaces
publish.** A row's state — `held`, `needs_classification`, `settled`, `rejected`,
`unreadable` — turns out to keep the rule already, and the check is worth writing
down because it is what makes the finding below a finding rather than a
preference. Every state that asks the owner for anything already carries its
words: `needs_classification` carries the question and each answer's consequence,
`settled` carries the reason **and** `explanation` beside the code, and the two
refusal states carry what was expected and what arrived. `held` asks nothing of
him — it is the state of a row that will simply be written at commit. So no
sentence is added per row, which would in any case repeat five sentences across
hundreds of rows to say something identical every time — `answer_accounts`'
argument, in the response `answer_accounts` lives in.

**`ActionState` is the vocabulary where the rule is not kept**, and it is not
changed here: `informational` and `needs_owner_input` are published as words with
nothing beside them. It belongs to the bead that owns that queue, and it is named
here rather than left to be discovered.

**A decision number and a bead identifier are ours**, and the chain by which
they reach him has two links, not one.

The last link is a string a client renders. One instance survives: the broker
synchronisation refusal names a repair bead in the message it returns. Named here
in the spirit of §1.4a; it is a one-line change in another crate's scenario and
is not made under this bead.

**The link before it is a doc comment, and that is the one nobody was watching.**
The owner asked why the agent skill quotes decision numbers at him. It does not —
that document contains none. What it does is read the contract, and a doc comment
on a published type is rendered into the contract as the field's description.
`crates/iaam-server/src/dto.rs` carried seventeen such citations; a caller found
them in the schema and repeated them. A published description pointing at a
document its reader has no copy of is a dangling reference in a shipped
artefact — it tells whoever implements a client nothing, and it reads as an
invitation to quote it.

So a **published** description separates what a caller must know from why we
decided it. The first stays, reworded to stand on its own; the second is here,
and the contract does not cite this file. Nothing else changes: no field, no
shape, no behaviour, no schema key.

**A doc comment that is not published keeps its citation**, because the rule is
about the shipped document and not about the source. Three of the seventeen are
of that kind — a `from_domain` conversion, a section banner written with `//`,
and a `//!` header inside a test module — and they were left alone. Fourteen were
rewritten, together with the four this decision itself added.

Six more reach the shipped document from `#[utoipa::path]` doc comments in
`crates/iaam-server/src/routes.rs`, where they become operation descriptions.
They are the same defect and are named here rather than left to be discovered.

### 4. `row` is the session's counter, and the question does not carry the line

**The wording is fixed where a reader meets the number** — on
`SourceDocumentRowDto.row`, on `ImportRowDto.row`, on `OpenQuestionDto.row`, on
`OpenQuestion::row`, and on the store port that assigns it. «The row's position in
the session» was true and insufficient: it must say that the number is **not** the
document's line and name `locator` as the one that is.

**The question does not carry the locator**, and the case against is not that the
owner would not use it. He would: a day and a sum point at a line on a month's
statement (0012), and 0032 added the direction, the party and the word the source
filed the row under for the same reason — but a statement can print two records
agreeing on all of them, a real one does, and then the line number is the only
thing that tells them apart on his own page. That argument is granted in full.

It is refused because **a session row has no locator**, and a field that must be
absent for reasons a reader cannot see is worse than a field that is not there.

- A locator is a fact about **a reading of a document**, not about a row. A
  session also takes rows fed as a batch, which have no file and therefore no
  line; and a session may hold rows out of several documents, so a line number
  without the document beside it names nothing.
- Nothing stores one. An observation stores the document's digest and either the
  identifier the **source** printed for the row or a key derived from the digest
  and the locator. Recovering the line would mean parsing our own key back out —
  the act the reading engine already refuses when it keeps a printed account name
  as a field rather than re-deriving it from a refusal's prose — and it would work
  only for rows whose source printed no identifier of its own. The field would
  then be present for some records of one document and absent for others, for a
  reason no reader of the response could see.
- The absence would mean four different things at once: no document, a
  source-identified row, a row fed as a batch, and a session read before this
  change. `PrintedRow`'s «one absence and not six» refuses exactly that, and so
  does 0027's register.

**Where the mapping is instead.** `SourceDocumentRowDto` names every row the
session took by both numbers on one object, so a caller that keeps the reading's
response can map either way. A caller that did not keep it does not have to hold
the file either: reading a kept document into the same session again is
idempotent by row key — it returns the rows already taken, with their locators,
and writes nothing — and the call takes the document's digest **instead of** its
bytes. That is decision 0019's «retract and re-read» remedy without the
retraction, and it is why the document is kept in the first place. It works while
the session is open, which is precisely while its questions are open.

**What would falsify this.** A session holding rows from several documents whose
owner must find a line, often enough that re-reading a document to recover a map
is the wrong cost. The shape that would answer it is a **read of the session**
publishing `locator` beside `row` for every row that came from a document, where
the absence means «this row came from no document» and means nothing else — not a
field on the question, which would carry the absence into the one object that
must be readable to a person.

## Non-vacuity

`iaam-3nqt` exists because a guard checked only existence, so each test here is
made against an input written in the test and asserts the reason rather than the
refusal.

`a_question_names_the_account_it_is_about_in_the_owners_own_words` is the rule.
`a_row_on_an_account_the_directory_does_not_hold_publishes_no_title` is its
falsification, and it is the specimen that matters: it asserts that the title is
absent **and** that it is not the identifier rendered as a string, which is the
one wrong answer that would satisfy a test checking only that something was
published.

`each_leg_of_one_movement_names_the_other_row_and_not_only_the_shared_identifier`
asserts both halves against each other — one identifier, and each side naming the
row it is not — so a pair that published the same row twice, or two identifiers,
fails.

**What no test holds.** Whether a person who has never read this codebase can act
on what he is read out is not decidable by a rule, and 0027 already says so. The
acceptance test is the owner.

Nor does anything mechanically hold §3's third obligation. The check is a grep —
`[Dd]ecision 00[0-9][0-9]` and `iaam-[a-z0-9]{4}` over the files whose doc
comments are rendered into the contract — and what makes a guard hard to write is
exactly the exception: the same citation is correct in a doc comment that is
never published, so a rule over the source text would fire on the sites that are
right. The remaining sites are named above so that the next pass has a list
rather than a search.

## Consequences

`PrintedRowDto` gains two optional fields and `OpenQuestionDto.pair` changes from
an identifier to an object carrying that identifier. The second is a breaking
change to a published shape, and it is taken rather than adding a third field
beside `pair` and `alike`: the value published today cannot be used without the
scan it exists to remove, so nothing is losing a capability it had.

A client that ignores every field added here reads exactly what it read before,
apart from `pair`.

Nothing about the row numbering changes. `row` is what it always was; what
changes is that the surfaces publishing it now say what it counts.
