# 0026. A rule may ask what the source filed the row under, and it is not scoped to a source

Date: 2026-09-04 · Status: proposed · Bead: `iaam-93lz`

## Context

Decision 0019 §6 settles what a source profile does with an institution's own
category: it transcribes the word and stops. There is no map from that word to
one of the owner's categories, and there will not be — his rules do that job,
because they are his, editable, and re-runnable over rows already recorded,
where a map baked into a profile is frozen into every fact at the moment of
import and correctable only by retracting the import.

Decision 0020 §2 gave the word a field of its own, end to end: `source_kind` is
what the operation **was**, `source_category` is what it was **for**, and the two
stopped travelling through one slot.

What neither noticed is that only one of the owner's two rule vocabularies could
read the field.

`iaam_core::category::CategoryMatcher::SourceCategory` matches it and decides
which of his categories a row is filed under. `iaam_ingest::classification::RuleMatcher`
decides what a row **is** — a fee, income, a movement between his own accounts —
and it asked about three things: the counterparty, a fragment of the description,
and `kind`, which is the source's *operation* word. It had no condition for the
source's category at all.

So the mapping story 0019 §6 rests on was half available. «This institution's
category *Bank interest* is my category *Interest*» was writable; «a row this
institution filed under *Bank interest* is interest on a balance» was not,
under any spelling.

It is acute for the profile that ships first. That export prints no
operation-type column whatsoever, so `source_kind` is `None` on every row it
produces, and the category is the only word the source contributes. For those
rows the classification vocabulary had nothing to condition on but a counterparty
the export often does not print and the whole of a description, which recognises
one row and practically nothing else — the emptiness decision 0008 was written
to end.

`ObservedRow` has carried `source_category` since `iaam-p683`, and
`ObservedRow::subject` did not pass it on. A matcher added without that seam
would have been a condition that can never fire, which is the defect `iaam-3nqt`
was filed for.

## Decision

### 1. `RuleMatcher` gains `source_category`, matched exactly

A fourth optional condition, joined with the other three by **and**, compared
for equality against `ClassificationSubject::source_category`. `asks_nothing`
counts it, so a rule naming only a category is an ordinary rule rather than one
that matches nothing, and `ClassificationRule::describe` states it, so a rule the
owner reads back names every condition it fires on.

Exactly, and not as a substring, for the reason `kind` is matched exactly: a
source's category is a value out of a vocabulary that source controls, not prose.
A substring test would let a rule about one of an institution's categories reach
every other whose name contains it.

### 2. The seam: the subject carries what the row transcribed

`ObservedRow::subject` fills the field from the row, and
`scenarios/classification.rs::subject` fills it from
`Provenance::source_category` for recomputation. Without both, a rule the owner
writes at intake would match at intake and match nothing when he edited it and
asked for the plan — which is exactly what `iaam-oz8c` was about for the
description, one field along.

### 3. One word, one meaning, in both of the owner's rule vocabularies

`source_category` names the same string in `CategoryMatcher::SourceCategory` and
in `RuleMatcher::source_category`: the word the source filed the row under, read
out of the same place, transcribed verbatim, never mapped, and compared exactly
in both. What differs is the question asked of it and what a match then decides —
*which of my categories is this?* against *what is this row?* — which is why
there are two matcher types and not one.

It follows that neither may quietly widen the word. A classification rule on a
category does not fire on a row whose source used that string as its *operation*
word, and a rule on the operation word does not fire on a category. That pair
used to round-trip through one slot and mean the wrong thing in silence; the
vocabularies keep it apart now that the fields do.

### 4. The condition is **not** scoped to a source, and this is the trade

The objection is real: an institution's word for a category means nothing in
another institution's export, and a rule that fires across sources is a wrong
classification nobody asked for. It is refused all the same, because every
handle this journal actually holds scopes the wrong thing.

- **`SourceId` is not an institution.** It is derived from the owner, the
  **account**, and the channel (`SourceId::declared`). A rule scoped by it would
  have to be written again for every account the same bank's exports arrive on,
  and again for a paste of what had been a file. The owner would maintain one
  rule per account and find that the fourth account's statement asks him
  everything afresh.
- **`ParserVersion` names a profile *and its version*.** A rule scoped to
  `profile/x/3` stops firing the day profile `x` is corrected to version 4 —
  silently, since a rule that matches nothing is indistinguishable from a row
  nothing covers. It would also never fire on a row an external converter
  transcribed, and 0019 §8's whole point is that a profile is one of several
  origins.
- **The sibling conditions are unscoped for the same reason.** `kind` matches a
  vocabulary one source controls and is scoped to nothing;
  `CategoryMatcher::SourceCategory` matches this very field and is scoped to
  nothing. Scoping this one alone would make `source_category` mean one thing in
  a category rule and a narrower thing in a classification rule, which is exactly
  what §3 forbids.

What bounds a rule that is too wide is what already bounds the other three. The
conditions join with **and**, so a category condition is narrowed by a
counterparty or a description beside it and cannot reach an institution that
prints neither. The rule is the owner's own, it is listed where he reads it, and
retiring it replans what it classified. And the outcome vocabulary is small
enough that a collision has to be a real one: two institutions printing the same
category word and meaning different *kinds of operation* by it, not merely
different spending.

A scope is the right thing to add the day the journal names an institution.
Adding it now would mean inventing that name, and inventing it inside a rule
matcher is the worst place to put it.

### 5. A rule on a category is not tested against a fact from before the split

Decision 0020 §3 fixed that a fact below schema version 14 may carry the source's
*operation word* in `source_category`, refused a migration because the two cannot
be told apart afterwards, and gave the reader the version boundary instead. That
boundary is now named — `SOURCE_CATEGORY_IS_A_CATEGORY_FROM` — and recomputation
reads the field only at or above it.

Nothing is rewritten and nothing is guessed. The older row is reconsidered on the
evidence the journal holds for it, which is the sentence §3 already wrote about
its operation word: `Provenance::source_kind` is `None` on every fact below 14,
and this is the same sentence one field over.

The cost is a false negative — a pre-14 fact whose source really did print a
category is no longer reachable by a category condition. That is the direction
this program takes such choices in: a rule firing on evidence that may not be what
it claims puts a wrong fact into a correction plan, while a rule that does not
fire leaves the row as the owner already accepted it.

### 6. A proposal may name the category, third of four

This amends decision 0008's ordering. `matcher_for` proposes exactly one field,
now chosen: counterparty, else the source's operation word, else **the source's
category**, else the whole description.

Third and not second, because the operation word answers the question being
generalised — what the row *was* — and the category answers what it was *for*,
one axis over. Before the description, because it is a value out of a closed
vocabulary and a description is prose taken whole.

0008's own reasoning demands the insertion rather than merely permitting it. Its
last resort exists because `Generalisation::Impossible` claims no rule can be
built from the row under any token, and that claim would be false. For every row
of the first profile the chain fell straight to a whole description — a standing
decision settling the one line the owner had already settled by hand, which is
the emptiness 0008 was written to end.

## What was rejected

**Reusing `kind` for the category.** It is the defect 0020 §2 took the two words
apart to end, arriving from the other side.

**Scoping the condition to a `SourceId` or a profile.** §4.

**Matching the category case-insensitively, as `description_contains` is.** The
description is prose a source writes freely; a category is a value it prints from
a list. Case-folding a controlled vocabulary buys nothing and hides a source that
really does distinguish two.

**A migration moving pre-14 categories, or reading them anyway.** 0020 §3 refused
the first and §5 above declines the second for its reasons.

**Adding the condition without carrying the field into the subject.** A matcher
that can never fire is worse than none: it publishes a capability the owner will
write rules against and get silence from.

## Consequences

- `RuleMatcherDto` gains `source_category`, so `POST /v1/classification-rules`
  accepts it and the listing prints it. Absent stays absent on the wire, so
  nothing a client already sends changes meaning.
- Rules already stored carry no `source_category` key. A missing key reads as
  `None` — «this rule does not ask about the category» — so no standing decision
  of the owner's is widened by a deployment.
- The stored matcher shape gains a fourth key, written explicitly as `null` when
  unasked, as the other three already are.
- A question's `available` generalisation may now propose a category condition,
  and the action queue presets it in the body the owner posts.
- Decision 0019 §6 is true for facts at schema version 14 and above: the owner's
  rules can now do both halves of the job a profile may not do at all. Below that
  version, a category is not offered as evidence, by §5.
- `CategoryMatcher::SourceCategory` still reads `Provenance::source_category`
  without a version guard. That is a reporting assignment rather than a
  correction plan, and changing it is a separate decision about the owner's
  category reports; it is named here so the asymmetry is on the record rather
  than discovered.
