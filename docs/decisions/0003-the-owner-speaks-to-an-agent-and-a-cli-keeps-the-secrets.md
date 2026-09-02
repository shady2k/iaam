# 0003. The owner speaks to an agent, and a CLI keeps the secrets

Date: 2026-09-02 · Status: accepted · Bead: `iaam-l5y9`

## Context

Designing E9 — "the API's answers direct the agent, so no skill has to" — ran
into a question the code could not answer: when a next step requires the owner,
what does the answer tell the agent to do?

The obvious field was "who executes this call". Writing it down exposed that it
describes nothing real:

- **The owner has no surface except the agent.** There are no CLI subcommands.
  `iaam-bootstrap` is a server; what `docs/deployment.md` calls console commands
  are environment variables consumed at process start — `IAAM_ISSUE_OWNER_TOKEN`,
  `IAAM_ADD_BROKER_ACCESS`, `IAAM_GENERATE_BROKER_KEY`, and a key rotation driven
  by `IAAM_BROKER_KEY_OLD_FILE` / `IAAM_BROKER_KEY_NEW_FILE`. The owner's only
  genuine console act is reading the one-time claim code from stderr on a fresh
  instance.
- **Twenty-two call sites are closed to an agent token.** `Scope::Owner` versus
  `Scope::Agent` gates them through `require_admin`. Among them are recording a
  control balance, creating an account, and creating a contour — that is, the
  whole of onboarding.

Both facts together mean that an action item saying "the owner performs this
call" instructs a human to make an HTTP request by hand. Nobody will, and the
system's own privacy design says why the agent cannot simply do it instead: the
agent must not hold the owner's secrets.

A third fact set the deadline. `docs/agent-skill/SKILL.md` — the document an
agent reads today to learn how to use this system — states in four places that
routes answer `501` when their handlers have long been implemented. A document
that narrates our own API drifts silently, and this one already has.

## Decision

**1. No response assigns an HTTP call to a human.** Authorized clients act on the
owner's behalf; the owner interacts through a client surface and never
hand-crafts a request. What a response carries is where a missing value comes
from — the owner's knowledge, an external document, or state the system already
holds — never who types the request. There is no `executor` field. There is a
`required_scope` field, because clients differ in what they may do.

**2. No credential or secret except its own access token ever reaches the
agent.** No claim code, no broker credential, no encryption key, no second
party's secret. The agent of course receives the owner's portfolio data and the
values he states in conversation — that is its work; what it never receives is a
credential other than its own.

The boundary is between the **model's context** and the **agent host's
configuration**. A bearer token injected into an HTTP client by local tooling has
not "passed through the agent" in the sense this decision forbids; a secret typed
into a conversation has. Only the second is prohibited, and it is prohibited
absolutely.

That distinction is a boundary, not an enforcement, and naming it is not the same
as having it. A host qualifies only if it injects the credential without exposing
it to the model: the token is not readable as an environment variable the model
can print, the `Authorization` header is redacted from traces and tool output,
and no request echo returns it. A host lacking those properties has not kept the
secret out of the model's context; it has renamed where the secret sits.

**3. The CLI is rewritten as real subcommands, and it owns secrets and the trust
root.** Claiming an instance, issuing and revoking tokens, generating and
rotating the broker encryption key, provisioning broker credentials. Nothing that
is not a secret or the trust root moves into it.

Claiming moves there **entirely**, and the one-time code is retired with it.
`POST /v1/claim`, `CLAIM_LIFETIME` and `accept_claim` exist for one reason: HTTP
needs a proof of console access, and a code read from the console is that proof.
The CLI does not need to prove it — but not because it is a CLI. Its authority
comes from the operating system: the identity it runs as, the file permissions on
the database and the key, and the deployment boundary that decides who may
execute it at all. A CLI invoked from somewhere those do not hold has no more
authority than an HTTP caller, and the deployment is what must guarantee they do.

`iaam claim` creates the owner directly through the store, as `provision.rs`
already does for broker access, and prints the token once. It must do so
atomically: today `issue_owner_token` reads `sole_owner()` and then issues
(`main.rs:318-338`), with no single-owner constraint behind it — a race would
create two owners, which is exactly the failure that function's own comment
records having happened before.

Non-secret operations do **not** follow their secret siblings out of HTTP: broker
access status and revocation, and token metadata and revocation, stay callable,
because a surface must be able to show what exists and to withdraw it. What
leaves is credential **submission** — the plaintext.

**4. The order of setup is written once, as executable state predicates.** Not as
procedural prose in a document, a CLI, or the web UI that will come later. Every
surface consumes the same computed queue and renders it in its own way. Queue
uniformity does not imply credential uniformity: surfaces may see different
authorized subsets of the same work.

## Rationale

**The line is drawn at secrets, not at "initial setup".** "Setup" has no edge and
would creep until the CLI is a second application. "A secret must not reach the
model's context" has a sharp edge and is already the code's own rule:
`crates/iaam-bootstrap/src/provision.rs` keeps a broker token in plaintext for the
duration of one function and never lets it reach a log, the database, or an error
message. Sending that token through a chat message would defeat every line of
that care.

**Retiring the claim code removes a mechanism rather than relocating a risk.**
The code was a bootstrap credential that had to travel from a console to whoever
called the API. With the CLI it has no journey to make and stops existing, and
with it goes the timing difference between an armed and a claimed instance:
`accept_claim` hashes a submitted code only while a claim is pending.

**One executable policy is not the same as no policy.** The setup order will be
written — in detector predicates and in ranking. The gain is that it has exactly
one home, that home executes, and a test can perform an action and prove the
predicate goes quiet. That is not immunity from drift: a predicate can fall out
of step with a handler, a route can become a stub, required inputs can change.
It is drift that a contract test can catch, unlike a paragraph.

**The scope split is a trade-off, not a nullity.** As it stands it genuinely
prevents an agent token from creating accounts and contours, asserting balances,
rewriting instruments and classification rules, and issuing or revoking tokens.
What it does not do is leave a way for those things to happen at all, because the
owner has no surface. We are not removing a protection that does nothing; we are
choosing which of two real costs to pay.

## Consequences

**What becomes true.** An agent connecting to an instance reaches a first import
and a first report without reading anything a human maintains. The owner is asked
for values in conversation and, rarely, for one local command — never for a
secret, and never for an HTTP request.

**What must be built before this is real.** The action queue computed from state,
with addresses resolved from registered routes. The CLI subcommands that replace
the provisioning environment variables, including `claim`. Removal of
`POST /v1/claim` and the one-time code. A rewrite of `docs/deployment.md`. The
reduction of `docs/agent-skill/SKILL.md` to what cannot be computed.

**The risk we accept, stated narrowly.** Moving credential issuance and
credential submission out of HTTP prevents an owner-scope API token from
escalating credential authority or extracting broker plaintext. It does **not**
make such a token harmless. A stolen owner-scope token can still read the whole
portfolio, insert false journal facts, assert false control balances, rewrite
instruments and classification rules, and retire reference data — that is,
destroy the integrity of the record without touching a credential. We accept
this because the alternative is a system the only existing client cannot operate,
and we bound it by keeping revocation available from the console.

This is why the sequence matters: the primary agent gets owner scope **after**
the CLI owns issuance and broker credentials, not before.

**What we give up.** The idea that the owner might administer the system directly
over HTTP. The coming web UI is a client like any other: it reads the same queue
and is issued its own credential, and it does not get a private path. Its own
credential buys attribution and independent revocation — not containment: a
second owner-scope credential can destroy exactly as much as the first.
