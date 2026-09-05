# An operation's history

**Status:** design, approved in conversation on 2026-09-06.
**Beads:** `iaam-j1i5` (P1) is the half this design settles by choosing its second
option; the history route is the surface that makes the choice visible.

## 1. What he asked for

He asked, in his own words, whether a row a rule filed automatically can later be
corrected when every operation of the group is right except one — and whether the
agent can then show him the whole group. The wave that answered that question
(`wave-ai/what-a-rule-did`) gave him the group and the standing rule. He read the
answer and said the missing piece is **a log of decisions**, and then said what he
meant by it: **the change to an operation**.

So the thing to build is: ask about one operation, get its life.

## 2. What exists, and what is missing

The journal is append-only and a correction is two facts, not an edit. Correcting
an event writes a reversal naming it and a replacement naming it; `resolve`
excludes the reversed one and refuses a second replacement of the same event, so
the chain is linear.

Every fact carries `relation`, which points **backwards** at the fact it reverses
or replaces. Nothing points forwards. Looking at a row, he cannot see what became
of it; assembling the chain means reading the whole journal by eye. There is no
route that takes an event and returns its history, and `relation_target` carries
no index, so there is no cheap way to ask "what named this one".

Two smaller gaps sit underneath:

- **`recorded_at` is written and never read back.** The store stamps it on every
  insert; it is not on the domain `Event` and no port returns it. "When did this
  change" is therefore unanswerable today.
- **A row settled by an answer that also minted a rule records `NoRule`**
  (`iaam-j1i5`). The rule filter returns none of that group, correcting one of
  them reports no standing rule, and `still_filed` counts none of them. Nothing
  here is a lie — no rule *did* file those rows, the owner did — but the group a
  single decision of his reached is exactly what a history has to be able to name.

## 3. The design

### 3.1 A history is a list of acts, not of facts

One correction is two events. Publishing the events raw would show him two
entries for one thing he did, and he would have to know that a reversal and a
replacement of the same target are one act to read it. They are one act, and the
history says so: the pairing is unambiguous, because both name the same target,
both arrive through the correction channel, and at most one replacement of an
event can exist.

Three acts and no more:

- **`arrived`** — the fact entered the journal. The first step of every history.
- **`corrected`** — a reversal and a replacement of the same target: the fact was
  superseded by another. The state after it is the replacement.
- **`retracted`** — a reversal with no replacement: the fact stopped counting and
  nothing took its place. There is no state after it.

`retracted` covers a whole import taken back (`POST /v1/corrections/imports`)
exactly as it covers one row, and the history does not distinguish them: what the
owner is looking at is his operation, and "this was taken back" is the same fact
about it either way. Which act it belonged to is answerable from the import the
reversal names.

### 3.2 Entering anywhere in the chain

The history is addressed by an event identifier, and **any** identifier in the
chain returns the same history: the original, the current state, or one of the
reversals written along the way. Walking is done in both directions — backwards
by `relation.target` to the head, then forwards by the new index to the tail.

A reversal is part of an act rather than a state of the operation, so entering by
one resolves to its target's chain. Without this, the identifier he most often
holds — the one a correction just handed back — would answer a different question
from the one he holds from the import.

### 3.3 What a step publishes

Each step carries:

- `act` — one of the three words above.
- `at` — when the act was recorded, from `recorded_at`. On `arrived` this is when
  the fact was admitted, which is not its effective date and is not published as
  one.
- `state` — the fact as it stands after this act, as the existing
  `JournalEventReadDto`. Absent on `retracted`, because nothing stands after it.
- `changed` — which aspects of the fact this act made different from the previous
  state. Absent on `arrived`, which changed nothing, and on `retracted`, which
  left no state to compare.
- `events` — the identifiers this act wrote (`reversal`, `replacement`), so every
  entry is addressable and a correction of a correction can be asked for by name.

`changed` is a closed vocabulary computed from the two published states, never a
rendered before-and-after: both states are right there, and rendering the
difference twice is two answers that come to disagree. The aspects are `kind`,
`amount`, `account`, `dates`, `counterparty` and `confidence`.

**A category is not among them, and that is a real limit to state rather than
hide.** A category is not recorded on the fact; it is decided by the owner's
category rules when a report is computed. So "I filed this under the wrong
category" is not a correction of an operation at all, and this history will not
show it. Saying so is the point: an agent that quietly returns an empty
`changed` for it would tell him nothing happened.

The response also carries `current`: the identifier of the fact that counts now,
absent where the operation ends in a retraction.

### 3.4 The third settlement (`iaam-j1i5`)

`RuleSettlement` gains a third value:

```
AnsweredMintingRule { rule: ClassificationRuleId, version: u32 }
```

wire code `answered_minting_rule`. It says: the owner answered the question this
row raised, and that same answer minted rule R at version V, which settles rows
read from now on.

It is kept apart from the two that exist because they are different facts about
who decided:

- `Rule` — a rule already standing filed the row without asking him.
- `AnsweredMintingRule` — he decided, and the decision also became a rule.
- `NoRule` — a reading ran, none of his rules matched, and the answer minted
  nothing.
- absence — nothing was recorded, which is never evidence of any of the three.

Every place that asks "which rule is behind this row" treats the new value as a
rule and therefore covers the group the answer itself reached:

- `RuleSettlement::rule()` returns the rule and version, so the journal's
  `settled_by_rule` filter returns those rows — "show me what this decision did"
  finally includes the rows the decision was made on.
- `standing_rule_for` reports the rule when one of them is corrected.
- `still_filed` counts them.

Every place that asks "who decided" must distinguish it, and `code()` is the one
place the words are spelled.

**Where the value comes from.** The rule is minted after the answers are
written, and `resolution_of` reads the answer off the **observation**, not off
the question. So the minted rule is recorded where the answer already is:

1. `import_observations` gains `answer_rule` and `answer_rule_version`
   (migration `0030`), written for every row the answer settled — the row the
   caller addressed and every row its reach reached. The version is recorded at
   the moment the rule is minted and never read from the rules port at commit: a
   rule can be edited between the answer and the commit, and the fact must record
   the version it was filed under.
2. `import_questions.rule` is left exactly as it is. Attaching the minted rule to
   the sibling *questions* would be the other way to carry it, and it would change
   what the assessment says about each of them — a question that reported no
   recorded generalisation would start reporting one, and the owner would be told
   a dozen times that a rule was written from one answer. The observation is the
   thing commit reads; the question is the thing he is shown.

Because the store lifts `settled_by_rule` out of the payload from
`RuleSettlement::rule()`, the journal's existing rule filter and index cover the
new value without a second column.

### 3.5 The store

- Migration `0030`: an index on `(owner, relation_target)` where
  `relation_target IS NOT NULL`, and `answer_rule` / `answer_rule_version` on
  `import_observations`. Schema version 15 → 16.
- A new port method takes an event identifier and returns the chain, each entry
  with the event and its `recorded_at`. `recorded_at` reaches the scenario on a
  store-side row type and is **not** added to the core `Event`: it is a fact about
  when this instance wrote something down, not part of the fact that was written.
- Walking is `O(depth)` index lookups and never scans the journal.
- The scenario refuses an event the owner does not own with the same 404 the
  journal listing gives an idempotency key that addresses nothing.

### 3.6 The surface

`GET /v1/journal/events/{event}/history`

- 200 — the history, steps oldest first.
- 404 — an identifier that addresses no event of his.
- 422 — an identifier that cannot be read.

Steps are oldest first because a history is read forwards: he wants to see what it
was before he sees what it became. The listing beside it (`GET
/v1/journal/events`) stays exactly as it is; this is a second question, not a
filter on the first.

No pagination. The chain is one operation's corrections, its length is the number
of times he changed his mind about one row, and a page boundary in the middle of a
history would hide the very thing it exists to show.

## 4. What this does not do

- It does not offer to retire or rewrite the rule behind a row. That is his act
  under his own credential, and the correction outcome already states the cost
  without performing it.
- It does not back-fill. Facts written before `AnsweredMintingRule` existed keep
  `NoRule`, and facts written before any settlement was recorded keep the absence.
  A back-fill would put a claim into the journal that nobody made.
- It does not show category decisions (§3.3).
- It does not merge the history of two operations that a pairing decided are two
  halves of one transfer. That is a different relation from replacement and a
  question of its own.

## 5. Acceptance criteria

1. Asking for the history of any identifier in a chain — the original, the current
   state, or a reversal written along the way — returns the same history.
2. An operation corrected twice reads as three states and two `corrected` acts,
   each naming when it happened and which aspects changed.
3. An operation whose import was retracted ends in a `retracted` act with no state
   after it, and `current` is absent.
4. An operation never touched reads as one `arrived` act, and nothing about it is
   phrased as a change.
5. Correcting any row of the group an answer reached reports the standing rule the
   answer minted.
6. `GET /v1/journal/events?settled_by_rule=R` returns both the rows R filed on its
   own and the rows the answer that minted R settled, and each row says which of
   the two it is.
7. "He answered this row himself" and "his answer minted a rule and settled this
   row" are distinguishable on the wire and never conflated.
8. A fact recorded before either recording existed still says nothing about rules,
   and no reader turns that silence into a claim.
9. The published contract describes every new field; a schema-only change with no
   description is a defect here as everywhere.

## 6. Risks

- **`recorded_at` has never been read.** Rows written before this design have one,
  written at insert; nothing has ever depended on it, so it has never been wrong
  in a way anybody noticed. The history publishes it as what it is — when this
  instance wrote the fact down — and never as when the operation happened.
- **The reach's rows must be stamped, not only the addressed row.** The value of
  the whole change is that the group a single answer settled is findable; stamping
  only the row the caller named would leave the group split in two and the defect
  half-fixed in a way that reads as fixed.
- **Schema 16 is a migration on the answers table and on an index.** Neither
  rewrites a fact.
