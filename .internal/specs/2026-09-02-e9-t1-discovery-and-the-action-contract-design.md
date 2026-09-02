# E9.T1 — Discovery, and an honest refusal

Bead: `iaam-y10f` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

An agent arriving at a running instance has no canonical way to learn what this
API is and how a credential is obtained, except by reading a document a human
maintains. That document (`docs/agent-skill/SKILL.md`) is wrong in four places
today.

This task gives it a first call that answers without prose, and makes the refusal
it gets without a credential say something true.

**Nothing here is invented.** A survey of agent-facing API standards found that
every part of this task is already specified by somebody: the entry point by
RFC 9727, the link relations by RFC 8631, the document format by RFC 9264, the
error body by RFC 9457, and the challenge header by RFC 6750. An earlier draft
of this spec invented a `GET /v1` document and an `authenticate` action object;
both are withdrawn, because a bespoke entry point is one more thing an agent must
be told about out of band — which is the disease, not the cure.

**What it does not do.** It does not get anyone authenticated: a token is issued
at the console by `iaam claim` and injected into the client by local tooling
(ADR-0003), and no API call produces one. The action envelope and its route
resolver live in **E9.T2**, where the first real operation exists to exercise
them; shipping them here would produce a contract suite that is green and proves
nothing, since no action in this task has an address.

## What already exists

- `/v1/openapi.json` is **unauthenticated**, added to the combined router after
  `split_for_parts` (`lib.rs`). It is mounted with plain `Router::route`, so it
  does not describe itself.
- `/v1/health` answers unauthenticated with status and versions, and discloses
  nothing about owner state.
- `POST /v1/claim` is **gone** as of `iaam-iw9s`, along with the one-time code.
- `ApiError` carries `code`, `message`, `field`, `expected`, `actual`,
  `correlation_id` (`error.rs`) — a hand-rolled shape that RFC 9457 standardises.
  Migrating it is `iaam-3pkr`, not this task.

## 1. The entry point is `/.well-known/api-catalog`

RFC 9727 registers that path for exactly this purpose: a resource listing the
APIs a publisher offers. The document is `application/linkset+json` (RFC 9264),
and the relation types come from RFC 8631 — `service-desc` for a machine-readable
description, `service-doc` for human documentation, `status` for health.

```json
{
  "linkset": [
    {
      "anchor": "https://<host>/v1",
      "service-desc": [
        { "href": "/v1/openapi.json", "type": "application/json" }
      ],
      "status": [
        { "href": "/v1/health", "type": "application/json" }
      ]
    }
  ]
}
```

Unprotected, and it reads no state.

An agent that knows nothing else now needs one convention it already has — a
well-known URI — to reach the complete contract. There is no bespoke discovery
document to learn, and consequently nothing new to keep in sync.

`service-doc` is added when there is a human document worth pointing at.
`docs/agent-skill/SKILL.md` is not that document until `iaam-zu6m` reduces it to
what cannot be computed.

### What it must not say

The catalog must not disclose whether an owner exists or whether the instance has
been provisioned. It is built from constants and reads no state, so this is a
property of its construction rather than a filter over an answer. Stated as: the
response body is byte-identical across instance states, asserted over status,
the relevant headers and the raw bytes.

## 2. The security scheme states the credential contract

The one thing an invented discovery document was carrying that the standards do
not — "credentials are external; no API call issues one" — belongs in the OpenAPI
security scheme description, which every client already reads and which is
generated from code. `BearerSecurity` in `openapi.rs` already sets a description;
it is corrected to say where a token comes from and that no route issues one.

That is the whole of it. A separate prose endpoint would be `SKILL.md` in JSON.

## 3. `401` becomes protocol-correct

`ApiFailure::unauthorized()` returns a status and a JSON body and nothing else
(`error.rs`). It gains:

- `WWW-Authenticate: Bearer` when no credential was presented, and
  `WWW-Authenticate: Bearer error="invalid_token"` (RFC 6750 §3) when one was
  presented and rejected — the fuller challenge belongs only on the second,
  because the first has no token to call invalid.
- `Cache-Control: no-store` and `Vary: Authorization`, so an intermediary cannot
  cache a refusal and replay it. The catalog itself may be publicly cacheable
  precisely because it is state-independent.
- A body naming the external remedy: a token is issued by `iaam claim` at the
  console, and no call issues one.

Both refusal paths are tested, because they run through materially different
code: a missing `Authorization` header returns before the rate limiter
(`auth.rs:47-49`), while a present-but-unknown token passes through it first
(`auth.rs:50-70`).

The body on the missing-header path is a **static serialized constant**, not a
structure rebuilt and cloned per request. That path is unlimited by the rate
limiter, so its cost is an attacker's lever. Making it limited is out of scope
and belongs with `iaam-hbfw`.

`403` is not in scope: a scope refusal cannot honestly say what to do before the
re-examination in `iaam-hbfw`.

## 4. Tests

- The catalog's body is byte-identical across instance states, over status,
  headers and raw bytes.
- It is served as `application/linkset+json`, and every `href` it names resolves
  to a route that exists.
- `POST /v1/claim` is absent from the router and from the generated document —
  the same assertion `iaam-iw9s` makes, kept because this task's honesty depends
  on it.
- `401` with no header: bare `Bearer`, `no-store`, `Vary`, and the
  external-remedy body.
- `401` with an unknown or revoked token: `error="invalid_token"`, same cache
  headers.
- The security scheme description names the console as the source of a token.

## 5. Not in this task

The action envelope, the action catalog and its resolution from the completed
OpenAPI, the fallible `build` and the reordering that validates before spawning
the market scheduler — all in **E9.T2**. The RFC 9457 migration of `ApiError` is
`iaam-3pkr`. Any work queue or state reading is E9.T2. `403` remediation and the
rate-limiting hole are `iaam-hbfw`. Actions on verdicts are E9.T5. Reducing
`docs/agent-skill/SKILL.md` is `iaam-zu6m`.

## 6. Risks

**Two entry points for one release.** Until `iaam-3pkr` lands, the catalog is
standards-shaped while error bodies are still bespoke. That is visible
inconsistency, accepted because the alternative is one task that changes every
error response in the API at the same time as introducing the entry point.

**The catalog is trivially small.** It links two documents. Its value is that it
is the address an agent tries without being told, and that it grows the right way
— `service-doc` when there is prose worth reading, more anchors when there is a
second API. If it starts carrying explanation instead of links, it has become the
thing this epic deletes.
