# 0020. A fact names its reader, and the source's two words stay apart

Date: 2026-09-04 · Status: proposed · Beads: `iaam-h69n`, `iaam-p683`, `iaam-kobz`

## Context

Decision 0019 §5 and §7 depend on two properties of the journal that it did not
have. This decision implements them and answers the one question 0019 left
open — what a reader is owed about the facts already recorded.

**Nothing recorded what had read a row.** `iaam_ingest::operation::normalize`
held a constant, `ParserVersion("ingest/manual/1")`, and stamped it on every
event it built. One channel out of five replaced it afterwards. So the CSV
parser's rows, the import session's rows, and a replacement written by the
correction channel all claimed to have been typed by hand, while a reversal
written by that same correction channel — which does not go through `normalize`
— correctly said `correction/1`. `CORRECTION_PARSER_VERSION`'s own doc comment
said it was stamped on *every* correction fact, and it was not.

That is a lie of omission on its own. It is load-bearing for 0019: the whole
recovery story for a buggy source profile is "the facts version 3 wrote are a
set you can find and retract", and that set does not exist while every fact
names the same writer.

**Two different facts shared one slot.** `ObservedRow::envelope` filled the
operation's `source_category` from the row's `source_kind`, and
`scenarios/classification.rs` read `Provenance::source_category` back out as
`source_kind`. The pair round-tripped, so no test failed. But
`CategoryMatcher::SourceCategory` matches `source_category` for real, and for
every observed row that field held the source's *operation word*. A category
rule the owner wrote on a source's category never matched an observed row, and
one written on an operation word matched rows he was not describing.
`ObservedRow` had no `source_category` field at all, so the observation channel
could not carry a source's category even when the document printed one.

## Decision

### 1. The reader is an input to normalisation, and there is no default

`NormalizationContext` carries `parser_version` beside the owner and the source.
It is a plain field of a struct every caller builds by literal, and there is no
`Default` and no fallback: **a caller that does not say what read its rows fails
to compile.** A default is how this defect arrived, and a default is what would
bring it back the first time a channel is added.

What each caller supplies:

| Channel | Version | Because |
|---|---|---|
| `POST /v1/ingest/operations`, import-session rows | `ingest/manual/1` | the caller stated the row; nothing here read a document |
| `POST /v1/ingest/csv` | `ingest/csv/1` | `csv_source::parse` read it, and it is software with a version |
| broker synchronisation | the channel's own | as it already recorded |
| a broker report upload | the report parser's | as it already recorded |
| `POST /v1/corrections` — replacement | `correction/1` | this code wrote the fact, exactly as it writes the reversal beside it |
| journal facts | `ingest/journal/1` | its own parser, as its constant already said |

Two of these change what is written. `ingest/csv/1` is new: a row this parser
produced and a row an agent typed were previously the same fact in provenance,
so a defect in the parser named no set of rows. The replacement correction moves
from `ingest/manual/1` to `correction/1`, which is what its channel's constant
always claimed.

**The second place is removed rather than kept in step.** `documents.rs` and
`sync.rs` each rebuilt the whole provenance after normalisation to change the
version. Rebuilding to change one field silently dropped every other field the
normaliser had filled — the source category, the description — and only stayed
harmless because both parsers happen to fill neither. The document channel now
replaces the raw hash alone, through `Provenance::with_raw_hash`, and says why:
a report row is identified by its document and locator (§10.6 level 4), which
`normalize` has no document to know.

### 2. `source_kind` and `source_category` are separate fields end to end

`Provenance` gains `source_kind`; `SubmittedOperation` gains `source_kind`;
`ObservedRow` gains `source_category`; `envelope` carries each word to the field
that word belongs in; `scenarios/classification.rs` reads the operation word out
of `source_kind`; and the wire's `OperationDto` gains `source_kind` beside the
`source_category` it already had.

**All of it in one change**, because half of it is worse than none: giving
`ObservedRow` the field without correcting the two sites would put two different
things in one slot at once, and an intermittently wrong field is harder to find
than a consistently wrong one.

The journal row published by `GET /v1/journal/events` carries `source_kind` as
well. A field a rule fires on that no response ever shows is a rule the owner
cannot check.

### 3. Nothing already recorded is rewritten, and this is what a reader is owed

The journal is append-only. Facts recorded before this change keep the version
they were stamped with and keep whatever is in their `source_category`. There is
no migration, and there must not be one:

- **The repair is not determinable.** An event whose `source_category` holds an
  operation word cannot be told apart from one whose source really printed that
  word as a category. Both paths stamped `ingest/manual/1` and both wrote the
  same field, so a migration would have to guess — and a wrong guess writes, as
  the source's own category, a word the source never used there. That is the
  false transcription decision 0019 §2 refuses when a profile does it; it is not
  better done by a script.
- **Provenance is evidence, not bookkeeping.** It records what a path meant at
  the time. Rewriting it destroys the only thing it exists to hold.

What the reader gets instead is a statement and a boundary:

- `SCHEMA_VERSION` moves to **14**, so "before this" and "after this" is a
  question the journal answers per fact rather than per deployment. A row at
  version 14 or above whose `source_category` is set means the source printed a
  category; below it, on the observation path, it may be an operation word.
- `Provenance::source_kind` is `None` for every earlier fact, and `None` means
  "not recorded" — never "the source said nothing". A rule reading it must not
  resolve the absence either way, exactly as `declared_by` and `import_session`
  already require.
- The rows a given reader wrote are, from now on, a query over
  `parser_version`.

Recomputation reconsiders an older row on the evidence the journal holds for it:
a description, a counterparty, and no operation word. That is a narrower subject
than the row had at import, and it is the honest one — the alternative is
feeding a classification rule a value that may be a category.

### 4. `ObservedRow::movement` documents what it does

The doc comment opened "The source's own direction word first; failing that, the
sign it printed", and then argued at length why such a fallback would be wrong.
The body implements no fallback. The sentence is corrected, not the body: the
sign is evidence kept on `amount_minor` for whoever weighs evidence, and a bank
that prints every amount positive must not have every row read as an inflow.

## What was rejected

**A default parser version, or `Default for NormalizationContext`.** It is the
defect. A caller that forgets must not compile.

**Keeping the post-hoc overwrite beside the context.** Two places deciding one
field is the shape this defect had; the second one drifts, and the drift is
invisible because both write something plausible.

**A migration moving `source_category` to `source_kind` for observed rows.** §3.

**Folding the reader into the source, or into the import.** A source is what
deduplication is scoped by and an import is what a retraction is keyed on;
neither names a reader, and a session is opened per declaration, which names an
account and a label. The version belongs to the batch.

**Reusing `ingest/manual/1` for the CSV route.** It is the value for a row
nothing here read, and these rows were read by a parser in this product. Sharing
the string would have kept the two indistinguishable, which is the defect.

**Filling `source_kind` from the broker adapters' own operation word.** The
Tinkoff channel carries `ChannelOperation::source_kind` and currently discards
it. Transcribing it is right and is not this change: it would start matching
classification rules against rows that have never carried the field, and that
belongs in a change whose tests are about broker rows.

## Consequences

- A caller that has been sending its operation word in `source_category` on an
  observation must move it to `source_kind`. The old field keeps working and now
  means what it says; facts already written are untouched.
- `SettlementLagPolicy::with_profile` keys a proven settlement band on a
  `ParserVersion`. Its table is empty at version 1, so nothing changes today —
  but a band added for `ingest/csv/1` is now a band that can be added, which it
  was not while every channel shared one string.
- Adding a channel now costs one line naming its reader, and forgetting it is a
  compile error rather than a fact that quietly claims to be hand-typed.
