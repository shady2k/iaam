# A place of custody follows the broker

Beads: `iaam-klpv` (P0), `iaam-kks6` (P2), `iaam-no3w` (P1), `iaam-pzxl` (P1).
Brainstorming: `iaam-iau1`. Epic: `iaam-wwgv`.
Deliberately deferred behind `iaam-xep0`: the document channel's own places.

**Revision, 2026-09-09.** An earlier draft of this spec proposed one wave:
a route creating places, a place derived from an account's institution, and a
per-account default for a document row. An adversarial review found that those
three create a **second custody namespace** alongside the one broker channels
already mint, and that the consequence is not a display split but silent
double counting — §8 records what was found and why the three are now deferred.
This document is the revised design, not the original with a note.

## 1. What is broken

The relational journal made `custody_places` a real reference table. Four
columns point at it — `event_legs.custody`, `event_control_assertion.custody`,
`event_corporate_action.custody` (`NOT NULL`) and `event_offer_exercise.custody`
(`migrations/0001_schema.sql:1176, 1335, 1400, 1480`) — and the single write
primitive refuses a leg whose place is not registered to the event's owner
(`journal/write.rs:103-136`).

**No line of production code ever writes a row into that table.**
`upsert_custody_place` exists (`store/src/reference.rs:774`) and is called only
by fixtures; `InstrumentDirectory` exposes `list_custody_places` and nothing
that creates one (`app/src/ports.rs:593`); the server wires only the read
(`server/src/routes.rs:7393`).

So in the current tree, in production, none of the following can be recorded:
a trade with a security leg, a portfolio quantity claim for reconciliation, a
corporate action, an exercised offer. Before the relational journal the journal
was an opaque blob with no keys and nothing noticed.

**And a report's position rows do not merely fail — they disappear.** Both
report readers spell the custody lookup `let Ok(custody) = … else { continue; }`
(`report/tinkoff.rs:458`, `report/finam.rs:455`): no rejection, no row verdict,
no assertion. With an empty directory every opening and closing balance in every
broker report vanishes with nothing saying it was there. The shape is not about
custody — it swallows an unrecognised instrument the same way. This is
`iaam-pzxl`, and it is the most urgent thing in this document.

## 2. The decision

**The owner chooses a broker. A place of custody follows from it, and he is
never asked for one.**

He has no information about a depository and does not need any: every Russian
broker settles into the same one, and what actually has to be distinguished is
not the depository but the section a given broker keeps. The same security held
through two brokers is two positions, reconciled separately against two
statements, however identical their legal custodian. A foreign broker, when one
appears, arrives by the same route and needs no new concept.

**But that principle cannot be carried out until `CustodyId` means one thing,**
and today it means two: the report channel resolves a real custodian by the
owner's title for it, and the API channel puts the broker's `positionUid` there
— a property of the *instrument*, not of any place. §8 is why that ordering is
forced rather than chosen.

So this design does two separable things. It **restores what the relational
journal broke**, without creating any custody identity that did not exist
before. And it **makes an archive restorable**. It does not reopen the document
channel's securities path; that waits for `iaam-xep0`.

## 3. Two origins, and only one is a place

`custody_places` gains one column:

```sql
origin TEXT NOT NULL CHECK (origin IN ('declared', 'minted'))
```

- **`declared`** — a place of the owner's, with a title he would recognise. No
  path creates one yet; the column is declared now because §4 needs the
  distinction and because a table that will hold both should say so from the
  first row.
- **`minted`** — a bare handle a channel produced. The T-Invest channel puts the
  broker's `positionUid` in this field; the Finam parser puts the broker's own
  account identifier there. Neither is a place. The row exists so the foreign
  key holds and for nothing else: it is never offered by title and never shown
  as one of his places.

The column is a discriminator and not a repair list. It makes an affected
custody reference **discoverable**; it does not identify the events to retract,
because it labels custody rows and the events naming them are spread across four
tables. An earlier draft claimed more than that.

A nullable `institution` cannot carry this distinction instead: a legitimate
declared place may have none, and "no institution" would then mean both "not
stated" and "not a place".

**Declared titles are unique per owner:**

```sql
CREATE UNIQUE INDEX custody_places_declared_title
    ON custody_places (owner, title) WHERE origin = 'declared';
```

Without it the schema would admit duplicate titles while `lookup_custody`
refuses an ambiguous one — a state no reader can interpret. Titles are stored
trimmed so that a title and the same title with surrounding space are not two
keys; case is left exact, because the existing lookup is exact and folding it
here would be a silent change to what resolves.

## 4. Broker synchronisation registers the handle it mints

There are **two** append paths in a sync, and this is where an earlier draft was
wrong. The operations loop (`sync.rs:139-207`) appends events built from
`SubmittedOperation`s. The portfolio loop (`sync.rs:288-325`) runs afterwards and
independently appends a legless `ControlAssertion` per claim, carrying
`custody = positionUid`. A position the broker reports but that saw no trade in
the interval has no operation at all, so a registration driven from the accepted
operations alone leaves the second path broken.

Both loops register before their own append. `minted` origin. The value in the
event is not touched: no fingerprint and no reconciliation key moves.

**Not atomic, and that is accepted.** Registration is one call and the append
another. The ordering means the only partial state reachable is an unreferenced
`minted` row — reference data no reader consumes, invisible to balances and to
reconciliation, both of which are event-driven. The dangerous ordering, an
append committing while its custody row is absent, cannot occur. A combined
store primitive would buy nothing for this.

**The journal-events route deliberately does not register.**
`POST /v1/ingest/journal-events` carries corporate actions and offer exercises
whose custody the caller supplies, and it shares `ingest::append_checked` with
the sync. Registering there would turn a refusal into an acceptance and let an
agent bring a place into being by naming one.

## 5. The owner check covers every custody a fact names

`ensure_legs_are_owned` (`journal/write.rs:103`) walks `event.legs`, and its own
doc comment says why the check exists in code at all: the child tables carry no
`owner` column, so "this check exists in code or it does not exist at all".

**A control assertion has no legs**, and its custody sits in a detail row. So
does a partial redemption's, which produces only a principal leg. The foreign
keys on those columns are global — `REFERENCES custody_places (id)`, no owner —
so an event of one owner naming another owner's place is accepted today.

Nothing can write those events at the moment, because nothing registers a place.
§4 makes them writable again, which is exactly what turns this from latent into
live, so it is repaired here rather than filed for later (`iaam-no3w`).

The check moves to one exhaustive domain traversal —
`Event::referenced_custodies()` — collecting from security legs,
`ControlClaim::PositionQuantity`, every `CorporateAction` variant and
`OfferExerciseAction::Settled`, with no `..` patterns that would let a new field
pass unnoticed. `insert_event_in` performs one ownership check over it.

The invariant is about **accounts and custody places only**. Instruments are
global and have no owner; an ownership check over them would be meaningless.

A schema guard pairs with it: every `(event table, custody column)` foreign key
declared in `0001_schema.sql` must appear in an explicit coverage set. Exact
pairs, never substring matching — a substring guard passes vacuously the moment
a second such column is added to a table the query already names.

## 6. A refusal stops promising a capability that does not exist

Two changes, and neither reopens the path.

`resolve_custody`'s refusal says *"a place of custody of the owner's, or a
default one for this account"* (`csv_source.rs:697`). There has never been a
default, and there is no way for him to register a place. A refusal describing a
capability the system does not have is worse than one that admits the gap: it
says what the caller must do, and the caller then cannot do it. It is reworded
to name the real state — a custody title already registered for this owner, and
no API in this build that registers one. **No bead identifier goes into a wire
response**; the API description carries the limitation.

And `iaam-pzxl`: the report readers' position rows stop being dropped. Each
unresolved row becomes a located rejection carried in the parsed report. This
stays row-level — the document's valid cash rows still import, and a
document-level refusal would be a worse regression than the one being fixed.

## 7. The bundle carries what a restore into an empty database needs

`iaam-kks6`, and independent of everything above.

The design's §4.1 justified leaving `import_session` and `settled_by_rule`
unkeyed because "`Bundle` carries events, accounts and contours and nothing
else" — while `event_legs.custody` and `event_legs.instrument` are keyed, so a
bundle holding one security leg restores into nothing. The round-trip test
currently registers reference data into both stores by hand to get past it
(`store/tests/bundle.rs:320-373, 522-553`).

**An archive is needed exactly when the outside world is not available** — the
disk is gone, the broker is gone, the quote vendor is gone. An archive that
needs a market sync before it will restore is not an archive. So it carries both
sections.

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub custody_places: Vec<CustodyPlaceSection>,
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub instruments: Vec<InstrumentSection>,
```

**The attributes go on `Bundle` and on `BundleContent` both, and this is the
trap.** `BundleContent` is a separate type assembled by hand inside
`compute_checksum` (`bundle.rs:60-98`). `#[serde(default)]` on `Bundle` alone
fixes reading an old archive and leaves the checksum wrong;
`skip_serializing_if` on `BundleContent` alone fixes the checksum and leaves old
archives undeserialisable. ciborium writes a struct as a definite-length,
named-key map, and serde-derive computes that length after skipping — so a
skipped field emits no bytes, shifts no neighbour, and does not move the map
header. An archive exported before this change hashes byte-for-byte as it did.

`BUNDLE_VERSION` goes to 2, and that does not invalidate an old archive:
`compute_checksum` hashes `self.bundle_version`, so an archive carrying 1
recomputes over 1. The module comment already gives the reason for the bump — a
reader that silently skips a section it does not know loses data.

**What travels:**

- Custody places: every `declared` row, and only those `minted` rows reachable
  from the exported events. A minted row is as load bearing for the foreign key
  as a declared one, and scoping it to reachability also keeps a failed sync's
  unreferenced handles out of the archive.
- Instruments: the closure reachable from the exported events, plus
  `lineage_parent` transitively. Aliases do **not** travel: nothing in the
  journal's foreign keys reaches `instrument_aliases`, because the bundle
  carries resolved `InstrumentId`s rather than codes.

  The closure is taken rather than the whole directory because a market sync
  fills that directory with an exchange's entire universe, and an archive of one
  owner's affairs has no business carrying it.

  **A closure query is a list, and a list goes stale**, so it is guarded by the
  same exact-pair mechanism §5 uses: every `event*` column declared
  `REFERENCES instruments` in `0001_schema.sql` must appear in an explicit set
  the query is built from. Ten such columns exist across eight tables today —
  `event_corporate_action` carries three — and all eight tables arrived in one
  change.

**Import order** is a hard requirement, not a nicety: `PRAGMA foreign_keys` is
on (`store/src/lib.rs:239`) and no constraint in the schema is deferred, so
SQLite checks at the offending statement. Custody places and instruments insert
before the events loop, and an instrument whose `lineage_parent` is itself in
the section inserts after its parent.

**Import policy differs by ownership, deliberately.** Custody places are the
owner's, so they mirror the accounts insert already in `import_bundle` —
`ON CONFLICT DO UPDATE … WHERE custody_places.owner = excluded.owner`.
Instruments are global reference data shared with whatever else the target
database holds, so they are `ON CONFLICT DO NOTHING`: **an archive supplies what
is missing and never overwrites what is there.**

## 8. What is deferred, and the argument that forced it

Three things an earlier draft placed here are deferred behind `iaam-xep0`:
a route that creates a place, a place derived from an account's institution, and
a per-account default for a document row.

**Each of them mints a custody identity the channels do not share, and that is
not a display split.** A position is keyed `(account, custody, instrument)`
(`projection/balances.rs:18`), but `Balances::quantity_of` deliberately **sums
across custody** because lots do not distinguish it (`balances.rs:110`). And
`custody` is a field of `OperationKind`, serialised whole into the deduplication
fingerprint (`ingest/dedup.rs:280`). Put together: one economic trade arriving
from a document under a declared place and from the API under a minted handle
does not dedupe, and its quantity is counted twice — while the lots invariant
grows on both sides and does not notice.

Worse, evidence is awarded per **dimension** rather than per subject.
`ground_three` intersects `Dimension` sets (`reconciliation/mod.rs:654`) and
`confirmed_dimensions` discards the instrument and custody a claim was about
(`:689`). Two channels can each reconcile against its own custody silo and
together produce `BrokerApiAgreesWithStatement` without having agreed on a
single position.

A history-dependent guard was considered and rejected: it is order-dependent,
wrong for a legitimate transfer of securities between depositories, racy outside
the append transaction, and defines nothing — it would detect the modelling
mistake rather than remove it.

A narrower route — letting the owner attach his title to an *existing* minted
handle, creating no second identity — was considered and rejected too.
`positionUid` is per instrument while a report's title names a section, so one
title would name a dozen handles and `lookup_custody` would refuse it as
ambiguous; and for a broker with no API channel there is no handle to name at
all.

**The cost, stated plainly because it is the owner's to accept.** Until
`iaam-xep0` is settled, securities cannot enter from a broker **report**. Only
the wired API channel can record them, and the only wired channel is T-Invest
(`adapters/sqlite.rs:1691`). For every other broker the report file is the sole
route and it stays closed. This is a regression against the pre-relational
journal, where the blob store checked nothing and the path worked. §6 makes it
an honest refusal rather than a silent one; it does not make it work.

## 9. Testing

1. A custody place round-trips its origin, and a third origin value cannot be
   written.
2. Two declared places cannot share a title; a minted one is unconstrained.
3. A store holding no custody place synchronises a broker trade **and** a
   portfolio position claim — a position with no trade in the interval included.
4. A minted row is absent from the document directory's title map.
5. A control assertion, a corporate action and an offer exercise naming another
   owner's place are each refused, in the vocabulary the leg-level refusal uses.
6. The coverage guard fails when a custody or instrument column is added to an
   `event*` table without being added to the traversal or the export set.
7. A bundle exported from a store holding a security event restores into a
   genuinely empty store with no pre-seeding.
8. An archive produced by the version-1 serialiser still deserialises and still
   verifies. The fixture is produced by that serialiser or reproduces its
   content and checksum exactly — **not** by exporting from the new code and
   deleting keys, which yields `bundle_version: 2`.
9. Changing one custody place's title in an exported bundle breaks the checksum.
10. With an empty custody directory, a broker report's position rows are
    reported as refused with their coordinates, and the document's valid cash
    rows still import.

## 10. Order of work

**Wave 1** is the P0: §3, §4, §5, §6. It restores what the relational journal
broke and creates no custody identity that did not exist before.
**Wave 2** is §7, independent of wave 1 and touching only `bundle.rs`, the
schema section and the store tests.
**Wave 3** is `iaam-xep0` and, after it, §8's three deferred pieces.
