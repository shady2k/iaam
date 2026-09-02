# E9.T1 — Discovery, and an honest refusal

Bead: `iaam-y10f` · Epic: `iaam-l5y9` · Decision: ADR-0003 · Date: 2026-09-02

## What this task is for

An agent arriving at a running instance has no canonical way to learn what this
API is and how a credential is obtained, except by reading a document a human
maintains. That document (`docs/agent-skill/SKILL.md`) is wrong in four places
today.

This task gives the agent a first call that answers without prose, and makes the
refusal it gets without a credential say something true.

**What it does not do.** It does not get anyone authenticated: a token is issued
at the console and injected into the client by local tooling (ADR-0003), and no
API call produces one. Two earlier drafts of this spec were larger and both were
withdrawn — the first claimed standalone bootstrap value it did not have; the
second carried the whole action envelope and its route resolver into a task whose
only action had no address, so its contract tests would have been green and
vacuous. **The envelope and the resolver move to E9.T2**, where the first real
operation exists to exercise them.

## What already exists

- `/v1/openapi.json` is **unauthenticated**, added to the combined router after
  `split_for_parts` (`lib.rs:174`). It answers which operations exist, with
  methods, paths, schemas and security. It is mounted with plain `Router::route`,
  so it does not describe itself — and `/v1` must not be mounted the same way, or
  discovery will advertise a capability document that omits discovery.
- `/v1/health` answers unauthenticated with status and versions, and discloses
  nothing about owner or claim state.
- `ApiError` carries `code`, `message`, `field`, `expected`, `actual`,
  `correlation_id` (`error.rs`).

The OpenAPI document is necessary and insufficient. It says what may be called.
It cannot say why an operation is relevant now, which values the system already
holds, or what observably proves a step done. It is the capability document; E9
adds the work, starting at T2.

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
    "header": "Authorization: Bearer <token>",
    "credentials_are_external": true,
    "how": "Tokens are issued at the server console and injected by local tooling. No API call issues one."
  }
}
```

No journal `SCHEMA_VERSION` here. An API client writes DTOs, not journal events;
the journal schema version is data-compatibility diagnostics and already reaches
clients through `/v1/health`, which discovery links to. Publishing it as though
it were the contract version would invite a client to branch on the wrong number.

No action object either. An earlier draft carried an `authenticate` action with
no operation and, after claiming moved to the CLI, no options — which is not an
action but a polite way of saying nothing. The status code, the standard
challenge header and this `authentication` object already say it, and calling it
an action would make `/v1/actions` mean less when it arrives.

### What it must not say

Discovery must not disclose whether the instance has been claimed or whether an
owner exists. It is built entirely from constants and reads no state, so this is
a property of its construction rather than a filter over an answer.

Stated as: **the response body is byte-identical across instance states**,
asserted over status, the relevant headers and the raw bytes. It is not a claim
that every observable channel is closed — with claiming removed from HTTP
(ADR-0003) the timing difference in `accept_claim` disappears along with the
route, but this document does not pretend to have audited channels it does not
control.

## 2. `401` becomes protocol-correct

`ApiFailure::unauthorized()` returns a status and a JSON body and nothing else
(`error.rs:70`). It gains:

- `WWW-Authenticate: Bearer` when no credential was presented, and
  `WWW-Authenticate: Bearer error="invalid_token"` when one was presented and
  rejected — the fuller RFC 6750 challenge belongs only on the second, because
  the first has no token to call invalid.
- `Cache-Control: no-store` and `Vary: Authorization`, so an intermediary cannot
  cache a refusal and replay it. Discovery itself may be publicly cacheable
  precisely because it is state-independent.
- A `code` and `message` naming the external remedy: a token comes from the
  console, and no call issues one.

Both refusal paths are tested, because they run through materially different
code: a missing `Authorization` header returns before the rate limiter
(`auth.rs:47-49`), while a present-but-unknown token passes through it first
(`auth.rs:50-70`).

The body on the missing-header path stays **small and allocation-free per
request** — a static serialized constant, not a structure rebuilt and cloned.
That path is unlimited by the rate limiter, so its cost is an attacker's lever.
Making it limited is out of scope and belongs with `iaam-hbfw`.

`403` is not in scope: a scope refusal cannot honestly say what to do before the
re-examination in `iaam-hbfw`.

## 3. Dependency on the CLI

This task depends on `iaam-iw9s`, which moves claiming to the CLI and deletes
`POST /v1/claim` with its one-time code.

The order matters and is not a preference. While that route exists, discovery
must either advertise it — routing a bootstrap secret through the agent, which
the owner has forbidden absolutely — or state that no API call issues a
credential, which would be false. Shipping the CLI first leaves an awkward
window in which the console works and `/v1` does not yet explain it; shipping
discovery first leaves a window in which the API lies. The awkward window is the
one to take.

## 4. Tests

- Discovery's body is byte-identical across instance states, over status, the
  relevant headers and raw bytes.
- `/v1` appears in `/v1/openapi.json`, which catches it being mounted with plain
  `Router::route`.
- Discovery names no route that is absent from the completed OpenAPI document.
- `401` with no header: bare `Bearer` challenge, `no-store`, `Vary`, and the
  external-remedy code.
- `401` with an unknown or revoked token: `error="invalid_token"`, same cache
  headers.
- A contract test asserts `POST /v1/claim` is gone — the same assertion
  `iaam-iw9s` makes, kept here because this task's honesty depends on it.

## 5. Not in this task

The action envelope, `ActionOptionDto`, the action catalog and its resolution
from the completed OpenAPI, the fallible `build` and the reordering that
validates before spawning the market scheduler — all of it moves to **E9.T2**,
where `/v1/actions` has operations to address. Shipping a resolver here would
mean a contract suite that appears to prove address resolution while no action
has an address.

Also out: any work queue or state reading (E9.T2), `403` remediation and the
rate-limiting hole (`iaam-hbfw`), actions attached to verdicts (E9.T5), and edits
to `docs/agent-skill/SKILL.md` beyond what `iaam-zu6m` does on its own.

## 6. Risks

**The task is small enough to look pointless.** It ships one document and a
header. Its value is that the entry point exists before anything needs to point
at it, and that the refusal an agent will hit most often stops being mute. If it
is folded into T2 instead, T2 grows a second concern; that trade was considered
and rejected because T2 is already the largest task in the epic.

**Discovery becomes a second document.** If `GET /v1` grows prose about how the
system works, it is the skill in JSON. It carries versions, two links and the
authentication scheme. Anything else belongs in the OpenAPI descriptions or in
`/v1/actions`.
