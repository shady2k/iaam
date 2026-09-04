# 0021. The authority a call demands is a property of the call

Date: 2026-09-04 · Status: proposed · Beads: `iaam-woeh`, `iaam-4jso`

## Context

Two findings about the outstanding-work queue, both of them about the queue
telling a client something that is not so. They are one decision because the
second is what the first's shape makes possible: an item that can say «this
resolution wants that authority» is an item that can also say «this question
could not be answered».

### The queue graded an item, and an item is not what wants an authority

`ActionFacts` carried one `Option<Scope>` for a whole item. Most items have one
way out and the field was exact for them. `retired_account_not_empty` has three:
a reconstructed opening, a correction, and the withdrawal of the retirement
statement. The first admits an agent token; the other two are the owner's. One
field cannot say three things, so it said `owner` — the safe reading — and an
agent that filtered the queue by the token it holds dropped the item entirely
and never reached the call it could in fact have made. The queue told a client
the server would refuse a request the server would have accepted.

`start_account_import` hid the same gap. Its two options both admit an agent, so
the single field happened to be true; nothing about the shape made it true.

Behind that sat a second defect, and it is the one that made the first
inevitable. The authority a call demands was written **twice**: once in the
handler, as `require_admin` or a `may_submit` test, and once by hand beside every
queue item that offered the call. Two statements of one fact are a fact that
eventually disagrees with itself. `iaam-3nqt` recorded the same class for
`closed_by`, where a named remedy was checked for existence and not for effect.

The obvious place to resolve the authority is the completed contract, the way
`ActionCatalog::from_openapi` resolves a key's method, path and request schema.
The contract does not carry it. Every route declares the same
`security(("bearer" = []))`, and the prose beside the refusal already disagrees
with the handlers — one owner-only route says «Owner only» and its neighbour,
equally owner-only, says «Insufficient permissions». OpenAPI 3.1 does permit role
names in a non-OAuth security requirement, so the document *could* be made to
carry it; but `utoipa::openapi::security::SecurityRequirement` keeps its map
private, so what was written there could not be read back in the typed form the
catalogue resolves everything else in. A fact written into the contract that
nothing reads back is a second hand-maintained statement, which is the defect,
not the fix.

### The queue failed entirely when a retired owner's journal would not fold

`frontier` folds the effective journal for an owner who has retired anything, to
decide whether a retired account still holds a figure. A journal that will not
fold therefore failed the whole request.

That was taken deliberately, on a good argument: there is no honest third value
for «is it empty», and a guessed one is either an item the owner does not owe or
a silence about one he does. What the argument left out is stated two functions
away, in `standing_rules`, about a rule the queue cannot read: **the queue is the
surface the owner recovers from.** An owner whose journal will not fold had no
queue to recover through, and the one act that repairs the fold was published
nowhere he would look for acts. `iaam-y1dp` records the same fork for the rules
port.

## Decision

### 1. One statement of the floor, and the route enforces from it

`iaam_app::ports::required_scope` is a total function from `OperationKey` to the
narrowest `Scope` that reaches the call. It is exhaustive, so a seventeenth
operation cannot compile until someone has answered, for that call, what
authority it demands.

`iaam_server::routes::require` gates every one of the sixteen routes by asking
that function. The handler no longer states an authority of its own; it reads the
one it publishes. Drift between «what the queue says» and «what the route does»
is not caught by a test — it is not expressible, because the two are one
expression.

`require_admin` survives for the owner-only routes no `OperationKey` names —
aliases and declarations, categories, instruments, tokens, broker access. Those
have no second reader, so there is nothing for a floor to disagree with. A route
that becomes an `OperationKey` moves to `require` in the same edit.

The word is **floor**, and `docs/api/conventions.md` §4.7 already used it: the
transport keeps only the scope that cannot be right under any journal.
Everything narrower is decided where the evidence is, and §4.4's split — one
route, two acts, gated separately inside — is unaffected.

### 2. Every resolution publishes its own floor; the item publishes the narrowest

`ResolutionOptionDto` and `ActionTargetDto::Operation` carry `requiredScope`, and
so does `ClosingOperationDto`, because a caveat's `closed_by` names the same sets
of calls for the same states and a register that names an owner-only remedy to an
agent has told it to make a call that will be refused.

`Action::required_scope` is no longer stored. It is the narrowest floor among the
item's resolutions, and `ActionFacts` has lost the field, so an item cannot hold
an opinion about an authority separate from the calls it offers.

**The narrowest and not the widest.** A client filtering a queue by the token it
holds wants one pass over the items, and the question it is asking is «is there
anything here I can act on». The narrowest floor answers exactly that. The widest
would answer «can I finish this alone», which no single value can answer: whether
a call succeeds depends on the body, and who holds a value the queue cannot
supply is already published as `provided_by` on the missing field. So a client
that keeps the items its scope admits sees every item it can make at least one
call on, and does not see the items where it can make none; what it does **not**
see from that field alone is which of an item's resolutions it may call, and that
is on the resolutions.

Two invariants moved as a consequence. `BlockedWithScope` is gone — the
combination cannot be built, because the scope is no longer supplied.
`NonBlockedWithoutScope` now refuses a non-blocked item that offers no
resolution, which used to be legal and should not have been: an item the owner
must act on through no call in this API is `Blocked`, and that is the word for
it.

### 3. A fold that refuses is an item, not a failed queue

`retired_products` no longer propagates the fold's refusal. Correction resolution
and the balance projection are the two steps that read the *content* of the
events, and each of them returns `Retirements::NotAssessed` carrying what
refused. The store failing to answer at all still fails the request: a store that
will not answer takes every other read with it, and there is no queue left for an
item to appear in.

The refusal becomes an item of its own, `retirement_not_assessed`. It says the
question could not be answered, quotes what refused, and names
`submit_corrections` as the way out — the only write in this system that changes
what an existing fold sees, and therefore the honest remedy for both refusals.
Nothing is preset: which fact should stop counting is the judgement the item
cannot make for the owner.

The item that would have been raised is **not** raised. That is the whole
discipline: an item silently absent because its input failed is worse than a
request that failed loudly, because the caller reads «nothing outstanding».

One item for the owner and not one per retired account: the fold is over the
whole journal and either produced verdicts for every declaration or for none. It
carries no subject, because the subject is the journal and no account is what
refused.

It names one goal, the asset snapshot, which is the goal of the item it stands in
for. A journal that will not fold of course refuses more than the snapshot — but
this item is raised only for an owner who has retired something, so grading it
against every report would say nothing at all to an owner who has retired nothing
and whose journal is just as unfoldable, while telling the one who has that his
*retirement* is what stands between him and his money flow. The goal an item
names is the goal it is about.

## What was rejected

**Writing the floor into the OpenAPI security requirement.** It is permitted by
3.1 and it would put the fact where a client already looks. It was rejected
because utoipa exposes no reader for it: the catalogue could write it and could
not resolve it, so the document would carry a hand-generated annotation that
nothing checks — a second statement, which is the defect. If a future utoipa
exposes the map, this becomes a publication of the same one statement and should
be revisited.

**A per-resolution `Scope` stored on `ResolutionOption`.** The finding asked for
a scope per resolution and this is the literal shape of it, but the value is a
function of the operation, so storing it would be a copy that can disagree with
the function — the same defect one level down. The resolutions publish the floor;
they do not carry it.

**Keeping the item's field and asserting it equals the derived value.** That is
the hand-maintained table with a test bolted on, and the test would pass on the
day someone changed both.

**A fourth `ActionState` for «could not be computed».** The vocabulary already
distinguishes the states an item can be in, and this item is in an ordinary one:
the owner must supply something, and there is a call for it. What is unusual is
the *kind*, and the kind is where it is said.

**Grading `retirement_not_assessed` as `Blocked`.** `Blocked` means no operation
in this API is available, and one is. Naming a call in prose while the target
says `none` is exactly the reading `iaam-4hcy` refused.

**Degrading the other reads `frontier` makes.** `standing_rules` already skips a
rule it cannot read, and that stays. The one remaining content failure —
a stored import question this build cannot parse — still fails the queue, and it
is left alone here: the same shape would fix it, the argument is the same, and it
is a different item to design. It is filed rather than fixed.

## Consequences

A published client sees three additions and one changed value.

- `requiredScope` appears on every action resolution — on
  `target.options[]`, on `target` when it is an operation, and on every
  `confidence.caveats[].closed_by[]` entry. Additive; no field was removed or
  renamed.
- The item-level `required_scope` of `retired_account_not_empty` changes from
  `owner` to `agent`. A client filtering the queue by an agent token now sees
  that item, which is the finding: it was being told the item was unreachable
  when the ordinary remedy was reachable.
- A new item kind, `retirement_not_assessed`, can appear in the queue. Clients
  that switch exhaustively on `kind` see one more value; clients that read
  `reason` and `target` need no change.
- `GET /v1/actions` no longer fails for an owner whose journal will not fold. It
  returns a queue with the new item in it, and the reports that fold the same
  events go on failing.

Inside the workspace, `ActionFacts` loses a field and
`ActionInvariantError::BlockedWithScope` is gone; both are workspace-internal.
