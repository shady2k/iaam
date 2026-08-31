# T4: custody comes from `position_uid`, and old facts are not silently doubled — design

Bead: `iaam-6aun`. Parent epic: `iaam-zn38`. Date: 2026-08-31.

Parent design: `.internal/specs/2026-08-31-tinkoff-execution-facts-design.md`
§8. Preceding tasks: T1 `iaam-woil`, T2 `iaam-dz94`, T3 `iaam-0gbe`, all
closed. Line numbers are against the `t1-order-state` worktree, which
carries all three uncommitted.

Reviewed adversarially by codex on 2026-08-31. Four findings, all
accepted, three of them this document being factually wrong: the parser
version does not identify the defect it was proposed as a marker for, the
already-loaded event vector is bounded by the synced interval and does
not answer the question asked of it, and "reconciliation can never
succeed" was too strong — a zero holding matches either way, which is
worse than a loud failure.

## 1. Problem

A trade fabricates its custody from the account:
`custody: CustodyId(account.inner())`
(`iaam-app/src/adapters/tinkoff.rs:474,485`). The portfolio side of the
same channel does not: it takes `positionUid` and puts it in `custody`
on the claim it raises (`iaam-broker/src/tinkoff/parse.rs:227-238`,
`ControlClaim::PositionQuantity`).

So the two sides of one channel are keyed differently. Observed positions
are keyed by `(InstrumentId, CustodyId)`
(`iaam-core/src/reconciliation/observed.rs:70`), so a position built from
trades sits under one key and the broker's claim asks about another.

**Precisely:** a claim about a **non-zero** holding cannot match, because
the key it asks for holds nothing. A claim about a zero holding still
matches — an absent key reads as zero
(`iaam-core/src/reconciliation/check.rs:138-148`) — so the failure is
invisible on exactly the accounts where nothing is held. That is worse
than a loud failure, not better, and it is why the claim in an earlier
revision of this document that reconciliation "can never succeed" was
both too strong and beside the point.

The channel is the only place that fabricates custody. The report
channel resolves a real custodian from the directory, named or default
(`iaam-ingest/src/csv_source.rs:361-377`); every other `CustodyId(...)`
in non-test code reads a stored value.

## 2. Decision: the trade side adopts the portfolio side's key

`custody` comes from `position_uid = 35` (`operations.proto:496`). A
trade row without a `position_uid` is **refused** — quarantined with a
reason naming the field — rather than falling back to the account.
Falling back would restore the very mismatch this task removes, on
exactly the rows where it is hardest to notice.

`position_uid` is present on every row of the recorded response, trades
included, and is stable per instrument
(`tests/fixtures/api/tinkoff-operations.json`).

**The semantic caveat, stated once.** `position_uid` is not a custodian.
It identifies a position, and using it as `CustodyId` conflates "where
the paper is held" with "which position it belongs to". That conflation
already exists — the portfolio side made it, and `ControlClaim` carries
it today. T4 does not introduce it and does not resolve it: it makes the
trade side consistent with the claim it must reconcile against, which is
the only thing that can make reconciliation work at all. Whether this
channel should map a real custodian instead is a question about the
channel's model, filed as `iaam-xep0`, not something to decide inside a
task about trades.

## 3. The existing data, which is the hard half

Revision 3 of the parent design said facts recorded under
`tinkoff-api/1` "will not reconcile until re-imported". **Re-importing is
not repair, and the bead is right to say so.**

An old fact carries account-derived custody. A re-imported one carries
position-derived custody. Custody is part of `OperationKind`, and the
canonical form that produces the content fingerprint covers `kind`
(`iaam-ingest/src/dedup.rs:350-355`), so the fingerprint differs too.
After T2 the identity differs as well — `{op}#{num}` against the old bare
`{op}`. Nothing links the two. `dedup::assess` returns `Fresh`, the event
is appended, and the position is counted twice.

So a re-import of affected history does not fix a wrong position: it
creates a wrong position twice the size.

### 3.1 The affected set is identified by the defect, not by a version

The obvious predicate is the parser version, and it is wrong. `/3` does
not mean position-derived custody: T2 bumped the version, and the trade
branches still fabricate custody at this moment
(`iaam-app/src/adapters/tinkoff.rs:474,485` against
`iaam-broker/src/tinkoff/parse.rs:17`). A fact appended from the epic
branch as it stands today would be `/3` **and** fabricated, and a
version-based predicate would bless it. Bumping the version in T4 would
patch that particular hole and leave the shape of the mistake: a marker
that says when the code changed, standing in for a property of the data.

The predicate is the property itself:

> an event of kind `Trade` on this account whose `custody` equals
> `CustodyId(account.inner())`.

That is exactly the defect, needs no version marker, and is independent
of which channel or which credential recorded it — which also disposes of
a second trap: the persisted `SourceChannel` carries an opaque `SourceId`
and no broker identity
(`iaam-core/src/reconciliation/evidence.rs:23`), and a T-Invest source id
is the id of the *currently active* broker-access record
(`iaam-app/src/adapters/sqlite.rs:658,727`), so facts recorded through a
since-revoked access would not compare equal to the current channel's
source at all.

The one false positive this predicate admits: a legitimate custody
identifier that happens to equal the account's own UUID. `CustodyId` and
`AccountId` are both bare UUID wrappers and nothing enforces disjointness
(`iaam-core/src/ids.rs:10`), so it is possible in the type system and has
the probability of two independently generated UUIDs colliding. It is
named here rather than defended against.

The set is bounded and probably small: before `iaam-jdmc` a trade row
carrying a commission was rejected outright, so only commission-free
trades could have been recorded through this channel. "Probably small" is
not "empty", and §3.3 requires the strategy proven on a seeded journal
either way.

### 3.2 Strategy: refuse the account, do not insert beside

Of the three strategies the bead allows — reversal, replacement, refusal
to re-import — T4 takes **refusal**, and files the repair.

When `sync_broker` is about to sync an account whose journal contains a
fact matching §3.1, it **refuses that account's sync** before appending
anything, with a message naming the count of affected events and pointing
at the repair task. Other accounts of the same owner are unaffected: the
refusal is per account, because the harm is per account.

**The check must read the whole history, and the vector `sync_broker`
already has does not contain it.** `sync_broker` loads
`load_events_through(owner, to)` (`iaam-app/src/scenarios/sync.rs:70-74`),
and the store filters `effective_date <= to`
(`iaam-store/src/events.rs:104-112`). An affected trade dated **after**
the interval being synced would escape an account-wide refusal — and a
narrow interval is exactly what an owner syncs when re-importing a
suspicious month. An earlier revision of this document claimed the loaded
vector answered the question; it does not.

The check therefore loads the owner's events through `Date::MAX` before
the append loop. That is the whole journal, which this scenario already
loads most of; if journal size ever makes it expensive, the fix is a
narrow store query for the predicate, not a smaller date bound. The
distinction matters and must not be lost in implementation: the
**dedup** vector stays bounded by `to`, because dedup is about the
interval; the **refusal** vector is the whole history, because the defect
is not confined to the interval.

Why refusal rather than reversal-and-replacement here:

- the harm to prevent is the silent double-count, and refusal prevents it
  completely and immediately;
- reversal plus replacement is the correct repair and the core already
  models it (`Relation::Reversal`, `Relation::Replacement`,
  `iaam-core/src/event/mod.rs:39-46`), but it needs an entry point to run
  from — a maintenance route or a job — and none exists. Building one
  inside a broker-channel task is exactly how the monolithic design grew
  until it could not be reviewed (parent §12);
- refusal is reversible and loses nothing. The facts stay, the owner is
  told which ones block the account, and the repair can run whenever it
  is built.

What refusal costs, stated: an owner with affected history cannot sync
that account at all until the repair exists. That is deliberate. The
alternative on offer is a portfolio that reconciles against nothing and
silently doubles on the next sync, and a refusal the owner can read beats
a number they cannot.

The repair — reverse each affected trade, replace it with the same trade
carrying position-derived custody, under a versioned repair that names
itself in provenance — is filed as `iaam-y3a2`. That bead carries the
same predicate: custody equal to the account's own identifier, not a
parser version.

### 3.3 Proven on a seeded journal

The bead requires the strategy proven, not asserted. Three tests over a
seeded event list:

1. a journal seeded with one account-custody trade on the account refuses
   the sync, and the message names the count;
2. a journal seeded only with position-custody trades syncs normally;
3. a journal seeded with an account-custody trade on **another** account
   of the same owner does not block this account;
4. a journal seeded with an account-custody trade dated **after** the
   requested interval still refuses, and appends nothing — the case the
   `to`-bounded vector would have missed.

## 4. Existing tests

The adapter's tests assume the fabricated custody and must be rewritten
around the real identifier. That is a rewrite, not a deletion: each test
keeps its subject and changes what it expects the custody to be. A test
that exists only to assert `CustodyId(account.inner())` is deleted, and
its deletion named in the report — a test asserting the defect is not
coverage.

## 5. Not changed by T4

- `TINKOFF_PARSER_VERSION` stays `tinkoff-api/3`. T4 deliberately does
  **not** bump it: §3.1 explains why a version marker is the wrong tool
  for this predicate, and bumping it would only make the wrong tool look
  right.
- The portfolio side. It already uses `position_uid`.
- The report channel's custody resolution.
- The core's reversal and replacement mechanics — T4 uses neither; the
  repair task will.
- Pagination and securities transfers — T5, T6.

## 6. Acceptance criteria

1. A trade fact's `custody` is the row's `position_uid`.
2. A trading row without a `position_uid` is quarantined, with the field
   named, and does not fall back to the account.
3. A position built from trades and the `ControlClaim::PositionQuantity`
   the portfolio raises for the same holding carry the same custody, so
   the claim can match.
4. An account whose journal holds a trade whose custody equals the
   account's own identifier refuses to sync, naming the count, and
   appends nothing — including when that trade is dated outside the
   interval being synced.
5. The refusal is per account: another account of the same owner still
   syncs.
5b. A claim about a **non-zero** holding matches after the change and did
   not before; the design does not claim anything about zero holdings,
   which matched either way (§1).
6. No adapter test asserts account-derived custody.

## 7. Tests

Inline JSON and seeded event lists in the crates' own test modules;
`tests/fixtures/api/` is a policy directory
(`scripts/check-diff-lint.sh:80-83`) and may be read, not modified.

- the recorded BUY's fact carries `positionUid`
  `f1a60ae6-3f1e-43c8-8d46-042df0fdc97a` as its custody;
- a trading row with `positionUid` absent, and one with it empty, are
  both quarantined naming the field;
- the custody on a trade fact and the custody on the portfolio claim for
  the same instrument are equal — asserted directly rather than through a
  reconciliation run;
- a reconciliation of a **non-zero** holding matches after the change and
  does not before, on the recorded fixture's own BUY and portfolio
  position;
- the three seeded-journal cases of §3.3;
- a non-trade family is unaffected by all of the above.

## 8. Gates

`cargo check` and the crates' own tests, as in T1 to T3. The workspace
gates run once at the end of the epic.
