# E9.T1 — Discovery without a token, and one action contract

Bead: `iaam-y10f` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

An agent arriving at a running instance has no canonical way to learn what this
API is and how to authenticate, except by reading a document a human maintains.
That document (`docs/agent-skill/SKILL.md`) is wrong in four places today.

This task gives the agent a first call that answers without prose, and defines
the one envelope every later answer in E9 will use.

**What it does not do, stated plainly.** It does not get anyone authenticated. A
token is issued at the console and injected into the client by local tooling
(ADR-0003); no API call produces one, and after this task none can. The value
here is a canonical entry point, an honest recovery answer on `401`, and the
contract plus its route resolver — which is infrastructure the next three tasks
all stand on. An earlier draft of this spec claimed "a client with no prior
knowledge reaches an authenticated state". That was false and is withdrawn.

## What already exists

- `/v1/openapi.json` is **unauthenticated**, added to the combined router after
  `split_for_parts`, outside the `authenticate` middleware (`lib.rs:174`). It
  answers which operations exist, with methods, paths, schemas and security. It
  is mounted with plain `Router::route`, so it does not describe itself — and
  `/v1` must not be mounted the same way, or discovery will advertise a
  capability document that omits discovery.
- `/v1/health` answers unauthenticated with status and versions.
- `ApiError` carries `code`, `message`, `field`, `expected`, `actual`,
  `correlation_id` (`error.rs`).

The OpenAPI document is necessary and insufficient. It says what may be called.
It cannot say why an operation is relevant now, which values the system already
holds, whether a condition blocks or advises, or what observably proves a step
done. It is the capability document; E9 adds the work.

## 1. `GET /v1` — the discovery document

Unprotected, mounted through `OpenApiRouter` so that it appears in the document
it points at.

```json
{
  "service": "iaam",
  "api_version": "v1",
  "openapi": "/v1/openapi.json",
  "health": "/v1/health",
  "authentication": {
    "scheme": "bearer",
    "header": "Authorization: Bearer <token>"
  },
  "action": { "...": "the authenticate action, see §3" }
}
```

No journal `SCHEMA_VERSION` here. An API client writes DTOs, not journal events;
the journal schema version is data-compatibility diagnostics and already reaches
clients through `/v1/health`, which discovery links to. Publishing it as though
it were the contract version would invite a client to branch on the wrong number.

### What it must not say

Discovery must not disclose whether the instance has been claimed or whether an
owner exists. It is built entirely from constants and the static action catalog
and reads no state, so this is a property of its construction, not a filter over
an answer.

The requirement is stated as: **the response body is byte-identical across
instance states**, asserted by a test over status, the relevant headers and the
raw bytes. It is not a claim that every observable channel is closed — with
claiming removed from HTTP (ADR-0003) the timing difference in `accept_claim`
disappears along with the route, but this document does not pretend to have
audited channels it does not control.

## 2. The action envelope

One type, defined once, used by every answer in E9. In this task it is a
transport type in `iaam-server`; domain-side action kinds arrive with the
detectors in E9.T2 and map into it rather than replacing it.

```rust
pub struct ActionDto {
    pub id: String,               // stable identity: dedup, rendering, tracking across refreshes
    pub kind: String,             // stable discriminator; the agent branches on this
    pub state: ActionState,       // ready | blocked_external | informational
    pub reason: ActionReason,     // { code, message } — code is control flow, message is not
    pub required_scope: Option<String>,
    pub operation: Option<OperationRef>,
    pub request: Option<ActionRequest>,   // { preset, missing }
    pub completion: Option<Completion>,
    pub options: Vec<ActionOptionDto>,    // mutually sufficient ways to satisfy one need
}
```

`ActionOptionDto` is a **narrower, non-recursive** type: `kind`, `reason`,
`operation`, `request`. An option cannot contain options. A recursive action type
would admit arbitrary-depth trees, leave it ambiguous whether the parent or a
leaf is the thing to do, force every client into recursive traversal, and require
deliberate recursion handling in the generated schema.

There is **no `subject` field** in this version. An untyped `Value` reserved for
"typed at construction" is typed only for us: the client and the schema still see
arbitrary JSON. Authentication has no domain subject, so nothing is lost; a
tagged subject type is added in E9.T2 when the first real subject exists.

Three rules that are the point of the type:

- **There is no `executor`.** No response assigns an HTTP call to a human
  (ADR-0003). `required_scope` stays, because clients differ in what they may do.
- **`operation` is `Option`.** Where no call resolves the need it is `null` and
  `state` says why. A plausible URL that does not fix the problem is worse than
  silence: the agent will follow it.
- **`request.missing` names values, not prose.** Each entry is a JSON Pointer into
  the request schema plus `provided_by`: `owner` (knowledge only the owner has),
  `external_document`, or `caller` (the request itself is malformed).
  `request.preset` carries what the system already knows, so the agent copies
  nothing from an unrelated field.

**Legal combinations are constrained, not merely avoided by constructors.** A
`ready` action has an operation; an `informational` one has none; `request`
without `operation` is not representable; an action carries either an operation
or options, never both. Where the Rust type cannot express this, a test asserts
each illegal combination is unreachable, and the schema documents the constraint.

## 3. The `authenticate` action

The only kind in this task, and after ADR-0003 it has exactly one option, because
no API call issues a credential any more:

```json
{
  "id": "authentication-required",
  "kind": "authenticate",
  "state": "blocked_external",
  "reason": {
    "code": "no_credentials",
    "message": "This API requires a bearer token. Tokens are issued at the server console and injected by local tooling; no API call issues one."
  },
  "options": []
}
```

`blocked_external`, and it is honest: the system cannot supply the credential and
no operation exists that would. It asserts nothing about this instance's state,
so it is identical everywhere.

`claim_instance` was in an earlier draft and is removed. Claiming moves to the
CLI with the one-time code retired (ADR-0003), so advertising it would send an
agent to a route that will not exist and, worse, would route a bootstrap secret
through the agent. This task therefore **depends on** the claim route's removal
in `iaam-iw9s`, or it advertises a lie for one release.

## 4. `401` carries the same action

`ApiFailure::unauthorized()` gains the envelope, and the response gains what the
protocol requires and it currently lacks:

- `WWW-Authenticate: Bearer` — the JSON body supplements the standard challenge,
  it does not replace it.
- `Cache-Control: no-store` and `Vary: Authorization`, so an intermediary cannot
  cache a refusal and replay it. Discovery itself may be publicly cacheable
  precisely because it is state-independent.

The action on this path is a **constant**. `authenticate` never varies, so it is
built once and cheaply cloned, not reassembled per refusal. This matters because
a missing `Authorization` header returns `401` **before** the rate limiter
(`auth.rs:47-49`), so that path is unlimited: enlarging it must not make it
expensive. Making that path limited is out of scope here and belongs with the
scope work in `iaam-hbfw`.

Both refusal paths are tested — no header at all, and a token that is unknown or
revoked — because they run through materially different code.

`403` is not in scope: a scope refusal cannot honestly say what to do before the
re-examination in `iaam-hbfw`.

## 5. Addresses come from the registered routes

`operation_id` is the only address written by hand, and it is written on the route
itself. `utoipa-gen` 5.5.0 takes `operation_id` as an `Expr` (`src/path.rs:48`),
so a shared constant is used rather than a repeated literal.

Resolution reads the **completed** OpenAPI document — the one `build` returns
from `split_for_parts` — not `ApiDoc::openapi()`. `ApiDoc` declares schemas; the
paths are merged by `OpenApiRouter` from the `routes!` registrations, so
resolving against `ApiDoc` alone would hand out an operation that was never
mounted.

What that resolution proves, and what it does not: it proves a route was
registered through `routes!` and gives its declared method, path, request body
and security. It does **not** prove the middleware chain, that runtime
authorization matches the declared security, or that the handler accepts what the
schema claims. The black-box test below is not redundant with it.

### Construction becomes fallible, and order changes

`build(state) -> (Router, OpenApi)` has no error channel (`lib.rs:107`), and it
spawns the market scheduler **before** assembling the routers (`lib.rs:110`) — so
a validation failure today would come after a background side effect.

This task changes it to `build(state) -> Result<(Router, OpenApi), BuildError>`
and reorders: assemble routers, produce the completed document, resolve and
validate the action catalog, install it, and only then spawn the scheduler and
return. A server that cannot address its own actions must not start, and must not
have started anything else first. Call sites in `iaam-bootstrap` and the contract
tests are updated.

The catalog is installed into `ServerState` behind a `OnceLock`, which resolves
the circularity: the state exists before the document does, and discovery and the
authentication middleware both need the catalog.

The alternative — calling the generated `__path_claim::path()` marker directly —
is rejected: it is doc-hidden generated API and proves only that an attribute
exists, not that a route was mounted.

## 6. Tests

- Discovery's body is byte-identical across instance states, over status, the
  relevant headers and raw bytes.
- **Every input the referenced schema requires is either preset or listed in
  `request.missing`.** Asserting only that each listed pointer names a real
  property would pass while an action stayed unusable — the defect that an
  earlier draft of this spec actually had, omitting `label` from `ClaimRequest`.
- Each pointer in `request.missing` names a real property of the referenced
  component schema.
- Every action kind resolves to exactly one operation, and the advertised method
  and path equal the resolved operation's.
- A black-box request to each advertised address reaches its handler.
- Illegal envelope combinations are unreachable (§2).
- `ActionCatalog` construction rejects a document with a missing or duplicated
  `operation_id` — a unit test over a mutated document, since `build` cannot be
  compiled with a route removed. Separately, `build` returns a catalog containing
  every declared kind.
- `401` carries the envelope, `WWW-Authenticate` and `Cache-Control`, on both
  refusal paths.

## 7. Not in this task

No work queue, no detectors, no state reading (E9.T2). No `403` remediation and
no rate-limiting change (`iaam-hbfw`). No actions attached to verdicts (E9.T5).
No edits to `docs/agent-skill/SKILL.md` beyond what `iaam-zu6m` does on its own.
No generic predicate language, now or later.

## 8. Risks

**The envelope is designed against one kind, and a thin one.** `authenticate`
exercises `state`, `reason` and `id`, but not `operation`, `request` or
`completion` — the fields that carry the weight later. Mitigated by nothing
external depending on the contract yet and by E9.T2 arriving next; accepted
knowingly, because the alternative is designing the envelope inside the queue
task and shipping both at once.

**Discovery becomes a second document.** If `GET /v1` grows prose about how the
system works, it is the skill in JSON. It carries versions, two links, the
authentication scheme and one action. Anything else belongs in the OpenAPI
descriptions or in `/v1/actions`.
