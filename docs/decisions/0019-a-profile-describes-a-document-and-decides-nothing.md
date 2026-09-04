# 0019. A profile describes a document, and decides nothing

Date: 2026-09-04 · Status: proposed · Beads: `iaam-ewty`

## Context

The shipped artefact is a Docker image. `tools/`, `AGENTS.md` and
`.claude/skills/` are in the development checkout and in nothing that ships, so
whoever pulls the image has no converter for any institution's export and no
document describing one. What the image does publish is a route that takes
already-converted rows, and it cannot tell whether a tested converter or an
improvisation produced them.

`docs/import-boundary.md` §6 says why that arrangement was tolerated: the
observation channel used to be strictly poorer than the conclusive one, so an
honest caller produced a poorer journal than a converter that concluded. Decisions
0006 and 0013 closed the vocabulary gaps that made this true. What is left
standing after them is not a vocabulary problem at all — it is that **there is one
rule and no implementation of it inside the product**, and every user is invited
to write his own.

Three things go wrong when the converter lives outside, and none of them is
privacy.

**One rule, several implementations.** `tools/README.md` fixes that an importer's
rules have exactly one copy, because two copies drift silently and the first sign
is an import that files the wrong operations. Shipping no converter guarantees
one copy per user rather than one copy.

**The row loses its identity on the way.** `csv_source::parse` stamps a row that
named no key with a key derived from the document digest and the row's own
locator, and its doc comment says why: rows in a file are ordered and located, and
a caller holding only the parsed operations can no longer see either. Decision
0017 §2 then refused to derive such a key inside a session, on the ground that the
"document" a session knows is a name a caller typed rather than a digest of
anything. Both are right, and together they say something about the converter
arrangement: **conversion outside the server destroys the only identity the rows
had, and nothing downstream can restore it.**

**The document is not kept.** `upload_report` stores the bytes before the rows
become facts, precisely so a failed or corrected parse has something to try again
from. Rows converted on a laptop and posted as JSON leave the server holding no
document, so a parser fix has nothing to re-read.

The owner has decided the direction: **one engine, in the core; the source
formats are small plugins shipped per institution and per document type.** This
decision fixes what such a plugin is allowed to say, and — the load-bearing half —
what it is structurally unable to say.

## Decision

### 1. The plugin is a profile, it is data, and the engine reads the document

A profile is one JSON file validated against
`crates/iaam-ingest/schema/source-profile-v1.json`. It is not a dynamic library,
not a script, not an embedded expression language, and nothing loads it into a
process that then evaluates it. It is loaded into the process that writes the
owner's journal, and nothing loaded there may be arbitrary.

The engine takes the document's bytes and a profile and produces **observations** —
`iaam_ingest::observation::ObservedRow`, the row as its source stated it — which
it feeds to an import session. It does not produce operations, and it cannot: an
`ObservedRow` has no `OperationKind`, no `Classification`, no category and no
amount arithmetic. Everything a row *turns out to be* is settled after the engine
has finished, by the owner's directory, by one of his classification rules, or by
his answer to a question. That is where constraint "a plugin describes, the engine
decides" stops being a rule anybody has to remember: the type the engine emits
cannot carry a conclusion, so a profile has nothing to reach for.

Two things follow at once, and both are recoveries rather than new features.

- **The derived row key becomes honest again.** The engine holds the bytes, so it
  has a true document digest and a true line number for every row — the pair
  decision 0017 called sound and could not use, because a session holding only
  rows has neither. The key is derived from the document digest and the row's
  locator **and nothing else**: not the profile, not its version, not the session.
  A content digest remains forbidden for ADR 0017's reason — it merges two genuine
  identical payments and loses a movement that really happened.
- **The document is kept**, as `upload_report` keeps a broker report, so a
  document can be read again under a later profile version without anybody holding
  the file a second time.

### 2. What a profile says

One column-to-cell mapping and one vocabulary translation, per document type. The
schema is authoritative; this is the shape of it.

| The profile names | The engine does |
|---|---|
| how the bytes become a table: format, encoding, delimiter, sheet, header row | reads them, refusing a document whose header row lacks a named column |
| which header cells identify this document | matches a document to at most one profile, and refuses one that two profiles recognise |
| whose statement it is: a column, or the caller's declaration | resolves the printed identity through decision 0004 and 0010's tiering |
| the date column or columns, and the named format of each | parses strictly, or rejects the row |
| the amount: one signed column, or a debit and a credit column | transcribes the sum with the source's own sign, refusing zero and refusing more precision than the currency's minor unit |
| the currency: fixed for the document, or a column with spellings | validates the code against the currencies it knows |
| direction: the amount's sign, or a column with a total map of the source's words | records one of `in`, `out`, `inner`, `unknown`, or rejects the row |
| far side: a column with a total map, where a source asserts it in words | records `own_account` or `unstated` |
| the counterparty, description, the source's own operation word, the source's own category, the source's own row identifier | transcribes each verbatim into the field that field belongs in |

Three properties of the vocabulary matter more than its contents.

**The token maps are total and have no catch-all.** A word the map does not carry
rejects the row and names the word. Mapping it to `unknown` was refused:
`unknown` asserts that the source *said nothing* about direction, and here the
source said something the profile could not read. Recording the second as the
first is a false transcription — the same refusal `ObservedDirection::parse`
already makes of a caller who means "out" and types "outgoing". There is no
catch-all and none can be smuggled in as a key: a key is matched against a cell
exactly, after trimming, and the engine has no wildcard — so `"*"` is a key that
matches a cell printing an asterisk and nothing else.

**A named date format is not a pattern.** A `strptime`-style pattern is a small
program; its acceptance set cannot be reviewed by looking at it, and `%m/%d`
against `%d/%m` is indistinguishable on the first twelve days of every month, so
being wrong produces a wrong date rather than a rejected one. Formats are
therefore names for acceptance sets the engine fixes, and a source that needs a
new one needs an engine release. That cost is the point: it is what keeps the
acceptance set reviewable.

**A profile names a column, never a value of the owner's.** There is no key that
takes an account, a counterparty or a category of his. A profile is a shipped
artefact; a file that carried any of those would be the operator's data inside the
product, and decision 0005 already retired the account map for the neighbouring
reason.

### 3. What a profile cannot express, stated in the schema and not only here

The schema carries three review invariants at its root, and they are what make
"a plugin cannot widen what the engine accepts" checkable in an afternoon rather
than argued about.

1. **Closure.** Every object describing a profile's structure sets
   `"additionalProperties": false`. The single exception is a token map, where
   `additionalProperties` is the *value* schema and `propertyNames` closes the
   keys; each such site is marked. A widening would have to be a key, and there is
   no key to write. An unknown key is refused rather than ignored, because an
   ignored key is a rule its author believed was in force.
2. **Three leaf kinds and no fourth.** Every leaf is a **locator** (a column
   heading, a sheet name, a header row number), a **token** (a word from a closed
   vocabulary of iaam's own words), or a **literal** (text the source prints,
   admissible only as a map key or a required header cell). There is no number the
   engine computes with, no regular expression, no format pattern, no predicate. A
   widening is necessarily a statement about the engine's behaviour, and none of
   the three kinds can name any behaviour at all.
3. **No leniency vocabulary.** The schema contains no key meaning accept,
   tolerate, ignore, skip, default, fallback, on-error, round or coerce. There is
   no way to say what should happen to a cell the engine cannot read, because the
   engine has one answer and it is not a profile's to change.

Concretely, and because these are the four an author will look for: a profile
cannot say that amounts may be floats, that a number with more precision than the
minor unit is rounded rather than refused, that an unparseable date is acceptable,
or that an unknown currency passes. None has a key, and the leaf kinds give no way
to smuggle one into a value.

That claim was checked rather than asserted, and the check is the implementation
bead's first test list. Each of these is a profile a well-meaning author would
write, and the schema refuses every one: a rounding flag; a `strptime` pattern in
place of a format name; `allow_unknown` beside a currency column; an
`on_unknown_token` arm on a direction map; an account map under `account`; a group
separator equal to the decimal separator; a category map under `source_category`;
an `extract` expression on the counterparty; a count of trailing lines to ignore
on the document; a row block with no date at all; version zero; and a file
claiming a schema version the loader does not implement. Freezing them as tests is
what keeps the third invariant true after somebody adds a fourth date format.

### 4. Every cell is validated, and one bad row is one bad row

A row that cannot be read is refused with `Rejection { field, expected, actual }`
— the shape the CSV path and the report path already produce — and the remaining
rows of the document are read (§10.1). No cell is guessed at, and a cell the
profile did not name is not read at all.

This is why there is no key for "the last two lines are totals". A trailing
totals line is read as a row, fails to be one, and is rejected by name. A count of
lines to drop is a claim that is true of one export and false of the next, and
when it is false it discards real movements in silence — while a rejected row
discards nothing and is visible.

### 5. A profile is a `ParserVersion`, and an upgrade rewrites nothing

Every fact a profile produced carries `ParserVersion("profile/<id>/<version>")`.
The `profile/` prefix is reserved and no existing version uses it — the versions
in the tree are `tinkoff-xlsx/1`, `finam-xls/2`, `tinkoff-api/4`,
`ingest/manual/1` and their neighbours — so the origin of a fact is readable
from the first segment alone.

**Two things must change before that sentence is true, and neither is cosmetic.
The first is that nothing records the reader today.**
`iaam_ingest::operation::normalize` stamps
`ParserVersion("ingest/manual/1")` on every event it builds, and only the
document channel replaces it afterwards. Every row committed out of an import
session therefore records `ingest/manual/1` today, whatever read it — so the
session path, which is the path a profile-read document takes, currently loses
exactly the field constraint 6 depends on. The decision is that a row records
what read it: `NormalizationContext` gains the parser version beside the owner
and the source, `ingest/manual/1` stays the value for a row a caller submitted
itself, and the rows of a batch the engine produced carry the profile's. The
version belongs to the batch and not to the session, because a session is opened
per declaration and a declaration does not name a reader.

**The second is that a version must name a content.** An instance records the
digest of each profile it loads and refuses to load a different content under an
`(id, version)` it already recorded. Without that, "the rows version 3 read" is
not a set, and constraint 6 — the facts a buggy plugin wrote are findable and
retractable — is not true.

The digest is recorded beside the profile rather than folded into the
`ParserVersion` string, and that is a trade worth naming.
`SettlementLagPolicy::with_profile` keys a proven settlement band on a
`ParserVersion`; a digest inside the string would demand a new band entry for
every byte changed, including changes that touch no date. Recording the binding at
load time is enough, because retraction is per journal and a journal can only
retract what it recorded.

**Nothing is rewritten when a profile is upgraded.** The journal is append-only
and a `ParserVersion` change is visible in provenance. What the reader is owed is
therefore a statement, not a repair:

- every fact says which profile and which version read it, so the rows a version
  produced are a query rather than an archaeology;
- the instance publishes its profile catalogue — each profile's id, version,
  digest, origin, and the reason any profile was refused — so "my journal contains
  rows from two versions of one profile" is answerable without opening the
  journal;
- the document bytes are kept, so the remedy for a fixed profile is the ordinary
  pair: retract the import through the import-correction channel, then read the
  stored document again under the new version.

The order matters and follows from §1. Because the derived row key is over the
document and the locator alone, re-reading the same document under a new profile
version yields **the same keys**, so the second import is answered `duplicate` and
appends nothing until the first is retracted. That is deliberate. The alternative
— putting the profile version into the key, as `sync.rs` puts it into a control
assertion's idempotency key — would let both imports stand at once and double a
month of movements while the owner reads a green response.

### 6. A source's own category is transcribed and never mapped

`Provenance::source_category` already says what it is for: a bank calling a
subscription by some word is a hint the owner may map or override, and storing it
as the owner's own category would let the bank decide what his spending was.

A profile therefore names the column and stops. There is no map from a source's
category to one of the owner's, and there will not be. His category rules already
do that job; they are his, they are editable, and they can be re-run over rows
already recorded — while a mapping baked into a profile is frozen into every fact
at the moment of import and correctable only by retracting the import.

The same applies one field over: the source's own operation word goes to
`source_kind` verbatim, **beside** the direction the profile read out of it rather
than instead of it, so a wrong entry in a direction map is visible against the word
it was made from.

### 7. The engine must separate two fields the observation path currently conflates

There is nowhere for a source's category to go today, and this decision cannot be
implemented without fixing that. `ObservedRow` has `source_kind` and no
`source_category`, and `ObservedRow::envelope` fills the operation's
`source_category` from `source_kind`; `scenarios/classification.rs` reads
`Provenance::source_category` back out as `source_kind`. The pair round-trips
consistently, so nothing fails — but `CategoryMatcher::SourceCategory` matches on
`Provenance::source_category`, which for every observation holds the source's
*operation word*. A category rule the owner writes on a category never matches an
observed row, and one he writes on an operation word matches rows he was not
describing.

The decision is that the two are separate fields end to end: `ObservedRow` gains
`source_category`, `envelope` carries each to its own field, and the read-back in
`scenarios/classification.rs` is corrected in the same change, because fixing
either half alone breaks the round trip. Facts already recorded are not rewritten;
what they carry is what the observation path meant at the time.

### 8. Where a profile comes from

Two origins, one validator, one trust rule.

- **Bundled.** Profiles live in the repository under `crates/iaam-ingest/profiles/`
  and are copied into the image. They are reviewed like code, covered by fixtures
  invented end to end, and their integrity is the image's.
- **Local.** The operator may point the instance at a read-only directory of his
  own, by an `IAAM_`-prefixed variable with no default, as every other path in this
  program is supplied. It is how a profile for an institution nobody has shipped
  yet can be used without waiting for a release, and it is safe to allow precisely
  because a profile is data.

A local profile whose id collides with a bundled one does not shadow it: it is
refused, and the catalogue publishes it as refused with the reason. Silence would
mean an export read by a profile nobody chose.

**A profile is accepted whole or not at all.** The unit of that rule is one
profile, not the catalogue: one unreadable file must not take the instance's other
formats down with it. What keeps the failure from being silent is that the
catalogue names every refused profile and why — a profile that is merely absent
looks exactly like one that was never written.

**Integrity is the digest and the version binding, not a signature.** A signature
the owner makes with a key on the same host proves nothing about the file that the
file's presence does not; there is no key infrastructure here, and inventing one
would put a trust root beside the one ADR 0003 already established for the console.
What integrity has to buy is the ability to say which bytes wrote a fact, and §5
buys exactly that.

## What was rejected

**A dynamic library, a WASM module, or an embedded interpreter.** Rejected by the
owner before the design started, and the reason is worth writing down rather than
cited: the thing being loaded goes into the process that appends to the owner's
journal, and it would be loaded from a directory whose contents are, by
construction, not reviewed by this repository. A parser expressive enough to be
useful is expressive enough to write a fact nobody asserted.

**A profile that computes.** A per-row expression — a formula for the amount, a
conditional for the kind — was never seriously available:
`scripts/check-architecture.sh` already forbids monetary arithmetic outside the
core, and a plugin that could compute would be a second implementation of the
rules, which is the defect class this repository keeps paying for.

**A regular expression to cut a counterparty out of a purpose line.** This is the
most tempting feature in the whole design and the most damaging. It is an
expression language by another name, it is where the profile becomes code, and its
failure mode is silent: a group that matches the wrong half writes a counterparty
that then feeds classification rules, transfer pairing and the money-flow report.
The alternative already exists and is better: the purpose line is transcribed to
`description`, and the owner's own description rules cut it — a rule he can see,
correct, and re-run, against a fact that still carries the source's whole text.

**A category map from the source's category to the owner's.** §6.

**A default arm in a token map.** §2. It is the shape a widening would take, and
it is the one an author reaches for when a bank adds an operation type.

**A `strptime`-style date pattern.** §2.

**A leniency flag of any kind** — accept unpadded components, tolerate a stray
thousands separator, round to the minor unit. Each is a profile changing what the
engine accepts, which is the line. Where a source genuinely prints a different
shape, that is a different named format, added to the engine and released.

**A count of trailing lines to ignore.** §4.

**Several columns joined into one description.** The joined string is what every
description rule the owner ever writes will match, so the join order becomes a
silent part of every one of them.

**The profile version inside the derived row key.** §5. It looks like the careful
choice and is the dangerous one.

**A content digest as the row key.** Forbidden by ADR 0017 and `iaam-1k9t`: it
merges two genuine identical payments and loses a movement that happened.

**A timezone or offset in the profile.** Converting a printed timestamp changes
which day a row falls on and therefore which month a sum lands in. If a source's
timestamps must be shifted, that is an engine decision with an ADR of its own.

**An install route that stores profiles in the owner's database.** Rejected
because the format catalogue is a property of the deployment, not of the journal.
Two instances of one image must read one institution's export the same way, and a
per-journal catalogue makes an export's reading depend on who uploaded what and
when. It would also need an authorisation class of its own — a profile decides how
every future row of that format is read, which is a stronger power than any write
route currently grants. The condition to reconsider is stated under "what this
does not settle".

**Teaching the server one institution's format in Rust**, which
`docs/import-boundary.md` §8 rejected and which this decision does not overturn.
The objection there was that every institution is another format and every format
another release of the server. A profile is exactly the answer to that objection:
the *format* is data, and only the engine is released.

## Consequences

- **The agent can drive a whole import without ever holding a row.** The owner
  uploads his document to his own instance; the agent opens the session, reads the
  assessment, relays the questions and commits. `docs/import-boundary.md` §4 is
  unchanged and, for the first time, is not also a handicap.
- **A wrong profile is a findable set of facts.** `profile/<id>/<version>` is in
  every row's provenance, the binding of that pair to a content is enforced at
  load, and the retraction channel already exists.
- **Adding a bank costs a reviewed JSON file; adding a date format, an encoding or
  a delimiter costs a release.** That asymmetry is the design working. A profile
  author can be wrong about *where* a value is and the engine still validates what
  it finds; he cannot be wrong about what counts as a valid value, because he
  cannot say.
- **A bank that adds an operation type breaks the affected rows, loudly.** They
  are rejected by name, the other rows import, and the remedy is one line in a map
  and a version bump. This is the deliberate cost of refusing a catch-all.
- **`POST /v1/ingest/csv` is not what this replaces.** Its columns are iaam's own
  and a bank export still rejects on every row. What the profile channel removes is
  the *reason* an export kept arriving there.
- **Neither in-tree report parser is touched.** Schema version 1 describes a cash
  statement: a table of movements. A broker report has control sections, trades,
  instruments and places of custody, and `ReportParser` stays where it is.

## What this does not settle

- **Which route carries a profile-read document.** The engine's output is an
  import session and not appended facts, because a cash statement needs
  classification and the report path has none — that much is decided here. Whether
  it is reached by extending the existing document channel or by a route beside it
  is the contract bead's, under `docs/api/conventions.md`, with one constraint from
  §1: the bytes must be kept the way a report's bytes are kept, or a re-read after
  an upgrade is impossible.
- **A document that names its account in a preamble cell rather than a column.**
  Common in spreadsheet exports and not expressible in schema version 1, which
  would need a fourth locator kind — a cell coordinate outside the table. Left out
  deliberately rather than forgotten; the caller's declaration covers the case
  today.
- **A column carrying the far account's own identifier.** `ObservedRow` has no
  field for it, only the printed counterparty string, which the directory resolves.
  Adding one is a change to the observation channel's published shape and belongs
  with the parity work, not here.
- **Whether a profile may be installed through the API.** Rejected above on the
  ground that the catalogue belongs to the deployment. The condition to revisit is
  an operator who cannot mount a directory at all; the answer then is a route that
  writes to the deployment's profile store and is gated as strongly as ownership
  itself, not a table in the journal.
- **Whether a profile can describe a securities statement.** Schema version 1 does
  not, and a version 2 that did would have to answer what a control section is
  before it could be trusted with one.
- **The questionnaire's cost**, which `docs/import-boundary.md` §6 leaves open. A
  profile improves the *evidence* a row carries — the source's word, its category,
  its description, its counterparty, its far-side claim — and improves nothing
  about how many questions an unmatched row raises. That is decision 0008's
  territory and it is untouched here.
