# 0040. The agent's authority is bounded by what cannot be undone

Date: 2026-09-06 · Status: proposed · Beads: `iaam-lzwb`, `iaam-45gc`

## Context

`docs/api/conventions.md` §4.2 grades an operation by **reach**. Its question is
what the act does to decisions the owner has made or will have to live with, and
its three bullets sort every write into one of three answers: disposing of
something the caller itself submitted is the agent's; stating a standing decision
that will apply to rows nobody has looked at is the owner's; ruling on what is
already in the journal is the owner's.

Applied, that rule makes eleven operations owner-only: creating an account, a
contour, a contour version, a category rule and a classification rule; recording
an account's transfer partners, its scope and its retirement; recording a
disposition about a name; recording a control balance; and submitting
corrections.

**A field report measured what that costs.** An external agent operating a
deployed instance, holding both tokens, worked a queue of forty-two items. Of
the thirty-nine open, thirty-one needed the owner and eight were reachable by the
agent. In the arrangement this system is designed for — the agent is the way the
owner reaches his own books — that is thirty-one departures from the conversation
to a console, for one month of one person's money.

Two of those single decisions account for most of it. Seventeen
`resolve_transfer_relationships` items and fourteen `provide_control_assertion`
items are, between them, thirty-one owner-scope requests expressing two
judgements.

**The owner's position.** Prohibitions must be justified, and the justification
is destruction and irreversibility — not reach.

## Decision

### 1. The test is whether the act can be undone

An operation is the owner's when performing it wrongly cannot be put back. It is
the agent's otherwise. Reach is not the question; a standing decision that can be
withdrawn is a decision the owner can revisit, and one he never sees because the
agent could not make it is a decision he never gets to revisit at all.

### 2. Applied here, the test admits all eleven

This is not a close call, and the reason is that this project already made
reversibility the shape of the journal. Decision 0003 makes it append-only: a
correction is two facts, never an edit, so nothing recorded is ever destroyed.

| Operation | What undoes it |
|---|---|
| `CreateAccount` | retirement; an account carrying no facts is inert |
| `CreateContour`, `AddContourVersion` | contours are versioned — the undo is the mechanism itself |
| `CreateCategoryRule`, `CreateClassificationRule` | a rule is retired, and retirement reports what it reached |
| `RecordAccountTransferPartners`, `RecordAccountScope` | restated; the call is a `PUT` |
| `RecordAccountRetirement` | withdrawn under the same key |
| `RecordAccountNameDisposition` | `disposition: undecided`, already published as the settled item's own target |
| `SubmitCorrections` | a correction is itself two facts in an append-only journal |
| `RecordOwnerBalance` | restated — but see §4 |

### 3. What stays the owner's is credentials, and only credentials

Two things in this system cannot be put back, and neither is about reach:

- **Issuing a token.** An agent that can mint a credential escapes every limit,
  including this one. It is not a fact in a journal and no correction reverses
  it.
- **Broker credentials and the encryption key.** A deleted broker access is one
  the owner must obtain and enter again; the key is stated by this system's own
  documentation to be unrecoverable from a single database.

`SyncBroker` stays the agent's, as it already is. It spends a stored credential
on an outward call and reads; reading destroys nothing.

### 4. A control balance is admitted, and the reason it is different is recorded

`RecordOwnerBalance` passes the test: the record is restated like any other. It
is nonetheless unlike the other ten, and the difference is worth writing down
where it is graded rather than discovered later.

A control balance is the owner's own assertion about the world outside this
system, and reconciliation is computed **against** it. Nothing here can ever
detect that it is wrong, because it is the thing everything else is checked by.
Every other fact an agent may now record has something to disagree with it; this
one has nothing.

It is admitted because the test admits it and the test is the rule. What follows
is §5.

### 5. Reversible is not the same as noticed, and attribution is what replaces the console

The guard being removed was never really the refusal. It was that the owner had
to type it himself and therefore saw it.

A wrong fact in the journal reads exactly like a right one until somebody looks,
and every report computed in between is wrong. The rollback is one call; the
noticing is the whole cost. So this decision is not «widen the gate» on its own —
it is inseparable from making the agent's decisions **reviewable as a set**: the
owner asks what his agent decided over a period and reads it as one list, with
what would undo each beside it. The provenance is already recorded on the facts;
what is missing is the read.

The two land together. Admitting the eleven without §5 trades a cost the owner
can see for one he cannot.

## Rationale

The reach test answers a real question — it is exactly right about *which
decisions are the owner's* — and it uses that answer for the wrong purpose. Which
decisions are his governs what he must be **asked**, and the skill already
enforces that with a rule the transport cannot: an agent never answers in his
place, never reads silence as a value, and never narrows what he is being asked.
A 403 was doing a second, cruder job on top of that, and doing it by making the
honest path expensive.

That expense is not neutral. Thirty-one console departures is not a security
control that holds; it is one an owner routes around by pasting without reading.
A boundary obeyed in form and defeated in practice protects less than a wider one
that is actually observed.

The alternative considered was the field report's own proposal: keep the eleven
owner-only, but let an agent token submit a decision when it matches, byte for
byte, the proposal the system itself computed — compared server-side against the
system's value, never the submitted one. It is a good mechanism and it removes
injection and hallucination completely. It was not chosen because it does not
address the risk that actually matters: an agent that never asked the owner is
indistinguishable, at the server, from one relaying his answer. It buys a
guarantee against a failure mode the skill already forbids, at the cost of a
whole mechanism, and leaves the one that needs detection untouched. §5 addresses
that directly and costs less.

## Consequences

- `crates/iaam-app/src/ports.rs`'s `required_scope` is regraded, and each arm
  carries the undo rather than the reach.
- `docs/api/conventions.md` §4.2 and §4.3 are rewritten. The old three-bullet
  test is removed rather than kept beside the new one; two tests for one question
  is how a later reader restores the one that was replaced.
- Published authority changes for eleven operations. Every client that inferred
  what an agent could do from a refusal will now see it succeed.
- The four bullets' fourth case — an act returning the journal to the state
  before the caller acted, admitted «under a bound narrow enough to be checked
  rather than trusted» — stops being a special case, since the general rule now
  covers it.
- Nothing here relaxes the skill. An agent still never answers in the owner's
  place; what changes is which call carries his answer, not who decides.
- §5 is not optional and is tracked as `iaam-45gc`. If it is not built, this
  decision should be reversed rather than left standing alone.
