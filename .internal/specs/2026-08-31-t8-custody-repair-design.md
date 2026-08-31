# T8: repairing trades recorded with account-derived custody — design

Bead: `iaam-y3a2`. Date: 2026-08-31.

Preceding design: `.internal/specs/2026-08-31-t4-custody-design.md` §3.2,
which introduced the refusal this task exists to lift. Line numbers are
against `main` at commit `1698c40`.

Reviewed adversarially by codex on 2026-08-31. Seven findings, all
accepted; four of them made an earlier revision of this document
unimplementable, and they are recorded in §4 and §5 rather than hidden.
The one claim that survived the review unchanged is the central one:
**no replacement event is fabricated.**

## 1. Problem

Facts recorded through the T-Invest API channel before T4 (`iaam-6aun`)
carry custody fabricated from the account: `CustodyId(account.inner())`.
After T4 new facts carry the row's `position_uid`, so old and new facts
about the same holding sit under different keys, and observed positions
are keyed by `(InstrumentId, CustodyId)`
(`iaam-core/src/reconciliation/observed.rs:70`).

T4's answer was to refuse the synchronisation of any account holding such
a fact, reading the account's whole history rather than the synced
interval (`iaam-app/src/scenarios/sync.rs:99-107`, predicate at `:401`).
That refusal is deliberate — it stops a silent doubling — but it leaves
an owner with affected history unable to synchronise that account at all.
Lifting it is this task.

Re-importing is not repair on its own. Old and re-imported facts differ
in custody, which is part of `OperationKind` and so is covered by the
canonical form behind the content fingerprint
(`iaam-ingest/src/dedup.rs:350-355`); after T2 they differ in identity
too. `dedup::assess` therefore returns `Fresh`, the corrected fact is
appended beside the wrong one, and the position doubles.

## 2. Decision: reversal only, then an ordinary re-import

The bead proposed reversal **plus replacement**. Replacement is the wrong
half and this document drops it, for a reason that is a property of the
data and not of taste: **the correct custody is not in the journal.** It
is the row's `position_uid`, which lives in the broker's response. A
replacement built from the journal alone would have to invent it.

Reversal alone is sufficient for the projection, because of what
`resolve` does: an effective event is neither reversed, nor replaced, nor
itself a reversal (`iaam-core/src/event/correction.rs:90-97`), and the
projection is built from that effective set
(`iaam-core/src/projection/mod.rs:269`, `:317`). A reversed trade leaves
the position entirely; nothing has to take its place.

The correct fact comes back the way every other fact comes back — from
the broker, on the next ordinary synchronisation, with a real
`position_uid`. The repair does not guess it, and cannot get it wrong.

Reversal first, re-import second: at no point does the journal hold two
live facts for one holding.

## 3. The reversal event

One reversal per affected trade: same kind, same declaration, same
effective date and source time, a new identifier, and
`Relation::Reversal { target: original.id }`.

**Its legs mirror the original, and this is forced, not chosen.** A
`Trade` must carry exactly one cash leg and one security leg — `LegCount`
in `validate_trade` (`iaam-core/src/event/mod.rs:445-455`) — and after
`iaam-m03o` every write path validates. A legless reversal is not
writable, and a sign-negated one fails the same validation. Mirroring is
the only shape that passes, which is precisely why §4 exists.

**Provenance is newly constructed, not cloned.** This is the finding that
would have made the first revision silently do nothing:
`find_duplicate` tests `source_operation_id` **before** the idempotency
key (`iaam-store/src/events.rs:205` then `:232`). A reversal that
inherited its target's `source_operation_id` would be found as a
duplicate of the very event it reverses and would never be written, while
the repair reported success.

So the repair's provenance carries:

- the original's source — the repair concerns that source's facts, and a
  channel that never existed would be a lie about where the correction
  came from;
- parser version `custody-repair/1`, which is what makes the repair
  legible in the journal afterwards;
- a raw hash derived from the repair identity, not the broker row's — the
  reversal is our fact, not the source's;
- **no `source_operation_id` and no row locator.**

**Idempotency is structural.** The key is
`custody-repair/{account}/{target_event_id}`. It is enforced as
`(owner, idempotency_key)` (`iaam-store/migrations/0001_initial.sql:35`),
and each append runs in its own immediate transaction, so two concurrent
repairs cannot both write.

**"Already reversed" is read from relations, never inferred from a store
duplicate.** The store returns only the existing event's id
(`iaam-store/src/events.rs:19`); it does not prove that event is a
reversal of the intended target, and idempotency keys are client-supplied,
so an unrelated event could occupy the key. The count of
already-repaired targets is derived from the journal's own
`Relation::Reversal` links before anything is written.

## 4. Prerequisite: a reversal must be effective everywhere it is read

This is the part the first revision of this document got wrong, and it is
the reason T8 is an epic rather than a task.

`recompute_history` computes a correction plan and discards it —
`.map(|_| ())` (`iaam-app/src/scenarios/classification.rs:72`). **No
production path writes a reversal event today.** So the consumers below
have never met one, and this repair would be the first thing to produce
one. They are not pre-existing breakage that can be deferred; they are
breakage this repair would cause.

Every consumer that reads a raw event slice and sums legs must resolve
corrections first. With mirrored legs and no resolution, the original is
applied once and the reversal applied again with the same signs — the
repair would **double** exactly what it was run to remove:

| Consumer | Effect of an unresolved trade reversal |
|---|---|
| `project` / `advance` (`projection/mod.rs:269`, `:317`) | none — both resolve |
| `observe`, via `ReconciliationLedger::build_with` (`reconciliation/mod.rs:229`; `observed.rs:153-219`) | **wrong**: doubles balances, turnover, fees |
| `perimeter::assess` (`perimeter.rs:215`, `:248`) | **wrong**: doubles cash effects, can invent or erase negative-cash spans |
| `active_instruments` (`projection/active_instruments.rs:29`) | **wrong**: can keep a fully reversed instrument active |
| `first_event_date` (`jobs.rs:149`) | operational: a fully reversed trade still sets the market-data start date |
| `collect_groups`, `collect_coverage_gaps` (`reconciliation/mod.rs:352`, `:388`) | none for a **trade** reversal — they select other kinds |
| bundle export (`iaam-store/src/bundle.rs:106`) | none — exporting both is correct append-only audit behaviour |

Reports reach the wrong answers directly: they call the raw perimeter
assessment and then the raw reconciliation ledger
(`iaam-app/src/scenarios/reports.rs:461-464`).

**Second prerequisite: the store's duplicate search must not match a
reversed event.** `find_duplicate` (`iaam-store/src/events.rs:200`)
searches the raw table. Fixing `known_records` in the application
(§5.2) is not enough: the corrected fact passes the application gate and
is then suppressed by the store, so it is silently **missing** rather
than doubled — the worse of the two failures, because nothing reports it.
The relation columns are persisted (`events.rs:152`), so the query can
exclude ids that appear as a reversal target.

## 5. Two application-side changes

**5.1 The refusal predicate reads the effective set.**
`affected_trade_count` (`sync.rs:401`) scans the raw slice. A reversed
trade is still in that slice, so after a successful repair the refusal
would still fire and the account would still be unsynchronisable — the
repair would appear to do nothing.

**5.2 Deduplication reads the effective set.**
`known_records` is built from the raw `bounded_events` (`sync.rs:395`),
and `dedup::assess` suppresses an exact identity match (`dedup.rs:209`).
A reversed fact must not suppress its own corrected re-import.

This is a wider rule than the repair: a reversed fact never deduplicates
against anything, on any path. That is the right rule, and stating it
once is better than special-casing the repair. It has one accepted
consequence: `assess` also uses known records for `PossibleDuplicate`
hints (`dedup.rs:225`), so some outcomes move from `PossibleDuplicate` to
`Fresh` and lose the `of` audit link. The candidate is recorded either
way, and for this repair the corrected custody changes the fingerprint,
so the reversed target would not have supplied the hint anyway.

## 6. What the repair costs the owner, stated honestly

An earlier revision claimed the repair "does not make it worse" when the
broker access has been revoked. **That was false**, and the correction
matters more than the wording: before the repair the projection holds a
trade under a wrong custody key; after it, the effective projection holds
no such trade at all. Positions, cash movements, lots, basis and return
history built from those facts go with it, and the route deliberately
does not re-import.

So the repair reports, before writing anything, which case the account is
in:

- affected trades, and an active broker access that can restore the
  interval;
- affected trades, and **no** active access able to restore them — the
  repair retracts and nothing brings them back;
- nothing affected.

The second case is the owner's decision to make, not ours. The route
takes an explicit acknowledgement for it rather than proceeding silently,
because "N reversals written" is success at the journal level while the
owner's effective history is materially poorer.

**Partial application is accepted and named.** `append_checked` validates
the whole batch before writing, but the SQLite adapter appends each event
in its own transaction (`iaam-app/src/adapters/sqlite.rs:198`), so a
failure on target N leaves targets 1…N−1 reversed. Idempotency makes a
retry finish the job rather than redo it; the response says how many were
written so a partial run is visible rather than inferred.

**On effective order:** the store assigns the sequence itself,
`MAX(sequence)+1` (`iaam-store/src/events.rs:62`), and
`(owner, effective_date, sequence)` is unique
(`0001_initial.sql:31`). The reversal carries the same effective date and
source time; it does not carry the same sequence, and must not try to.
Resolution is set-wise, so replay order does not affect it.

## 7. Entry point

`POST /v1/accounts/{account}/repairs/custody`, beside the existing
maintenance-shaped route `reparse_document` (`iaam-server/src/lib.rs:130`).
Requires the submit scope, the same as the synchronisation it unblocks.

Owner-triggered rather than a background job, deliberately:

- a job that rewrites an append-only ledger without anyone asking is the
  wrong default, and §6 is why;
- the refusal is per account, so the repair is per account;
- it is observable — the owner sees how many facts were retracted, which
  is the number they need to judge the re-import that follows.

The response reports reversals written, targets already reversed by an
earlier run, and which §6 case the account was in. A second run writes
nothing, which is how the owner sees idempotency rather than being told.

## 8. What is deliberately not here

- **No replacement events.** §2.
- **No sweep across accounts or owners.** §7.
- **No automatic re-synchronisation.** Chaining would hide a failed
  import behind a successful repair.
- **No repair of anything but the T4 custody defect.** The predicate is
  T4's — a `Trade` on the account with a quantity leg whose custody
  equals `CustodyId(account.inner())` — and deliberately not the parser
  version, which does not identify the defect (`t4-custody-design.md` §1).
- **Correction-awareness is scoped to what §4 lists.** A reversal of a
  `ControlAssertion` would raise the same question for `collect_groups`;
  no path produces one, and inventing the answer here would be guessing.

## 9. Task breakdown

1. **Corrections become effective where legs are summed** — `observe` via
   `build_with`, `perimeter::assess`, `active_instruments`. Tests prove a
   reversed trade contributes nothing. (§4)
2. **The store's duplicate search ignores reversed events.** Test: a fact
   whose identity matches a reversed event is recorded, not suppressed.
   (§4)
3. **The refusal predicate and `known_records` read the effective set.**
   (§5)
4. **The repair scenario** — predicate, provenance, idempotency,
   already-reversed from relations, the §6 preflight. (§3, §6)
5. **The route and its DTOs**, including the acknowledgement for the
   no-active-access case. (§7)

Order matters: 1 and 2 are prerequisites. Landing 4 or 5 before them
produces a repair that doubles the very facts it retracts.

## 10. Acceptance

- An account with affected trades: one reversal per affected trade, and
  the subsequent synchronisation is no longer refused.
- After repair and re-import the position is correct and not doubled —
  asserted through the reconciliation ledger and the perimeter
  assessment, not only through the projection, because those are the
  consumers §4 is about.
- A reversed fact does not suppress a re-import of the same underlying
  row, proved on a fact whose identity matches, and proved at the store
  level and not only in `known_records`.
- Running the repair twice writes reversals once; the second run reports
  zero written and the same number already reversed.
- An account with no affected trades: nothing written, zero reported, no
  failure.
- An account whose affected trades cannot be restored: the repair says so
  before writing, and proceeds only on explicit acknowledgement.
