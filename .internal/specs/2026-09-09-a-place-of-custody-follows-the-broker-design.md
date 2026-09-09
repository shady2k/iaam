# A place of custody follows the broker

Beads: `iaam-klpv` (P0), `iaam-kks6` (P2). Brainstorming: `iaam-iau1`.
Related and deliberately not resolved here: `iaam-xep0`, `iaam-zhej`.

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

So in the current tree, in production, none of the following can be recorded at
all: a trade with a security leg, a portfolio quantity claim for reconciliation,
a corporate action, an exercised offer. Before the relational journal the
journal was an opaque blob with no keys and nothing noticed.

The document channel is dead from the other end too. `build_directory` fills
`Directory.custodies` from a list that is always empty and never sets
`default_custody`, which stays `None` from `Directory::default()`. So
`resolve_custody` (`ingest/src/csv_source.rs:697`) refuses every security row:
by title, because no title is registered; by default, because there is none.
This is the blocker `iaam-zhej` records — the contract harness cannot provision
a custody place, so the refusal it wants to pin cannot be reached.

## 2. The decision

**The owner chooses a broker. The place of custody follows from it, and he is
never asked for one.**

He has no information about a depository and does not need any: every Russian
broker settles into the same one, and what actually has to be distinguished is
not the depository but the section a given broker keeps. The same security held
through two brokers is two positions, reconciled separately against two
statements, however identical their legal custodian. A foreign broker, when one
appears, arrives by the same route and needs no new concept.

Three things follow, and the third is what keeps the first two honest.

## 3. A place can be created through a port, and usually is not

`InstrumentDirectory` gains a write:

```rust
/// Create or rename a place of custody.
///
/// The identifier is assigned by the caller, upsert by identifier, owner-scoped
/// exactly as `upsert_account` is: a request carrying someone else's identifier
/// changes nothing (§14).
async fn record_custody_place(
    &self,
    owner: OwnerId,
    place: CustodyUpsert,
) -> Result<CustodyId, AppError>;
```

`SqliteAdapter` delegates to the existing `upsert_custody_place`, whose
`WHERE custody_places.owner = excluded.owner` guard is already the right one.
Routes: `POST /v1/custody-places` and `GET /v1/custody-places`, under the same
administration floor as account creation, described in OpenAPI beside it.

**This is the fallback path, not the ordinary one.** The ordinary one is §4.
Every test that today seeds the raw `SqliteStore` before wrapping it in
`SqliteAdapter` — the helpers in `app/tests/sync.rs:91-101`,
`app/tests/schedule_metamorphic.rs:126`, `server/tests/contract.rs` and the rest
of the list in `iaam-klpv` — goes through the port instead. A test reaching
around a port is how a missing port stays missing for a year.

## 4. A place is derived from the account's institution

An account already carries the institution that keeps it
(`AccountView.institution`). When an account is created or its institution is
declared, the owner's place for that institution is ensured: one place per
`(owner, institution)`, titled with the institution's name, created if it is not
there.

The owner types nothing. His list of places is his list of brokers. A second
broker produces a second place the first time an account names it, which is the
"foreign broker some day" case answered without a feature for it.

An account with no declared institution derives no place, and a security row on
such an account is refused in the ordinary way, naming what is missing.

## 5. Two origins, and only one is offered by name

`custody_places` gains one column:

```sql
origin TEXT NOT NULL CHECK (origin IN ('declared', 'minted'))
```

- **`declared`** — a place of the owner's: created through §3, or derived
  through §4. It has a title he would recognise, and the document directory
  offers it by that title.
- **`minted`** — a bare handle a channel produced. The T-Invest channel puts the
  broker's `positionUid` in this field (`tinkoff/parse.rs:255`); the Finam
  channel puts the broker's own account identifier there (`finam/parse.rs:130`).
  Neither is a place. The row exists so that the foreign key holds and nothing
  else: it is never offered to a document reader, never matched by title, and
  never shown as one of his places.

  Neither collides with the T4 repair. Its predicate is
  `leg.custody == Some(CustodyId(account.inner()))` (`scenarios/sync.rs:449`) —
  **iaam's** account identifier, not the broker's — so registering a minted row
  does not make a new event look like one the repair should retract.

`build_directory` builds `Directory.custodies` from `declared` places only. A
minted row therefore cannot collide with a real title, which matters: two ids
under one title make `lookup_custody` refuse for ambiguity, and a channel that
minted a hundred handles would break resolution for every row.

**Broker synchronisation registers the place before it appends the event.**
Same transaction boundary as the append, `minted` origin, title recording which
channel produced it. This is what puts the security path back into service, and
it changes no value the journal already carries.

## 6. A report row without a named custodian uses the account's place

`Directory.default_custody: Option<CustodyId>` becomes a map keyed by account,
and `resolve_custody` takes the row's account — which `row_to_operation` has
already resolved at `csv_source.rs:605`, one line before `build_kind`, so this
is threading rather than reordering.

The refusal wording already promises this: *"a place of custody of the owner's,
or a default one for this account"*. It has been describing a default that never
existed.

`build_directory` fills the map from §4's derived places, per account. A row
that names a custodian still resolves by title and is unaffected.

## 7. The bundle carries what a restore into an empty database needs

`iaam-kks6`. The design's §4.1 justified leaving `import_session` and
`settled_by_rule` unkeyed because "`Bundle` carries events, accounts and
contours and nothing else" — while `event_legs.custody` and
`event_legs.instrument` are keyed, so a bundle holding one security leg restores
into nothing. The round-trip test currently registers reference data into both
stores by hand to get past it (`store/tests/bundle.rs:320-373, 522-553`).

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
archives undeserialisable. ciborium writes a struct as a named-key map in
declaration order, and a skipped field emits no bytes and does not shift its
neighbours — so an archive exported before this change hashes byte-for-byte as
it did, and verifies.

`BUNDLE_VERSION` goes to 2. The module comment already gives the reason: a
reader that silently skips a section it does not know loses data, so an old
build must refuse a new archive rather than restore a partial one.

**What travels:**

- Custody places: all of the owner's, both origins. A minted row is as load
  bearing for the foreign key as a declared one.
- Instruments: the closure reachable from the exported events, plus
  `lineage_parent` transitively — one recursive query. Aliases do **not**
  travel: nothing in the journal's foreign keys reaches `instrument_aliases`,
  because the bundle carries resolved `InstrumentId`s rather than codes.

**Import order** is a hard requirement, not a nicety: `PRAGMA foreign_keys` is
on (`store/src/lib.rs:239`) and no constraint in the schema is deferred, so
SQLite checks at the offending statement. Custody places and instruments insert
before the events loop.

**Import policy differs by ownership, and deliberately.** Custody places are the
owner's, so they mirror the accounts insert already in `import_bundle` —
`ON CONFLICT DO UPDATE ... WHERE custody_places.owner = excluded.owner`.
Instruments are global reference data shared with whatever else the target
database holds, so they are `ON CONFLICT DO NOTHING`: **an archive supplies what
is missing and never overwrites what is there.** A restore must not silently
rewrite a live directory with a snapshot of an older one.

## 8. What this does not do, and why

**`iaam-xep0` stays open.** `positionUid` identifies a security's position, not
a place, and this design does not correct it — it registers it and hides it.
The reason is a hard one: `custody` is a field of `OperationKind`, which is
serialised whole into the deduplication fingerprint (`ingest/src/dedup.rs:280`).
Changing what the field holds changes the fingerprint of every affected
operation, so a re-import would not recognise the rows it already holds and
would **double the positions** rather than dedupe against them. Correcting the
semantics therefore requires reversing the affected history and re-importing it,
the way the T4 repair did (`scenarios/custody_repair.rs`) — an epic, not a tail.
The `minted` marker is what makes that future repair addressable: it names
exactly the rows to retract.

**A consequence to state rather than discover.** With §6 in place, a holding
imported from a document and the same holding synchronised from the API can
carry different custody values and reconcile as two positions. Today this cannot
happen only because the document path refuses everything. That is the divergence
`iaam-xep0` describes, and it becomes reachable here; the bead is updated with
this evidence and raised, and the limitation is written where a reader of a
position meets it.

**No production code is weakened to satisfy a fixture.** If a test wants a
check relaxed, the check is right and the fixture is wrong.

## 9. Testing

Each of these fails before the change and passes after.

1. **A bundle restores into a genuinely empty store.** `store/tests/bundle.rs`
   stops pre-seeding the restore target; `BondReferenceData::register_in` is
   called on the source only. The current test's own doc comment
   (`bundle.rs:320-331`) is rewritten to state what now holds.
2. **An old archive still verifies.** A fixture bundle serialised without the
   new sections deserialises and passes `compute_checksum`.
3. **A place is created through the port**, and an account's institution derives
   one without any call at all.
4. **A CSV buy row resolves a custody place by the owner's title** through the
   contract harness — the coverage `iaam-zhej` says does not exist, now
   reachable because the harness can provision one.
5. **A report row naming no custodian uses the account's place**, and one on an
   account with no institution is refused naming what is missing.
6. **A minted place is not offered**: it is absent from the document directory
   and from the list route, and a row naming its title is refused.
7. **A fresh store synchronises a trade** with nothing pre-seeded — the sync
   registers the place itself.
8. **A leg naming another owner's place is still refused.** The existing test
   (`journal/write.rs:787`) keeps its meaning under the new column.

## 10. Order of work

`iaam-klpv` is P0 because production cannot record securities. Its parts (§3–§6)
land first and independently of `iaam-kks6` (§7), which touches only
`bundle.rs`, the schema section and the store tests. §5's schema column is the
one shared edit and lands first of all.
