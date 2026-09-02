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
- **Twenty-three routes are closed to an agent token.** `Scope::Owner` versus
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

**1. Every API call is made by the agent.** "Who executes" is not an axis of the
workflow and no response carries it. What a response carries is where a missing
value comes from: the owner's knowledge, an external document, or state the
system already holds. The agent asks the owner in conversation and then makes the
call itself.

**2. The CLI is rewritten as real subcommands, and it is limited to secrets and
the trust root.** That means: claiming an instance, issuing and revoking tokens,
generating and rotating the broker encryption key, and provisioning broker access
credentials. Nothing else moves into it.

**3. The agent holds an owner-scope token.** `Scope::Agent` remains in the code
for a future secondary agent — a reporting agent that has no business
administering anything — not for the agent that is the owner's surface.

**4. The order of setup is written nowhere.** Not in the CLI, not in the agent
skill, not in the web UI that will come later. It is computed from state and
served as a queue of outstanding actions, which every surface reads and each
renders in its own way.

## Rationale

**The line is drawn at secrets, not at "initial setup".** "Setup" has no edge and
would creep until the CLI is a second application. "A secret must not pass through
the agent" has a sharp edge and is already the code's own rule:
`crates/iaam-bootstrap/src/provision.rs` keeps a broker token in plaintext for the
duration of one function and never lets it reach a log, the database, or an error
message. Sending that token through a chat message would defeat every line of
that care. The same holds for the encryption key and for token issuance, which is
the power to grant any authority at all.

**Three wizards would drift exactly as one document did.** If the setup order is
written in the CLI, it is written again in the agent's instructions and a third
time in the web UI. We have the evidence of what that costs: a single such
document is wrong in four places today. A computed queue cannot be stale, because
what produces an item is a query against live state, and the same query is the
check that the item is done.

**The scope split, as it stands, protects nothing.** It presupposes a second
surface for the owner, and there is none — so its only present effect is to make
half the system unreachable for the only client that exists. Moving token
issuance and broker credentials out of HTTP entirely is what makes an owner-scope
agent token safe: behind that token there is no longer anything with which to
seize the system.

## Consequences

**What becomes true.** An agent connecting to a fresh instance can reach a first
import and a first report without reading anything a human maintains. The owner
is asked for values in conversation and, rarely, for one local command — never
for a secret pasted into a chat, and never for an HTTP request.

**What must be built before this is real.** The action queue, computed from
state, with addresses resolved from the registered routes. The CLI subcommands
that replace the environment variables. A rewrite of `docs/deployment.md`, which
documents the mechanism being replaced. The reduction of
`docs/agent-skill/SKILL.md` to what cannot be computed: what a contour is, why
owner-stated evidence is not independent, why the agent is an external client.

**The risk we accept.** An owner-scope agent token can read and write everything
that is not a secret. We accept it because the alternative — an approval channel
for a human who has no surface — cannot be built without contradicting the
premise, and because the consequences of the token leaking are bounded by this
decision itself: it cannot mint further tokens, it cannot reach broker
credentials, and it is revoked from the console.

**What we give up.** The idea that the owner might administer the system directly
over HTTP. If a web UI later wants that, it is a client like any other and reads
the same queue; it does not get a private path.
