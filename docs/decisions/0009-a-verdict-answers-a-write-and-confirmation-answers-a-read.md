# 0009. A verdict answers a write; confirmation answers a read

Date: 2026-09-04 · Status: proposed · Bead: `iaam-eio5`

## Context

The ingestion vocabulary publishes ten verdict codes. `verdict_vocabulary!` in
`crates/iaam-ingest/src/verdict.rs` is the single source for the Rust enum, the
wire code and the OpenAPI schema alike, and its own comment claimed that "a
client reading the contract sees the same ten entries the server can produce".

**One of the ten is produced by nothing.** `Verdict::Accepted`, published as
`accepted` and described as "the fact was recorded and reconciliation matched",
is constructed at exactly two places in the tree, both unit-test fixtures. Every
other mention — in the broker sync, in transfer pairing, in the response DTO —
is a match arm that reads or serialises a value nothing creates. Every write
path ends at `Recorded::Inserted` and maps it to `Verdict::Provisional`, and
nothing upgrades it afterwards.

This is not a cosmetic drift. A previous reader took the published sentence at
its word and wrote two tests waiting for a code that never arrives. The document
misled them; their carelessness did not.

### Why nothing produces it, which is the part that decides this

A verdict and a reconciliation status are different kinds of thing, and the
difference is structural rather than a matter of unfinished work.

A **verdict** is the answer to one write. It is computed inside the request that
recorded the row, returned in that response, stored nowhere, and restated by no
later call. It is true of a moment and is never revisited.

A **reconciliation status** is a property of an account, a dimension and an
interval. `ReconciliationLedger` folds the journal against the owner's and the
sources' control assertions when a report is read, and yields
`DimensionStatus` — `discrepant`, `provisional`, `accepted_internal`,
`accepted_independent`. It moves: a second channel raises it, a correction
lowers it, a coverage gap disqualifies the attempt that would have confirmed it.
§10.3 says as much of `provisional` — "no action; the status will rise by
itself".

Nothing on a write path can honestly say that reconciliation matched, because at
that moment the fold that would decide it has not been taken and the evidence
that would move it has not arrived. Even where a write path comes closest —
committing an import session, which compares the batch against the control
section the source printed on the same page — what it has established is that
these rows agree with this one document. That is a statement about a batch, not
about the account's reconciliation state, and it would have gone stale by the
time the response was parsed.

So `accepted` in the verdict vocabulary is not an unimplemented feature. It is a
word for something the system already has, in a place that cannot carry it.

## Decision

**`accepted` stays in the published vocabulary, and its published meaning says
that no path emits it.**

The meaning now reads: reserved, emitted by nothing, and confirmation is
reported by the data quality block as `accepted_internal` or
`accepted_independent`. A contract test asserts that sentence, so an
implementation that ever makes the code reachable must change the description in
the same edit.

### Why not implement it

Because implementing it means putting a value that changes into a response that
is sent once. A client that stored `accepted` from a verdict would hold a claim
the system had stopped making, and would have no way to learn that it had — and
a client that treated the absence of `accepted` as "not confirmed" would be
wrong about every row of a fully reconciled account. Both failures are worse than
the code never arriving, because both look like information.

### Why not remove it

Because removal is a breaking change to a published enum that buys nothing.
`accepted` is one of the six verdicts §10.4 names, and deleting a value the
design document mandates is the owner's decision, not an agent's. Set against
that, the gain is zero: the branch was unreachable, so no client can ever have
taken it, and no client's behaviour changes when it goes. What a client is owed
is not a shorter list but a truthful one.

If the owner prefers removal, this decision is the argument for it as much as
against it, and the migration is small: drop the variant, the four dead match
arms and the two fixtures, and the wire code disappears with them, because the
schema is expanded from the same list.

### What a client that branches on `accepted` should do

Nothing to its code, and one thing to its reading. The branch is dead and always
has been, so deleting it changes no behaviour and keeping it costs nothing. What
must change is any place that treats a verdict as the answer to "is this
confirmed": that question is answered by the data quality block, per account,
dimension and interval, and it is answered differently at different times.

**This decision is not breaking.** No code is removed, no code is added, no
serialised value changes. Two schema descriptions and several doc comments
change.

## Consequences

- `crates/iaam-ingest/src/verdict.rs` states, on the type and in the published
  meaning, that a verdict answers a write and confirmation answers a read.
- `crates/iaam-server/tests/contract.rs` holds the published sentence, and holds
  it against a code that *is* emitted, so the caveat cannot spread into the rest
  of the vocabulary unnoticed.
- `docs/agent-skill/SKILL.md` tells an agent not to wait for the code, beside
  the existing rule not to call `provisional` an error. The two mistakes are the
  same mistake from opposite ends.
- Two further codes, `discrepancy` and `needs_reconciliation`, are also produced
  by nothing today. They were **not** covered by this decision, and this section
  guessed that theirs was a gap rather than an impossibility: a commit that
  overrides a control mismatch writes rows the source's own figures contradict,
  which something could report as a discrepancy, and a missing owner remainder is
  asked for by the action queue instead of by a verdict.

  **Decision 0011 examined both and found the guess wrong.** What a commit
  override knows is a fact about a batch and a document rather than about an
  account's reconciliation, and it is already reported in three places; and
  `needs_reconciliation` could never be emitted truthfully at all, because no
  write is declined for want of a remainder. Both are reserved, on the terms this
  decision sets for `accepted`. Read 0011 for where the three differ.
