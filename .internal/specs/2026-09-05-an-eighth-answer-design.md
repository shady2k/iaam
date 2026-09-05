# An eighth answer: between my own accounts, and I cannot say which

Bead: `iaam-axrf` · Prior: `iaam-fmih`, `iaam-0evk` · Decisions: 0006, 0012, 0013 ·
Date: 2026-09-05

## The gap

A question about an import row publishes the answers it admits. There are seven,
and `Answer::classification` (`crates/iaam-ingest/src/classification.rs:978`)
maps them onto five outcomes. `Classification::OwnAccountMovement` is not one of
them: it is reachable only from `FarSide::OwnAccount` — the **source's** own
assertion — on a row that therefore raises no question at all.

So a statement that prints «between the owner's own accounts» passes, and a
person who knows exactly that does not. His two available answers are both
wrong: naming a far account records a movement whose other half does not exist,
and «I paid somebody» files an internal move as spending.

**This is decision 0006's own defect, one row later.** 0006 compared the two
channels outcome by outcome and found that something could be said conclusively
and not observationally, so «the honest path was the losing one». The same table
now has another row, and the honest path is losing again — this time against the
owner rather than against a converter.

The wave that named the gap made it acute: an import session now tells him «this
document holds no counterpart for this row», which is true, useful, and points
at a door that does not exist.

## Decision

**An eighth answer shape**, meaning: this was money moving between accounts of
mine, and I cannot say which account the other side is.

It lands in the shapes decision 0013 already built, and in no new one:

- where the direction is known — the source printed a sign, or the owner gave
  one — `EventKind::OwnAccountMovement { amount }`: a signed amount and one cash
  leg on the row's own account;
- where it is not, `EventKind::UnresolvedOwnAccountMovement { amount }`: a
  magnitude and **no legs**, because the journal cannot debit or credit an
  account on a movement whose direction nobody stated.

`Answer::movement()` stays total over the answers that have a direction; the
second case is the one that has none, and it has no leg for exactly that reason.

## What this does not do, stated so nobody reads more into it

**The amount is not counted as an internal transfer.** `FlowEndpoints::OwnAccountUnnamed`
classifies as `FlowClass::Indeterminate`, and `docs/irreversible-core.md` lists
that as unchangeable without a migration: «an account of the owner's» is not
«inside this contour», for any contour — no contour can prove it holds every
account he has.

What he gets is therefore precise and partial: **his spending stops being
overstated**, and the amount is *reported* as one the report could not place
rather than absorbed into a figure. That is the behaviour decision 0013 designed
— an unplaceable amount is reported rather than absorbed — and it is the honest
half of the answer.

The other half is a separate decision: the owner asserting that the far side is
inside the group he drew, which is a different claim from the source's and would
have a different outcome. It touches the irreversible core and is filed
separately.

## This answer writes no standing rule

The other seven say what a thing **was**, and generalise. This one says what the
document did not contain, which is a fact about one row.

A rule made from it would file every later row of the same shape as an
unplaceable movement — including, next month, the rows whose other half *is* in
the export and which the pairing would have settled completely. A rule made of
«I cannot say» converts future known cases into unknown ones, and it does so
silently.

This is a decision rather than a discovery, and it is cheap to reverse if a live
session shows him answering the same thing every month.

## Success criteria

1. A question that admits an own-account answer also admits this one, and its
   published consequence says what it does to the figures of his year.
2. Answering it records `OwnAccountMovement` where a direction is known and
   `UnresolvedOwnAccountMovement` where none is, and posts no leg in the second
   case.
3. It writes no classification rule, and the answer's own published
   generalisation says so rather than leaving a client to infer it.
4. The sentence the previous wave added — «this document holds no counterpart
   for this row» — now leads to an answer that exists, which was that wave's one
   unmet criterion.
5. `make check` passes.
