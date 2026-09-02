# E9.T1 — Discovery without a token, and one action contract

Bead: `iaam-y10f` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

An agent arriving at a running instance has no way to learn how to authenticate
except by reading a document a human maintains. That document
(`docs/agent-skill/SKILL.md`) is wrong in four places today. This task gives the
agent a first call that answers the question without prose, and defines the one
envelope every later answer in E9 will use.

It is useful on its own: a client with no prior knowledge reaches an
authenticated state, and a stale document stops being the only path in.

## What already exists

- `/v1/openapi.json` is **unauthenticated**. It is added to the combined router
  after `split_for_parts`, outside the `authenticate` middleware
  (`crates/iaam-server/src/lib.rs:168`). It answers which operations exist, with
  methods, paths, schemas and security requirements.
- `/v1/health` and `/v1/claim` are the other two unprotected routes.
- `ApiError` carries `code`, `message`, `field`, `expected`, `actual`,
  `correlation_id` (`crates/iaam-server/src/error.rs`).

The OpenAPI document is necessary and insufficient. It says what may be called.
It cannot say why an operation is relevant now, which values the system already
holds, whether a condition blocks or merely advises, or what observably proves
the step done. It is the capability document; E9 adds the work.

## 1. `GET /v1` — the discovery document

Unprotected, alongside `health` and `claim`. Returns:

```json
{
  "service": "iaam",
  "api_version": "v1",
  "schema_version": 11,
  "openapi": "/v1/openapi.json",
  "authentication": {
    "scheme": "bearer",
    "header": "Authorization: Bearer <token>"
  },
  "action": { "...": "the authenticate action, see §3" }
}
```

`schema_version` and the projection version already reach clients through
`HealthDto`; discovery repeats the journal schema version because an agent that
cannot parse the journal contract should learn it before writing, not after.

### What it must not say

It must not disclose whether the instance has been claimed. `claim` deliberately
returns the same `403` for an invalid, an expired and an already-used code
(`routes.rs:1228-1231`), so that a guess cannot be confirmed. A discovery
document saying "this instance is unclaimed" would hand back exactly what that
refusal withholds. The `409 already_claimed` answer is not a leak, because it is
reachable only with a correct code.

Therefore the discovery answer is **byte-identical** on a claimed and an
unclaimed instance, and a test asserts that against both states.

## 2. The action envelope

One tagged type, defined once, used by every answer in E9. In this task it is a
transport type in `iaam-server`; the domain-side action kinds arrive with the
detectors in E9.T2, and map into this envelope rather than replacing it.

```rust
pub struct ActionDto {
    pub kind: String,             // stable discriminator; the agent branches on this
    pub state: ActionState,       // ready | blocked_external | informational
    pub reason: ActionReason,     // { code, message } — code is control flow, message is not
    pub subject: Option<Value>,   // typed per kind at the point of construction
    pub required_scope: Option<String>,
    pub operation: Option<OperationRef>,
    pub request: Option<ActionRequest>,   // { preset, missing }
    pub completion: Option<Completion>,
    pub alternatives: Vec<ActionDto>,     // mutually sufficient ways to satisfy one need
}
```

Three rules that are the point of the type:

- **There is no `executor`.** Every call is made by the agent (ADR-0003). What a
  response says is where a missing value comes from, never who types the request.
- **`operation` is `Option`.** Where no call resolves the need, it is `null` and
  `state` says why. A plausible URL that does not fix the problem is worse than
  silence, because the agent will follow it.
- **`request.missing` names values, not prose.** Each entry is a JSON Pointer
  into the request schema plus `provided_by`: `owner` (knowledge only the owner
  has), `external_document`, or `caller` (the request itself is malformed).
  `request.preset` carries everything the system already knows, so the agent
  copies nothing from an unrelated field.

`subject`, `request` and `completion` are typed per kind where they are built.
They are not free-form bags: this task ships one kind, and the shape of the next
is decided when its detector is written.

## 3. The `authenticate` action

The only kind in this task. Identical on every instance:

```json
{
  "kind": "authenticate",
  "state": "blocked_external",
  "reason": {
    "code": "no_credentials",
    "message": "This API requires a bearer token."
  },
  "alternatives": [
    {
      "kind": "present_bearer_token",
      "state": "blocked_external",
      "reason": { "code": "token_not_held",
                  "message": "Present a token the owner has issued." },
      "operation": null
    },
    {
      "kind": "claim_instance",
      "state": "blocked_external",
      "reason": { "code": "claim_code_not_held",
                  "message": "A one-time code is printed to the server console at startup." },
      "operation": {
        "operation_id": "claim",
        "method": "POST",
        "path": "/v1/claim",
        "request_schema": "#/components/schemas/ClaimRequest"
      },
      "request": { "missing": [ { "pointer": "/code", "provided_by": "owner" } ] }
    }
  ]
}
```

`blocked_external` on both, because the system cannot supply either credential.
Neither alternative asserts anything about this instance's state, so the answer
conceals nothing and reveals nothing.

## 4. `401` carries the same action

`ApiFailure::unauthorized()` gains the same envelope in its body. An agent whose
token expired mid-session learns the way back in from the refusal itself, and the
type gets its second carrier immediately, which is what proves it is a shared
contract rather than one endpoint's shape.

`403` is not in scope here: a scope refusal needs the re-examination in
`iaam-hbfw` before it can honestly say what to do.

## 5. Addresses come from the registered routes

`operation_id` is the only address written by hand, and it is written on the
route itself. utoipa 5.5 accepts an expression, so the id is a shared constant
rather than a repeated literal.

Resolution reads the **completed** OpenAPI document — the one `build` returns
from `split_for_parts` (`lib.rs:165`) — not `ApiDoc::openapi()`. `ApiDoc`
declares schemas; the paths are merged by `OpenApiRouter` from the `routes!`
registrations. Resolving against `ApiDoc` alone would happily hand out an
operation that was never mounted.

The resolver is built once at server construction and fails construction — not a
request — if an id is missing or duplicated. A server that cannot address its own
actions must not start.

The alternative, calling the generated `__path_claim::path()` marker directly,
is rejected: it is doc-hidden generated API, and it proves only that the
attribute exists, not that the route was mounted.

## 6. Tests

- Discovery answers identically on a claimed and an unclaimed instance, asserted
  byte for byte.
- Every action kind resolves to exactly one operation in the completed document.
- The advertised method and path equal the resolved operation's.
- Every `pointer` in `request.missing` names a real property of the referenced
  component schema.
- A black-box request to the advertised address reaches the handler, so a route
  that exists only in the document is caught.
- Server construction fails on a missing or duplicated `operation_id`, proved by
  a test that removes one.
- `401` carries the same envelope as discovery.

## 7. Not in this task

No work queue, no detectors, no state reading (E9.T2). No `403` remediation
(`iaam-hbfw`). No actions attached to verdicts (E9.T5). No changes to
`docs/agent-skill/SKILL.md` beyond what `iaam-zu6m` does independently. No
generic predicate language, now or later.

## 8. Risks

**The envelope is designed against one kind.** Shipping a shared type on a single
example risks a shape that does not fit the second. Mitigated by the fields being
optional and by the next kind arriving one task later, while nothing external
depends on the contract yet — and by `subject` and `completion` being typed at
construction, so the first wrong guess costs a variant, not the type.

**Discovery becomes a second document.** If `GET /v1` grows prose about how the
system works, it is the skill in JSON. It carries versions, one link, the
authentication scheme, and one action. Anything else belongs in the OpenAPI
descriptions or in `/v1/actions`.
