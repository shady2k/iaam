# E9.T2 — The computed frontier, and the action contract

Bead: `iaam-mr0f` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

E9.T1 gave an agent a standard way in. It still has no way to learn **what this
instance needs right now**. That knowledge lives in `docs/agent-skill/SKILL.md`,
a document a human maintains and which is wrong in four places today.

This task replaces it with an endpoint computed from state, and defines the
envelope the rest of the epic reuses. It is the spine: T3, T4 and T5 add
detectors and carriers to what is decided here.

An earlier draft of this spec was larger and is withdrawn. It carried pagination
over a computed set, a summary and groups, ranking, `dependsOn`, an Arazzo-shaped
condition language and a flattened target — none of it exercised by the two
detectors this task actually ships. What remains is what those two detectors
prove.

## 1. The honest name for the mechanism

A **versioned action policy**, not a derived workflow.

A dependency graph does not remove hand-written knowledge; it relocates it into
predicates and ordering. "Accounts before a contour" is not implied by the code —
operations do not require a contour, and a balance may precede any operation,
though it will be incomparable. That ordering is onboarding policy, and policy is
written by people.

What is genuinely better than the document it replaces:

- the predicate runs against live state, not someone's memory of it;
- the address comes from a registered route, not from a string;
- the detector and the completion check read **the same projections**;
- a test performs the action and asserts the completion condition holds.

That is drift a contract test catches. It is not immunity from drift: a predicate
can fall out of step with a handler, a route can become a stub, required inputs
can change. Claiming otherwise is how the last document started.

### What is explicitly refused

A serialised predicate DSL — `requires: accounts.count > 0`, or a condition
language carried in the response. It needs an interpreter, a state vocabulary,
typed joins across ports, cycle handling, evaluation errors, cost control and
schema evolution: a second application framework inside the first. **Detectors
are typed Rust functions** reading application-level views.

This is also why there is no `successCriteria` in the envelope. A condition
string is either executable — the DSL just refused — or prose, which is not an
observable completion check. See §5 for what replaces it.

## 2. Eligibility and completion are different things

The first draft said "the same predicate is the completion check". That is false
for the second detector, and the failure is instructive.

```
create_first_account   emit: accounts.is_empty()
                       done: !accounts.is_empty()

create_first_contour   eligible: accounts are non-empty
                       gap:      contours are empty
                       emit:     eligible && gap
                       done:     contours are non-empty
```

If the accounts vanish, `eligible && gap` goes false and the item disappears
**without having been done**. So absence from the queue is not proof of
completion, and a test asserting only absence would pass on a regression that
deleted the accounts.

Three separate notions, named separately in the code:

- **eligibility** — may this be offered now;
- **gap** — is the thing missing;
- **completion** — the positive postcondition proving the need was met.

Tests assert completion positively, never by absence alone.

## 3. Categories

- `blocking` — the operation cannot succeed yet;
- `required_for_goal` — necessary for a named result;
- `recommended` — it works, the quality suffers.

There is deliberately no `external_input` category. It is not exclusive with the
other three — a balance can be recommended *and* need the owner — and the
provenance of a value already belongs to the value, in `provided_by`. Putting it
in the category would make a field that is two things at once.

## 4. The envelope

Vocabulary is borrowed where a standard has a name — `operationId` from Arazzo's
Step Object, `pointer` as RFC 9457 §3 uses JSON Pointer, `required`/`value`/
`prompt` from the HAL-FORMS property object. That is **inspiration, not
compatibility**: this is a local vocabulary, and no Arazzo document can be
generated from it mechanically. Saying otherwise would be the same overclaim the
epic exists to remove.

```
Action {
  id             stable identity: dedup, rendering, tracking across refreshes
  kind           stable discriminator; the agent branches on this
  category       blocking | required_for_goal | recommended
  state          ready | needs_owner_input
  reason         prose
  required_scope
  target         tagged, see below
}
```

### `target` is tagged on the wire, not flattened

```json
"target": { "type": "operation",
            "operationId": "create_account",
            "method": "POST",
            "path": "/v1/accounts",
            "requestSchema": "#/components/schemas/CreateAccountRequest",
            "request": { "preset": {}, "missing": [ … ] } }
```

Internally a sum type, so that `ready` with no operation, or an operation beside
its own alternatives, is unrepresentable rather than merely disapproved of.
Flattening it would move the problem to the serializer: `serde(flatten)` over an
untagged enum does not produce a `oneOf` that expresses the exclusivity, so the
generated schema would either permit the illegal states through the public
contract or be hand-authored — a second source of drift. Later variants
(`{"type":"options"}`, `{"type":"none"}`) then cost nothing.

### `state`

`ready` means the agent can invoke it now. `needs_owner_input` means it must ask
the owner first — which is the case for **both** detectors in this task, so
calling them `ready` would make the field meaningless on its first outing.

### `reason` is prose, and that is deliberate

The consumer is a language model, and prose is its highest-bandwidth channel.
`kind` already carries the typed identity that tests and deduplication need; a
`reason.code` would be a second taxonomy over the same fact.

Prose for an agent is not prose for a human: state and consequence, no
politeness, entities named by identifier as well as by name. **Tests assert on
`kind`, never on the sentence**, or rewording breaks the build and the text stops
being improved.

### `request.missing` and `provided_by`

Each entry is a JSON Pointer into the request schema, plus `provided_by`:
`owner`, `external_document`, `caller`.

Nothing surveyed expresses this. The state of the art is A2A's binary
`INPUT_REQUIRED` versus `AUTH_REQUIRED`. It is an invention, recorded as one, and
it is the field an agent branches on to decide whether to involve a human at all.

An entry may carry **`candidates`**: values the system knows and the owner must
choose among. `create_first_contour` needs the accounts that belong to the
contour, and the system holds their ids, titles and institutions. Without
candidates the agent must either make an undocumented second call or guess that
every account belongs — and guessing is what this epic exists to stop.

## 5. Completion, without a condition language

One generic contract, stated in the response's own documentation:

> Re-fetch `/v1/actions`. The action with this `id` is done when it is absent
> **and** its completion condition holds. Absence alone means only that it is no
> longer applicable.

The completion condition lives in the detector, in Rust, next to the query that
emits the item. It is not serialised, because serialising it means shipping
either an interpreter or a sentence, and §1 refuses both.

## 6. Addresses come from the registered routes

`operation_id` is the only address written by hand, and it is written on the
route. `utoipa-gen` 5.5.0 takes it as an `Expr` (`src/path.rs:48`), so it is a
shared constant. Only action-addressable routes get an explicit one; everywhere
else utoipa's default (the handler name) stands.

Resolution reads the **completed** document returned by `split_for_parts`, not
`ApiDoc::openapi()`: `ApiDoc` declares schemas while paths are merged by
`OpenApiRouter`, so the latter would advertise an operation that was never
mounted.

Implementation notes that are not optional:

- `PathItem` in utoipa 5 has **no operation iterator**. Each method field is
  visited explicitly — the same lesson the existing contract test at
  `tests/contract.rs` already had to learn.
- Two distinct failures, named distinctly: an operation whose `operation_id` is
  `None`, and a catalog reference resolving to no operation.
- Duplicate `operation_id`s are rejected by the scan; utoipa does not enforce
  global uniqueness.

What resolution proves: the route was registered, with its declared method, path,
request body and security. What it does **not** prove: the middleware chain, that
runtime authorization matches the declared security, or that the handler accepts
what the schema claims. The black-box test is not redundant with it.

`operationPath` is not emitted. It is an alternative to `operationId` in Arazzo,
not a companion, and a JSON Pointer into the OpenAPI document is only useful to a
client that already holds it — which §7 says we must not require.

### The queue must not require a full-document fetch

Our OpenAPI document is dozens of schemas and forty-odd routes. What an item
advertises has to be enough to make the call. If it is not, the agent falls back
to reading everything, which is the cost that a hand-written summary answers —
and a hand-written summary is a second description that drifts.

## 7. `GET /v1/actions`

```json
{ "policy_version": 1, "items": [ … ] }
```

No pagination, no cursor, no `as_of`, no summary, no groups, no ranking. Two
detectors produce at most two items, and a cursor over a **computed** set cannot
honour the consistency the earlier draft promised: with recomputation between
pages, a new item can sort before the cursor and never be seen, so "no silent
omission" was undeliverable. Pagination arrives when a detector demonstrates
volume, and then as one honest model — best-effort traversal that says so, a
cursor bound to a state revision, or pagination inside a stable group.

`policy_version` is a number a person increments. Nothing forces it, and this
spec does not pretend otherwise; it exists so a client can notice a change that
was declared, not so that every change is declared.

## 8. Two detectors, and the capability one needs

`create_first_account` reads `list_accounts` (`ports.rs:223`).

`create_first_contour` cannot be written today: the store loads a contour only by
a known id (`reference.rs:513`, `latest_contour_version` at 539). The database
can answer it — `bundle.rs:120` already runs an all-contours query inline for
export — so what is missing is an API, not a capability. This task adds a
`ContourView` (contour id, latest version, title, account ids) through store,
port and adapter; a boolean would be replaced in T3 the moment a report action
needs to name the contour.

One behaviour must be decided rather than inherited: `load_contour` treats a
version with no memberships as absent (`reference.rs:531`) while bundle import
permits one (`bundle.rs:238`). The listing states which it follows and why.

## 9. Tests

- Every input the referenced schema requires is either preset or listed in
  `request.missing`. Asserting only that each listed pointer exists would pass
  while an action stayed unusable.
- Each detector: emitted when the gap exists; **the positive completion condition
  holds** after the action is performed; losing eligibility is not mistaken for
  completion.
- Every action kind resolves to exactly one operation; its advertised method and
  path equal the resolved operation's; a black-box request reaches the handler.
- `ActionCatalog` construction rejects a document with a missing or duplicated
  `operation_id`, over a mutated document — `build` cannot be compiled with a
  route removed.
- The tagged target round-trips, and the generated schema expresses the
  exclusivity rather than permitting every combination.

## 10. Construction

`build(state) -> (Router, OpenApi)` has no error channel and spawns the market
scheduler **before** assembling the routers, so a validation failure would follow
a background side effect.

- `build` returns `Result<_, BuildError>`; call sites in `iaam-bootstrap` and the
  contract tests are updated.
- Order: assemble routers → completed document → resolve and validate the catalog
  → **then** spawn the scheduler.
- The catalog is installed as an Axum `Extension<Arc<ActionCatalog>>` on the
  completed router. It is **not** put in `ServerState` behind a `OnceLock`: the
  circularity that would have required disappeared when T1 dropped the
  authenticate action, only `/v1/actions` needs the catalog now, and an
  `Extension` removes the empty-cell case, the repeated-build question and a
  runtime "not initialised" branch in the handler.
- The `Handle::try_current()` guard before spawning survives the reorder, or
  synchronous router tests panic for want of a runtime.

A server that cannot address its own actions must not start, and must not have
started anything else first.

## 11. Not in this task

Journal milestones (T3). Diagnostics and non-invokable items (T4). Actions on
verdict and report responses (T5). The RFC 9457 migration (`iaam-3pkr`). `403`
and the unlimited missing-header path (`iaam-hbfw`). Reducing `SKILL.md`
(`iaam-zu6m`). Removing the HTTP routes that still accept a broker credential
(`iaam-phym`) — a real contradiction of ADR-0003, but not this task's.

## 12. Risks

**The envelope is designed against two detectors.** Both are existential setup
actions, both POST a JSON body, both need owner knowledge, neither has path
parameters, alternatives, or a domain subject. Fields they do not exercise were
cut rather than shipped untested; the ones that remain may still prove wrong when
T3 brings an action about an existing account and period. Accepted, because
nothing external depends on the contract yet.

**The queue is a second place where domain knowledge lives.** It is, and that is
the point. The guard is that each piece is a query with a test that performs the
action. Note the limit honestly: T4 will carry items for problems with no repair
route, where nothing can be performed and the test can only assert the item
appears. That is a weaker guarantee, and it belongs to T4 to state.
