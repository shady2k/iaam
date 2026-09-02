# E9.T2 — The computed frontier, and the action contract

Bead: `iaam-mr0f` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

E9.T1 gave an agent a standard way in. It still has no way to learn **what this
instance needs right now**. That knowledge lives in `docs/agent-skill/SKILL.md`,
which is a document a human maintains and which is wrong in four places today.

This task replaces it with an endpoint computed from state, and defines the one
envelope the rest of the epic reuses. It is the spine: E9.T3, T4 and T5 all add
detectors or carriers to what is decided here.

## 1. The honest name for the mechanism

Not a derived workflow. A **versioned action policy**.

A dependency graph does not remove hand-written knowledge; it relocates it into
predicates, effects and ranking. Saying "the order is derived, not written"
would be a comfortable lie: "accounts before a contour" is not implied by the
code — operations do not require a contour, and a balance can be recorded before
any operation, though it will be incomparable. That ordering is onboarding
policy, and policy is written by people.

What is genuinely better than the document it replaces:

- the predicate runs against live state, not against someone's memory of it;
- the address comes from a registered route, not from a string;
- the **same predicate** is the completion check;
- a test performs the action and proves the predicate goes quiet.

That is drift a contract test catches. It is not immunity from drift: a predicate
can fall out of step with a handler, a route can become a stub, required inputs
can change. Claiming otherwise is how the last document started.

### What is explicitly refused

A serialised requires/provides DSL — `requires: accounts.count > 0` and the like.
It needs an interpreter, a state vocabulary, typed joins across ports, cycle
handling, evaluation errors, query-cost control, schema evolution, and a planner
for alternative providers. That is a second application framework inside the
first. **Detectors are typed Rust functions** reading application-level views.

## 2. A requirement is not one kind of thing

Conflating these either blocks lawful behaviour or presents advice as obligation:

- `blocking` — the operation cannot succeed yet;
- `required_for_goal` — necessary for a named result;
- `recommended` — it works, the quality suffers;
- `external_input` — the system cannot obtain the value itself.

An owner-stated balance is `recommended` for a report and `blocking` for nothing;
a missing contour is `blocking` for a report. Reporting the first as blocking
would tell an agent it may not do something it may.

## 3. The envelope

Borrowed vocabulary wherever a standard has a name for it, so that the answer
reads as something already known and so that an Arazzo document could be
generated from it later. **We do not author an Arazzo file**: a hand-written
workflow document drifts exactly as `SKILL.md` did. We compute; Arazzo describes.

| Field | Source |
|---|---|
| `operationId`, `operationPath` | Arazzo Step Object |
| `parameters[] {name, in, value}` | Arazzo Parameter Object |
| `successCriteria[] {context, condition, type}` | Arazzo Criterion Object |
| `dependsOn` | Arazzo |
| `required`, `value`, `prompt` on a missing input | HAL-FORMS property object |
| `pointer` | JSON Pointer, RFC 6901, as RFC 9457 §3 uses it |
| `input_required` as a state word | MCP Tasks |

```
Action {
  id             stable identity: dedup, rendering, tracking across refreshes
  kind           stable discriminator; the agent branches on this
  category       blocking | required_for_goal | recommended | external_input
  state          ready | blocked_external | informational
  reason         prose
  subject        typed per kind
  required_scope
  rank_reason
  target         Operation { operationId, operationPath, method, path,
                             requestSchema, parameters, request }
                 | Options([…])          -- mutually sufficient choices
                 | None                  -- nothing to call
  successCriteria
}
```

**`target` is a sum type, not three optional fields.** A struct of `Option`s can
represent `ready` with no operation, `request` without an operation, and an
operation alongside alternatives — all of which we would call illegal and none
of which the type would prevent. A test cannot prove future constructors behave.
The wire form stays flat; the internal representation makes the illegal states
unrepresentable.

There is **no `executor`**. No response assigns an HTTP call to a human
(ADR-0003). `required_scope` stays, because clients differ in what they may do.

### `reason` is prose, and that is deliberate

The consumer is a language model, and prose is its highest-bandwidth channel.
`kind` already carries the typed identity that tests, deduplication and grouping
need; a `reason.code` would be a second taxonomy over the same fact.

Prose for an agent is not prose for a human: state and consequence, no
politeness, no repetition of what `subject` already carries, entities named by
identifier as well as by name. **Tests assert on `kind`, never on the sentence**,
or rewording the text breaks the build and the text stops being improved.

### `provided_by` is ours

Each entry in `request.missing` is a JSON Pointer plus `provided_by`: `owner`
(knowledge only the owner has), `external_document`, `caller` (the request itself
is malformed).

Nothing surveyed expresses this. The state of the art is A2A's binary
`INPUT_REQUIRED` versus `AUTH_REQUIRED` — needing input versus needing authority.
Nothing distinguishes "the owner knows this", "a statement carries it" and "you
sent it wrong". It is an invention, recorded as one, and it is the field the
agent branches on to decide whether to ask a human at all.

### `successCriteria` extends its borrowed meaning

Arazzo evaluates a criterion against the response of the step just taken.
**Ours asserts an invariant that becomes true once the outstanding item is
done** — a different subject. Same shape, and the difference is stated in the
generated description rather than left for a reader to discover.

## 4. Addresses come from the registered routes

`operation_id` is the only address written by hand, and it is written on the
route. `utoipa-gen` 5.5.0 takes it as an `Expr` (`src/path.rs:48`), so it is a
shared constant rather than a repeated literal.

Resolution reads the **completed** document returned by `split_for_parts`, not
`ApiDoc::openapi()`: `ApiDoc` declares schemas while paths are merged by
`OpenApiRouter`, so the latter would advertise an operation that was never
mounted. Duplicate `operation_id`s are rejected by the scan — utoipa does not
enforce global uniqueness.

What that proves: the route was registered, and its declared method, path,
request body and security. What it does **not** prove: the middleware chain, that
runtime authorization matches the declared security, or that the handler accepts
what the schema claims. The black-box test is not redundant with it.

### The queue must not require a full-document fetch

Our OpenAPI document is dozens of schemas and forty-odd routes. An agent needing
one call must not pay context for all of it: what an item advertises has to be
enough to make the call. If it is not, the addressing has failed and the agent
falls back to reading everything — which is the cost that `draft-aiendpoint-ai-discovery`
answers with a hand-written summary, and a hand-written summary is a second
description that drifts.

`operationPath` is therefore a convenience for a client that already holds the
document, never a substitute for the method, path and required inputs being
present in the item.

## 5. Construction becomes fallible, in the right order

`build(state) -> (Router, OpenApi)` has no error channel and spawns the market
scheduler **before** assembling the routers, so a validation failure today would
follow a background side effect.

- `build` returns `Result<_, BuildError>`; call sites in `iaam-bootstrap` and the
  contract tests are updated.
- Order: assemble routers → produce the completed document → resolve and validate
  the catalog → install it → **then** spawn the scheduler and return.
- The catalog is shared, not copied: `Arc<OnceLock<ActionCatalog>>`. `ServerState`
  is cloned into the middleware before the router is complete, and a bare
  `OnceLock` would clone an empty cell that never fills.
- Repeated `build` on clones of one state is defined explicitly: refused, or
  idempotent with an equality check. A silently failing `set` is neither.
- The `Handle::try_current()` guard before spawning survives the reorder, or
  synchronous router tests panic for want of a runtime.

A server that cannot address its own actions must not start, and must not have
started anything else first.

## 6. `GET /v1/actions`

Named for the frontier, not for a sequence. `/v1/next` would promise a single
global order, and the domain has none.

```json
{
  "as_of": "2026-09-02",
  "policy_version": 1,
  "summary": { "blocking": 2, "required_for_goal": 1, "recommended": 37 },
  "groups": [ { "key": "reconciliation", "count": 29, "returned": 5 } ],
  "items": [ ],
  "next_cursor": "..."
}
```

- **Never one row per account × month × dimension.** Grouped by the unit one call
  can affect; for setup, one action per missing resource class.
- This introduces the API's first outbound pagination, so it sets a precedent:
  an opaque keyset cursor, deterministic ordering, a default limit of 20 and a
  maximum of 100. Not offset.
- Traversal is **eventual, not a frozen snapshot** — state changes between pages
  — and the contract says so. A duplicated or vanished item between pages is
  acceptable and documented; silent omission is not.
- `rank_reason` on every item. Ranking is policy and is versioned. With no stated
  goal, deterministic category order beats pretending to know the single best
  action.

## 7. Two detectors, and the capability one of them needs

`create_first_account` reads `list_accounts` (`ports.rs:223`), which exists.

`create_first_contour` cannot be written today: there is no `list_contours`. The
store loads a contour only by a known id (`reference.rs:513`,
`latest_contour_version` at 539), and the port exposes `load_contour` and
`latest_contour_version` — both of which need an id the caller does not have. So
"accounts exist, no contour" is not computable, and this task adds the capability
through store, port and application.

Each detector states, in one place, the query that produces the item and the
query that proves it done. They are the same query.

## 8. Tests

- Every input the referenced schema requires is either preset or listed in
  `request.missing`. Asserting only that each listed pointer exists would pass
  while an action stayed unusable.
- Every action kind that **has** an operation resolves to exactly one; its
  advertised method and path equal the resolved operation's; a black-box request
  reaches the handler. Non-invokable items are exempt by construction — by not
  having a target — never by a skipped assertion.
- An integration test performs each action and proves the item disappears,
  through the predicate that produced it.
- `ActionCatalog` construction rejects a document with a missing or duplicated
  `operation_id`, tested over a mutated document, since `build` cannot be
  compiled with a route removed. Separately, `build` yields a catalog containing
  every declared kind.
- Illegal envelope states are unrepresentable; where the type cannot express
  that, a test names each and shows it is unreachable.
- Pagination: ordering is deterministic across repeated calls on unchanged state.

## 9. Not in this task

Journal milestones and control assertions (E9.T3). Reconciliation diagnostics and
non-invokable items with no repair route (E9.T4). Actions attached to verdict and
report responses (E9.T5). The RFC 9457 error migration (`iaam-3pkr`). `403`
remediation and the unlimited missing-header path (`iaam-hbfw`). Reducing
`docs/agent-skill/SKILL.md` (`iaam-zu6m`).

## 10. Risks

**The envelope is designed against two detectors.** Both are setup actions with
`owner` inputs; neither exercises `external_document`, `successCriteria` against
a real invariant, or `Options`. Those arrive in T3 and T4, and the fields they
use may prove wrong. Accepted because the alternative is designing the envelope
against imagined cases, and because nothing external depends on the contract yet.

**Ranking will be argued about.** It is policy, it is versioned, and
`policy_version` exists so that a client can notice it changed. The first version
deliberately does not rank across categories.

**The queue becomes a second place where domain knowledge lives.** It already is
one — that is the point — and the guard is that every piece of that knowledge is
a query with a test that performs the action. Anything that cannot be written as
a query does not belong here; it belongs in the prose that survives in
`SKILL.md`.
