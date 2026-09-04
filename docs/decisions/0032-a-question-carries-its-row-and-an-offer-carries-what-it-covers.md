# 0032. A question carries its row, and an offer carries what it covers

Date: 2026-09-04 · Status: proposed · Beads: `iaam-pm4w`, `iaam-briy`, `iaam-7iyg`, `iaam-xchm`

## Context

Four findings from one run of a first import, and they are one paragraph taken
apart. The owner stated the principle they are judged by:

> «Агент должен быть проактивным и стараться минимизировать количество
> вопросов… Можно было бы спросить, ага, это выписка из Т-Банка, значит все
> счета из него? Пользователь подтверждает сразу все вопросы.»

Two confirmations replaced roughly fifteen questions in that run. What every
section below is measured against is whether a client can put **one** answerable
sentence to a person where it previously needed many. A client that has to parse
prose, or that cannot see what a group holds, cannot do that.

### 1. A caller had to read structure back out of prose

To show the owner what a group of questions contained — dates, amounts,
directions, accounts, counterparties — an agent extracted them from each
question's `prompt` with regular expressions. It had no alternative: the engine
holds every one of those as a typed value while it builds the question, and
published them only joined into a sentence.

This is the defect the system already refuses one level down. A caller may not
interpret a document's rows; that is what a profile is for, and
`docs/import-boundary.md` argues it at length. A caller re-deriving structure
from prose **this engine wrote** is the same act with the same failure: an
expression that is right today and silently wrong when the wording changes — and
the wording is being actively rewritten under decision 0027.

### 2. The session's question is one string where a queue field carries two

Decision 0027 makes an owner-facing question two values, because the half saying
what turns on the answer is the half a relayer folds away into a sentence that
already reads as finished. `OpenQuestion::prompt` is still one string, and
`iaam-3ewp` made it one deliberately: the same string is the ingest verdict's
`question`, the session's `prompt` and the action queue's `reason`.

### 3. The question said an answer names an account, not which accounts exist

`IsTransferInternal` asks whether the far side is one of the owner's accounts
and, if so, which. Two of its answers name one. `ImportQuestionDto` carries the
candidates; the assessment's `OpenQuestionDto` did not, even after decision 0029
gave it the alternatives. `MissingInput` has had `candidates` for exactly this
since the queue was built: an item that only said an account was needed would
leave the caller to find out elsewhere which ones are eligible.

Decision 0029 considered adding them per question and declined — they would
repeat the directory once per question, and `needs_account` plus the per-question
route covers it. That holds for a document of a few rows. On a first import of a
few hundred the caller then makes one extra call per question to learn a list
that is identical every time.

### 4. One source word held four incompatible things

`offered_rules` groups still-unanswered rows by the word the source filed them
under and offers a condition on that word. On a real export one such word covered
a large share of the document and held at least four incompatible things:
movements between the owner's own accounts, payments to a person, payments to a
company, and others. One rule on that word would have been wrong for most of what
it matched, and the agent could discover this only by expanding the group by
hand.

**An offer whose group is not homogeneous is worse than no offer.** It is a
confident recommendation to make one wrong standing decision instead of many
right ones, and the owner adopting it is the failure the offer exists to prevent.

The word is not the defect. An institution files by its own purposes, and a
transfer word covers every transfer, inward and outward, internal and not. What
was missing is that the offer said nothing about whether the group was one thing.

## Decision

### 1. An open question publishes the row it is about, typed

`OpenQuestion` gains `printed: Option<PrintedRow>`, reaching the wire as
`OpenQuestionDto.printed`: the account the row is on, the amount with the sign
the source printed and its currency, the day the row states, the direction the
source stated, the party it named, and the word it filed the row under.

**The sentence stays.** It is what a person reads, it is the carrier the ingest
verdict and the queue item share, and `iaam-3ewp` established that a question
must name its row because four rows of one export once produced four identical
sentences. Nothing is moved out of it; the fields are beside it.

**One optional object and not six optional fields.** The values are one row, they
fail together — a stored observation this build cannot parse — and six optionals
would suggest a row could state its amount and not its account. It is the shape
`NewImportQuestion` already takes for the three halves of a question.

**This is not a second reading of the row, and `Resolver::render` argued it would
be.** That doc comment refused published fields partly on the ground that a
second rendering of one row is a pair of readings that can disagree, as
`ImportSessionContentsDto.row_count` documents refusing. There is no pair here: a
row with an open question produces **no planned fact**, so the two lists are
disjoint by construction, and what is published is what the source printed rather
than what the commit would post. The paragraph now says so.

**The description stays out**, on `row_mark`'s grounds: it is the row's whole
text, of unbounded length and written by the source, and it belongs in no
sentence read to the owner and in no field beside one. Every other value here was
already in the prompt for some question, so the fields disclose nothing the
sentence did not.

### 2. `OpenQuestion::prompt` stays one string — a question about a row is not a question about a field

This is the branch `iaam-briy` explicitly allows, and it is taken on the ground
that decision 0027's two halves are already satisfied here, in a **stronger**
form than a split would give.

0027 is about a field the owner fills in. Its `consequence` exists because such a
field — a title, a label, a balance, a date — has an **open** value and therefore
nowhere else to attach what turns on the answer. An import question is the
opposite shape: it has a closed vocabulary of answers, and decision 0029 already
publishes, per word, what that word decides — `AnswerShape::consequence`, read
from one place by all five publishers.

So the consequence of an import question is **per answer**, and it must be:
what follows from «fee» differs from what follows from «payment out», and a
single `consequence` string could only say something true of both. Splitting
`prompt` into `ask` and `consequence` would therefore produce a second half that
is one of two things, and both are refused elsewhere:

- **the generic stakes clause the prompt already carries** — «what you answer
  decides which figure the row moves in your money-flow report; each alternative
  says which» — which would then be published twice; or
- **the seven per-word consequences folded into one sentence**, which is a
  mapping from a word to its effect encoded as prose. `docs/api/conventions.md`
  §5 refuses that, and `Resolver::render` already refuses it in those words.

0027's worry is that a relayer trims the consequence away. A structured list of
words, each carrying its own sentence, is harder to trim than a second string,
not easier: dropping it is visibly dropping the answers.

What `iaam-briy` correctly identified is that the assessment obliged its reader to
recover typed values from prose. That is §1 above, and it is the repair. The
prompt is left as the one string four publishers share, which is what `iaam-3ewp`
made it and what this decision has no reason to undo.

**This is not settled forever.** If a fifth question is added whose answers share
one consequence, or if `AnswerShape::consequence` is ever folded into the prompt,
the argument here lapses and the split becomes right.

### 3. The accounts an answer may name are published once per assessment

`Interpretation` gains `answer_accounts: Vec<AccountCandidate>`, reaching the wire
as `InterpretationDto.answer_accounts`.

**Once here and not once per question, which is the opposite of
`MissingInput::candidates` and not a disagreement with it.** A queue item is read
alone: it is one field of one call, and there is no second item in the response to
hang a shared list on, so per-item repetition is the only shape available to it.
An assessment is one response holding every open question of the session, and a
first import holds hundreds. Repeating the directory under each of them would
publish the owner's whole account list hundreds of times to say something
identical every time — which is decision 0029's objection, and it stands.

What 0029 offered instead — `needs_account` plus the per-question route — is the
same arithmetic in another currency: one call per question to fetch one list.

**The per-question exclusion is derivable, and that is why this bead and §1 are
answered together.** `answer_account_candidates` drops the account the row is on,
because an account is not the far side of itself. A shared list cannot drop it,
because it is not about one row — so `printed.account` says which account each
question is on and the caller filters. One comparison against a value published
beside the question, not a lookup somewhere else.

`ImportQuestionDto.accounts` keeps doing the exclusion itself. That response **is**
about one row, and a client answering one question should copy an id out of a list
that is already correct for it.

**It is stamped into the session revision, and it is the only new section that
is.** The line is what a section is derived *from*. Every field of an open
question, `offered_rules` and `withheld_offers` come out of the questions and
observations the session already holds, which cannot change while the questions
the fingerprint already folds do not — this is decision 0029's argument for not
folding `offered_rules`, and it still holds. This list comes out of the owner's
**directory**, which he can change without touching the session at all, so leaving
it out would let an assessment read before he created an account and one read
after it carry one stamp.

### 4. An offer is made only where the word covers one thing, and a withheld offer says so

`OfferedRule` gains `contains: RowShape`. `Interpretation` gains
`withheld_offers: Vec<WithheldOffer>`, each carrying the word, the rows, the
shapes it holds, and one sentence saying why no rule is offered on it.

A `RowShape` is two facts the source stated: which way the money went, and whether
a party was named at all.

**Why the group is not narrowed instead.** The obvious repair is to key the group
by the word *and* the direction, as `QuestionSubject` keys a decision, and offer a
rule per key. It cannot be done, and the reason is older and stronger than this
bead: `Classification` carries no direction on purpose — «a rule fires on rows the
owner has never seen; a direction carried over from the row he wrote it on would
be asserted about all of them» — so `RuleMatcher` has no field that could express
one. A group narrowed by direction would publish a `covers` list of the outgoing
rows beside a matcher that matches the incoming ones too. The offer would claim
something the rule does not do, and `POST /v1/category-rules/preview`'s answer to
«what would this match» would contradict the offer beside it. **The condition and
the group have to be the same question**, which means the group is the word.

**Why two lists and not one list with a marker.** A marker is a field a client can
ignore, and the thing being marked is what makes the entry dangerous. A client
that walks `offered_rules` and relays every entry cannot relay a bad one; a client
that ignored a `mixed` flag would relay exactly the offers that must not be
relayed. Nothing is hidden by the split: every row named in `withheld_offers` is
in `open_questions` as well, so such a client loses a shortcut and never a row.

**Why the invariant is in the type.** `OfferedRule.contains` is one `RowShape` and
cannot hold two, so an offer whose rows disagree is not representable. No caller
has to check the claim, and no later author can make an offer that breaks it
without changing the type.

**Two facts and not three.** The question a row raises is determined by exactly
this pair — a named party with a stated direction is asked whether the far side is
the owner's, an unnamed outflow whether it was a fee, an unnamed inflow what
arrived, and no direction at all which way it ran — so a third field naming the
question would be one fact written twice, in a place where the two spellings could
drift.

**The reason is a statement and not an `OwnerQuestion`.** Decision 0027 governs
what is *put* to a person, and its two halves are what is being asked and what his
answer changes. Nothing is being asked here and no answer of his changes anything,
so a `consequence` would have to be invented — and an invented one is precisely
the sentence that reads as finished which 0027 exists to prevent. The other two
obligations bind regardless, because a client that shows it shows it to him.

## Non-vacuity

**What can be proved mechanically.**

- `an_offer_covers_exactly_the_open_rows_its_own_condition_matches` runs every
  published offer's own `matcher` over the session's rows and compares the result
  with `covers`. It is the guard that forbids narrowing the group past what the
  condition can express, and it fires on the repair this decision refuses.
- `a_word_covering_money_that_arrived_and_money_that_left_is_offered_as_no_rule`
  and `a_word_whose_rows_agree_is_offered_and_publishes_the_shape_it_claims` are
  the two sides: the second is the falsification for withholding everything.
- `a_row_the_source_gave_no_direction_publishes_none_whatever_its_sign` is §1's
  falsification. The row's amount is positive and the source stated nothing,
  which is exactly the condition `UnresolvedDirection` is asked under; a
  published direction there would answer the question the question exists to ask.
- `a_session_whose_questions_name_no_account_publishes_no_accounts` is §3's:
  neither answer to «was it a fee, or a payment out?» names an account, so there
  is nothing for the list to be for.

**What no test holds.** `RowShape` is made of what the source stated, which is all
this side has. It separates rows that ran opposite ways and rows that named
nobody; it **cannot** separate a payment to a person from a payment to a company,
because no field of the row says which. One shape is therefore a statement that
the document contradicts itself nowhere, not a guarantee that the owner would
answer alike — and the offer's own wording, which says what it costs to be wrong,
is his protection for the rest. A word that holds two kinds of outgoing payment
the source spelled identically will still be offered, and will still be a bad
rule to adopt.

## Consequences

A client working a session now reads, per open question, the row it is about in
typed fields; per assessment, the accounts an answer may name; and, per word the
source filed rows under, either a rule it is safe to put to the owner **with what
that word holds**, or a statement that no rule fits and why. It can group, sort
and total without touching the prose, and it can put one sentence to a person
about a word instead of one per row — where the word deserves it.

`GET /v1/import-sessions/{session}/assessment` gains one optional object per open
question and two optional lists on the interpretation. Nothing is removed, and a
client that ignores every new field reads exactly what it read before — with one
change of substance it cannot ignore: a word whose rows are not one thing is no
longer offered as a rule at all. That is the point.

The session revision changes for one new reason: the owner's account list. It
already changed when he created an account that resolved a name; now it changes
when he creates one that resolves nothing, because the assessment's answer to
«which accounts may an answer name» is then different.

## What this does not settle

- **Whether an offer should be raised on the counterparty a session repeats
  most**, which decision 0029 left open and this does not touch.
- **Whether a mixed word should be offered as several rules once a matcher can
  express a direction.** Nothing here proposes giving it one; the argument
  against is `Classification`'s and is about rules, not about offers.
- **Whether the queue folds items proposing an identical rule into one**, which
  decision 0029 also left open.
