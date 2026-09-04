# 0008. A rule minted from an answer asks about one field

Date: 2026-09-04 · Status: proposed · Bead: `iaam-g7yc`

## Context

When the owner answers a classification question, the system turns his answer
into a standing `ClassificationRule` so that the next row like it settles without
asking. When an agent answers, it cannot — generalising is owner-only — and the
answer instead publishes the rule it *would* have made, as a proposal the owner
adopts.

Both come from one function, `matcher_for`, and it filled every field the row
offered:

```rust
RuleMatcher {
    counterparty_account: row.counterparty_name().map(str::to_owned),
    description_contains: row.description.clone(),
    kind: row.source_kind.clone(),
}
```

`RuleMatcher::matches` joins every present field with **and**. So the rule
demanded the counterparty exactly, *and* the source's word for the operation
exactly, *and* the description as a case-insensitive substring — and the
substring it was given is the row's **whole** description. A description is the
most row-specific thing a statement prints. A rule conditioned on the whole of
one recognises that row and, in practice, nothing else.

The rule was therefore correct and empty: a standing decision that settles one
line the owner had already settled by hand. Worse for the proposal, which exists
so the owner is not asked about the same counterparty every month — he adopts it,
and he is asked again anyway.

`docs/import-boundary.md` §7 recorded the question as open and said explicitly
that choosing which of the three a rule ought to ask about is the owner's
decision. This is that decision written down, which is why the status is
*proposed*.

## Decision

**A proposed matcher asks about exactly one field**, chosen in this order:

1. the **counterparty** the row named, where it named one;
2. otherwise the **word the source used** for the operation;
3. otherwise the **description**, whole.

A row printing none of the three proposes no matcher at all, unchanged: a
condition that asks nothing matches nothing, and an "everything" rule would
silently reclassify the portfolio.

### Why one, and why these three in this order

**One, because that is what a rule people meant looks like.** Every
hand-written classification rule in this workspace — in the ingest tests, in the
application tests, in the classification scenario's own fixtures — sets exactly
one field. Not one of them sets two. Those are the rules written by people who
meant something by them, and a proposal the owner is asked to adopt should have
the shape of a rule somebody would write.

**The counterparty first**, because the classification is a claim about who the
money moved with. "Anything with this counterparty is a fee." "This printed name
is in fact my own account at another bank." The printed name is the field that
identifies him, it is matched exactly, and it is the narrowest of the three that
still generalises past its own row.

**The source's word second.** It is matched exactly against a vocabulary one
source controls, and it is what a row with no counterparty has instead of one:
the word a bank prints for a movement internal to itself is the whole of the
evidence such a row carries.

**The description last**, because it is the only one matched as a substring and
the only one taken whole, so it barely generalises. It is kept rather than
dropped because the alternative is worse: with no matcher the row reports
`Generalisation::Impossible`, which claims that no rule can be built from it
under any token, and that would be false.

### What this costs

A matcher on one field settles more rows than a matcher on three, and one of them
can be settled wrongly. A source word a bank prints on every transfer would
carry one row's classification onto rows that do not deserve it. That is the
trade-off, and it is taken in this direction for two reasons.

The proposal is only ever *offered*. It is published as the body of the call that
writes classification rules, for the owner to read, narrow and send; the queue
item that carries it says so. And a rule he adopts is one he can retire, which
replans the history it classified.

And the rows this is computed for are rows the classifier could **not** settle —
a row a rule already matches is never asked about — so the field a proposal names
is one no standing rule was conditioned on.

The cost falls differently on the *automatic* rule, the one the owner's own
answer writes without being offered anything. There he does not see the condition
before it stands. He does see it afterwards, in his rule listing, where he can
edit or retire it, and the recomputation plan tells him what it changed. That is
the weakest point of this decision and the reason it is proposed rather than
taken: an owner who would rather approve each rule before it stands is asking for
a different arrangement, and it is his to ask for.

## Consequences

- `matcher_for` returns a single-field matcher; the fallback chain is total over
  the rows that generalise at all, and the `asks_nothing` guard remains the last
  word.
- The proposal an answer publishes, and the queue item that offers it, name one
  condition. A client showing it to the owner should show the condition, because
  that is the part he may want to change.
- Nothing already stored changes. Rules written under the old behaviour keep
  matching what they matched; they are simply narrower than anything written now.
- Reversing this is one function and one document. Nothing else reads
  `matcher_for`, and no stored shape depends on which fields it fills.

## Alternatives rejected

**Keep all three and let the owner delete what he does not want.** It reads
fairer and is not: the proposal's whole purpose is that adopting it costs one
call with an object copied unedited, and an object nobody may copy unedited is a
reconstruction with extra steps. It also leaves the *automatic* rule — the one
written when the owner answers under his own token, which he never sees before it
stands — as empty as it was.

**Fill all three but weaken the join to "any".** It changes the meaning of every
rule already stored, including the ones the owner wrote by hand expecting "and",
and it would reclassify his history on a deployment rather than on a decision.

**Take a fragment of the description rather than the whole.** Choosing the
fragment is guessing which words carry the meaning, which is the same guess
`classify` refuses to make about direction, one field along.
