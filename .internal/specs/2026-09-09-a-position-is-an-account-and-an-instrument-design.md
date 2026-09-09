# A position is an account and an instrument

Beads: `iaam-xep0` (P1), `iaam-1u7b` (P1). Epic: wave 3 of `iaam-wwgv`.
Unblocks: `iaam-laoh`, `iaam-6v5j`, `iaam-0fb3`.

## 1. The decision, and whose it was

**The owner keeps positions at the level of the broker and of the different
accounts he holds at that broker.** An account already says which broker and
which account there. A place of custody adds nothing to a position's identity.

That is his answer to a product question, not an engineering preference, and it
settles what two earlier design rounds could not. Both of those rounds tried to
find a custody value that all channels could agree on; there is none, because
the API channel's value — the broker's `positionUid` — is not a place at all.

**The system is greenfield: no facts on disk, no archives, no snapshots.** So
there is no migration, no reversal, no re-import, no fingerprint versioning, no
projection-cache invalidation and no backward compatibility to preserve. An
adversarial review of this design listed all four as required work; each of them
is required only where data already exists.

## 2. What a position is

```
PositionKey { account, instrument }
```

`custody` leaves every key, every grouping and every equality test: `Balances`
(`projection/balances.rs:18`), the anchor maps (`anchor.rs:97`), `ObservedTotals`
(`observed.rs:215`), `check_claim` (`check.rs:393`), the continuity comparison
(`reconciliation/mod.rs:617`), and `ControlClaim::subject_key` (`claim.rs:179`).

**The doc comments that justified the old key are rewritten, not left standing.**
`PositionKey`'s says a transfer of securities between depositories within one
broker is a real operation and custody is therefore part of the key;
`anchor.rs:197` says the same in its own words. Both stop being true. A comment
that argues for a decision the code no longer makes is worse than no comment: it
tells the next reader the opposite of what holds.

**One deletion is load bearing and easy to miss.** `snapshot_positions`
(`observed.rs:608`) skips a position whose custody is `None`, on the stated
ground that "the report's claim always names a depository, so a position
recorded without one is not eligible for reconciliation." Once custody is not
identity, that branch removes from reconciliation every position an API sync
produced. It goes.

## 3. What custody becomes

**It stays, as description, and stops pretending to be an identifier.**

A broker report prints a place of custody — `«место хранения»` in one,
`«место учета»` in another — and it is a fact the source stated. Refusing to
record it would be this system throwing away what it was told. Corporate actions
and settled offers carry one for the same reason.

So `Leg.custody`, `event_corporate_action.custody` and
`event_offer_exercise.custody` remain, and `custody_places` with them. Nothing
keys on any of it.

**`positionUid` stops being written there.** This is the honest half of the
change. If a broker handle keeps going into the custody field, then the field
does not hold "where the paper is kept" — it holds either that or an opaque
position handle, depending on which channel filled it, which is the defect
`iaam-xep0` has recorded since T4. The handle moves to a provenance field of its
own, `source_position_id`, where it is what it is.

**A consequence to state rather than let someone discover.** Once no producer
mints, `custody_places.origin` — added in wave 1 to keep minted handles out of
the owner's vocabulary — can only ever hold `declared`. The column is not
removed here: it is the guard that nothing mints again, and removing it in the
same change that stops the minting would leave nothing to notice a regression.
It is named as vestigial-by-design so that a later reader does not mistake its
single value for an oversight.

## 4. Two portfolio rows for one instrument

Today two rows for one instrument are distinguished by their `positionUid` in
`subject_key`. After §2 they are not, and something must decide what they mean.

**They are refused as a pair, and not summed.** Summing is only valid if the
rows are disjoint additive components, and nothing in this system can currently
establish that: `RawPortfolioPosition` (`tinkoff/parse.rs:754`) drops `blocked`,
`blockedLots`, `quantityLots` and ignores `virtualPositions` entirely, though the
response carries all of them. A total and one of its own subsets look exactly
like two peers once those fields are gone.

The arithmetic is worse than lossy — it launders. A negative security quantity
is refused outright (`event/mod.rs:924`: "shorts are out of scope, and a minus
here means either a short or a reversed sign during parsing"), so netting a long
against a short would turn a component the domain refuses into a total it
accepts, with nothing recording that it happened.

`iaam-1u7b` repairs the parser so that what the broker said survives. It is a
**prerequisite for accepting duplicates**, not for this change: the key collapse
ships with an explicit refusal, and the refusal names the pair it will not
guess about.

## 5. What is retired

- **The sync gate** (`sync.rs:98`), which refuses synchronisation while any
  trade carries account-derived custody, has no subject once custody is not
  identity.
- **The T4 repair endpoint** (`scenarios/custody_repair.rs`, and its route) with
  it. It writes reversals and no replacements, so running it after this change
  would retract facts for a defect that no longer affects any figure. Retiring
  a destructive route whose reason has gone is part of the change, not a
  follow-up: a route that still exists will be run.

## 6. What travels outward

`custody` appears in the returns assessment, its result types and its
`inputs_hash` (`returns/mod.rs:493, 731, 1928`), in the balances and assets
reports (`report/balances.rs:153`, `report/assets.rs:83`) and in the HTTP DTO
(`server/src/dto.rs:3928`). All of it goes: a caller cannot be offered a
breakdown by a dimension the system no longer computes. The OpenAPI document
changes with the DTOs, and the contract tests with it.

## 7. Testing

1. Two trades for one instrument on one account, recorded with different custody
   values, are one position.
2. A position an API sync produced — custody stated or not — is reconciled
   rather than skipped. This is the `observed.rs:608` deletion, and it fails
   before the change.
3. Two portfolio rows for one instrument are refused as a pair, with a refusal
   naming both, and the rest of the snapshot still imports.
4. A broker sync writes `positionUid` as `source_position_id` and **not** as a
   leg's custody; a test asserts the leg carries no minted place.
5. A report row naming a place of custody still records it, and the fact still
   carries it after a bundle round trip.
6. No `custody_places` row is created by any sync.
7. The returns assessment and both reports answer without a custody dimension,
   and the OpenAPI document no longer publishes one.

## 8. Order

`iaam-1u7b` is independent and lands first or alongside. Then the core key
change and the claim shape together, because they share `claim.rs`. Then the
outward surface, and the producer change, in parallel. Then the retirement of
the gate and the repair route, which depends on the producer change.

Nothing here is a data migration. It is a change of what the system computes,
made while nothing has yet been computed.
