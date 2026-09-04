# 0024. The system says which accounts a statement asked for

Date: 2026-09-04 · Status: proposed · Beads: `iaam-x9ls`

## Context

The first import into an empty instance broke no contract. Every refusal was
correct, every sentence was readable, and the path was still a closed loop:

- declaring an import session requires an account (`DeclaredSourceDto.account`);
- the account is created from the name the statement prints for it, because that
  is what decision 0004's tiering resolves a row against;
- that name is learned only by handing the document over;
- handing the document to an empty directory refuses every row.

There is an escape and it works — open a session that declares no account, hand
the document over, read the refusals — but as a *first* step it is a dead end,
and nothing published anywhere says it exists. What it cost, measured: a ninety
kilobyte response repeating one refusal two hundred and twenty times, for **seven
distinct names**.

Three surfaces are involved and each is silent in its own way.

**The document response names no set.** `SourceDocumentDto` carries `rows` and
nothing above them. A caller that wants to know which accounts to create must
walk every row, pick the refusals whose field is `account`, and deduplicate the
names out of a sentence written for a person to read.

**A printed name that resolves to nothing is invisible to the assessment.**
`AccountResolutionDto` is `{ resolved, missing, conflicting }`, and `missing` is
documented as "named by a row and absent from the owner's directory". A row
names an account by identifier, so `missing` is a list of identifiers — and a
printed string that matched nothing has no identifier at all. The section whose
whole purpose is to answer "which accounts did these rows name" structurally
cannot answer it in the one case where the answer decides what the owner does
next.

**The queue asks for an account it cannot name.** `create_first_account` says
"create an account". It cannot say which, and it publishes `/title` as the one
missing field — so an agent following the queue literally invents a title, the
import refuses every row against it, and the loop closes again.

## Decision

### 1. The reading publishes the set, and the set is arithmetic

`engine::read` tallies, per document, the account names it could not place: the
distinct printed strings, in the order the document first printed them, each with
the number of that document's records that printed it. `DocumentReading` carries
it, `DocumentImport` carries it, and `SourceDocumentDto.unresolved_accounts`
publishes it.

**It is a summary of the refusals and never an addition to them.** Every one of
those records is still refused individually, by field, by locator and by name, in
`rows` — that is the contract and it does not move. What is new is that the same
refusals are also said once per account, so a statement whose two hundred and
twenty rows are on seven unknown accounts is answered in seven lines.

Nothing in it is a conclusion, and the boundary is exact. Counting the records
that printed a string is arithmetic over the reading. Deciding what the string
*is* — which account of the owner's it means, whether it should exist, what he
ought to call it — is not, and nothing here does it. The count is not a count of
movements either: those records were refused, so nothing was read out of any of
them, and the field is named `records` rather than `rows` for that reason and
because `rows` beside it is the list of outcomes.

The name is recovered structurally and not by reading the refusal's own sentence
back. `engine::row` returns an internal refusal that carries the printed cell
where the account column was what failed; the wording belongs to
`AccountNames::resolve`, which is entitled to change it, and a summary that
parsed it would break the first time it did.

### 2. A fourth field, not a widening of `missing`

`AccountResolution` and `AccountResolutionDto` gain `unrecognised: Vec<String>`.
`missing` keeps its meaning and its type, and gains one word in its doc comment:
named by a row **by identifier**.

The argument is what a caller can be wrong about, and decision 0020 is the
precedent, one field over. `source_kind` and `source_category` round-tripped
through one slot for months; nothing failed, no test noticed, and the cost was
that a rule the owner wrote on a source's category never matched a row while one
written on an operation word matched rows he was not describing. Two facts in one
slot are not caught by the type system and are not caught by a test that writes
and reads through the same path.

Widening `missing` would be exactly that shape:

- **The types do not agree.** `missing` is `Vec<Uuid>` because a row that named
  an account carried a `Uuid`. A string a document printed has no identifier —
  that is the *state* being reported — so widening means a union, and a union
  means every reader branches on which half is filled. A reader that does not
  branch either fails to parse or silently drops the entries that matter most.
- **The quantifications do not agree.** `resolved`, `missing` and `conflicting`
  are statements about rows this session **holds**: they say what the commit
  would write. `unrecognised` is a statement about records that were **refused**,
  so the session holds no row for any of them and no figure below counts them.
  Reading the second as the first says the import is about to record something it
  will not.
- **The struct already answers this question the right way.** `conflicting` is a
  `Vec<String>` sitting beside two identifier lists, for a third kind of thing
  that is not an identifier. The shape is available and the precedent is in the
  same struct.

The field is fed from the instance's record of the readings of this session's
documents (§3), filtered through the directory as it now stands: a name the owner
has since created an account for is not listed, because it is no longer true.
Nothing recomputes it from the session's rows, and nothing could — those records
never became rows.

### 3. The queue reads the names from an instance fact the reading wrote

`ActionKind::CreateAccountNamedByDocument` is a new item, raised once per printed
name the owner's directory does not place. It carries the name, the number of
records that printed it, how many kept documents printed it, and it targets
`create_account` with `provider_account_id` preset to the printed string.

The names come from `document_unresolved_accounts`: a table written when a
document is read, holding the printed string, the records that printed it, the
position the document first printed it at, the document's hash and the session
it was read into.

**It is an instance fact and not a journal fact.** Nothing was recorded when
those names were read — every record that printed one was refused — so there is
no event to carry a name in its provenance, and writing one would record a
movement nobody read. The journal holds facts about the owner's money; "a
document printed a string I could not place" is a fact about a reading.

**The record is a transcription and carries no verdict.** It says a document
printed a string. Whether the directory places it is asked wherever the row is
read, against the accounts as they then stand, through
`AccountNames::resolve` — the one implementation of decision 0004's tiering,
reached through the same view-to-vocabulary translation the reader uses. A stored
verdict would say "missing" about an account created an hour later, and a queue
that publishes work already done is a queue the owner learns to ignore, which is
the failure the whole module is written against.

**A re-reading replaces the document's whole set.** A second reading answers the
same question against a directory that has moved, and answers it better; two
answers side by side with nothing saying which is current would also count one
document's records twice. An empty set is a statement too — this reading placed
every account the document named — and it is what clears what an earlier reading
recorded.

The alternatives, and why each was refused:

- **Re-reading every kept document on the queue's path.** A parse of every
  statement the owner ever uploaded, on every reading of the queue, to answer a
  question that was already answered when each was read. `iaam-4jso` was filed
  for widening `frontier` exactly like this and the fix landed one wave ago: a
  fold that refuses takes the queue with it, and the queue is the surface the
  owner recovers *from*. This fold would refuse on any document a later profile
  release stopped recognising — that is, precisely when it is needed.
- **Reading the refused records out of the session.** There are none, and that
  is correct: a record the reader could not read never reached the session,
  because a session holds rows and that was not one.
- **Publishing only the call that recovers the names**, leaving the queue to say
  "hand me a document and I will tell you". It is honest, it costs no schema, and
  it was refused for one reason: a resolution's target is an `OperationKey`, and
  the document channel is not one (`iaam-1tij`). So the option the queue could
  actually publish is `open_import_session` — the paste-rows route — which is
  the defect wave T recorded for `start_account_import` in `iaam-j5oz`: **an item
  publishing only the fallback points every agent at the fallback.** The item
  built here publishes `create_account`, which is already a key, and presets the
  one field that makes the next reading of the document work.

### 4. `provider_account_id` is preset; `title` is not

The asymmetry is decision 0004's and it is the whole reason the item is worth
building. The printed string is what the source repeats; a title is what the
owner reads and may rename tomorrow. Presetting it as the title would resolve the
rows — the third tier matches a title, trimmed and case-folded — and would do it
through the vocabulary `AccountNames::resolve` deliberately refuses to offer in
its own refusals, on the ground that a name is not an identity. The first rename
would then stop a statement importing, silently. As the identifier the source
prints, the same string is the second tier, it beats a title, and it survives
being renamed.

`provider` is missing rather than preset and is marked as the owner's: it is his
label for the source, it is what scopes the identifier so two sources printing
short sequential numbers cannot collide, and no document says what he calls the
institution.

The item is `NeedsOwnerInput` with every field the queue can supply supplied.
Whether one of his accounts is meant by this string — and whether it is one
account or two — is his judgement. A complete request does not change who may
send it.

### 5. `create_first_account` names the way out

The item cannot say which account to create, and that is a property of the state
rather than a defect of the item: nothing has been read, so no name exists, and
an item naming one would be inventing it. What it can do is say that the question
has an answer and how to get it — open a session, which need declare no account,
hand it the document, read the accounts the document asked for — and then create
each with the printed string as the identifier its source prints.

It is written as prose and not as a second resolution for the reason §3 gives:
the channel is not an `OperationKey` yet. It names no path, method or status
code; the queue names calls, and a route typed into prose is a second route
table.

## What was rejected

**Recording the unresolved names in the journal.** §3. Nothing happened that a
journal records.

**Storing the verdict — "this account is missing" — rather than the
transcription.** §3. It is the one field that goes stale in the direction that
hurts.

**Widening `missing` into a union of identifier and string.** §2, and decision
0020 is the same defect one field over.

**A summary that says what a row *was*.** Refused before this was designed and
restated here because this is the section somebody would add it to. Counting the
records that printed a name is arithmetic over the reading; proposing what the
account is, or what it should be called, is a conclusion, and the engine's own
output type is built so that a profile cannot reach for one.

**Guessing an account for a printed name the directory does not hold.** A row
whose account cannot be placed refuses, loudly, by name. `skipped_outside_contour`
was rejected in the parity port for being a silent drop of a month, and this
would be a silent drop of an account.

**Teaching a profile the owner's account names.** Decision 0019 §2 and decision
0005: a profile names a column and never a value of his, and the account map is a
run-time input that lives outside the repository.

**One item per document rather than per name.** Two statements of one bank naming
the same unknown account are one account to create, and an item per document asks
for it twice.

**An `ActionSubject` for the item.** The vocabulary's account subject carries an
identifier and a title. This item has neither, because the account does not
exist; filling either with a printed string would publish, as one of the owner's
accounts, something that is not one. The absence already reads as "this is not
about an account you hold".

## Consequences

- **The first reading of an empty instance's queue says how to find out which
  accounts to create.** The loop is still a loop — the names really do only exist
  in the document — but it is now a loop the queue walks the caller through in one
  reading instead of one an agent has to discover by provoking a refusal.
- **The second reading names them.** Once a document has been handed over, the
  accounts it asked for are items with names, counts and a preset request, and
  they close themselves: an account that answers to the string removes the item
  without the document being read again.
- **The document response is readable at scale.** Seven lines instead of two
  hundred and twenty refusals, with the refusals still there for whoever needs
  the locator.
- **The plan's stamp changes when the set does.** `unrecognised` is folded into
  the session revision, so an account created between the assessment and the
  commit refuses a stale commit, as every other change to what the plan says
  does.
- **One migration, `0026`.** The schema head was 25 when this was written; the
  number reserved for this work was 33, and taking it would have stranded 26–32
  on any database that ran this build first, because `migrate` advances
  `PRAGMA user_version` monotonically (`iaam-0xn0`). A collision on 26 is a merge
  conflict, which is visible; a gap is silent.
- **The queue pays one extra read, and only where it can produce an item.** The
  recorded names are one indexed query; the owner's account details are read only
  when that query returned something, which is the same bargain `retired_products`
  strikes for the journal fold.

## What this does not settle

- **The document channel is still not an `OperationKey`** (`iaam-1tij`). Until it
  is, neither this item nor `create_first_account` can publish it as a
  resolution, and both say in prose what they would rather say in a target.
- **`start_account_import` still offers only the fallback for a cash account**
  (`iaam-j5oz`). Unchanged here, and the same bead.
- **A document that names its account in a preamble cell** is still outside
  profile schema version 1, so such a statement raises none of these items — it
  is refused whole, for want of a declaration, which is decision 0019's
  unfinished business and not this one's.
- **Nothing here helps an account named only by a counterparty string.** That is
  `conflicting`'s neighbourhood and a different question: the far side of a
  movement is a guess about a string, and the account whose statement a record is
  on is not.
