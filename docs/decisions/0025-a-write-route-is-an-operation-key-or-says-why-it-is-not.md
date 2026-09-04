# 0025. A write route is an operation key, or says why it is not

Date: 2026-09-04 · Status: proposed · Beads: `iaam-z36f`, `iaam-ripl`, `iaam-j5oz`

## Context

`GET /v1/actions` is this system's published answer to «what do I do next». An
agent that reads it is supposed to need nothing else, and every defect in this
line has had one shape: the queue could not name the act that leaves a state, so
somebody guessed. Twice the guesser was a live agent, and twice it woke the owner
with a question the queue was built to answer.

Four guards already stand over the queue, and all four start from a call that is
already offered. One refuses an action that names no goal. One refuses a caveat
whose named remedy does not resolve against the contract. One refuses an offered
route that states its own authority instead of reading the floor it publishes
(0021). `iaam-3nqt`'s refuses a named remedy that does not in fact remove the
caveat it is named for.

**None of them asks whether a state has a named act at all.** That question is
one level up from `iaam-3nqt`: that bead asked whether the remedy remedies, and
this one asks whether there is one.

`iaam-1tij` is what the gap costs. `POST /v1/import-sessions/{session}/document`
— the ordinary way a cash account's statement arrives — was a write route that
was not an `OperationKey`. Two things followed, and neither was checked by
anything. It sat outside the authority sweep, so it stated its own floor in the
handler. And a resolution's target *is* an `OperationKey`, so no item and no
caveat could point at it: the channel existed, was reachable, and was
unofferable. An agent learned it from the specification or not at all. A field
report found it.

`POST /v1/import-sessions/{session}/rows` was the same defect standing beside it,
untouched. The queue's `start_account_import` item has told callers to open a
session and feed it the rows since the item existed, and the call that feeds it
was not a name a resolution could hold.

### «Every reachable state» is not enumerable, and one half of it is

A state is whatever a journal can be in, and no test enumerates that. What *is*
enumerable is the vocabulary of acts: `OperationKey` is a closed list, and so is
the set of write routes this transport declares. Two coverage statements over
those closed sets say most of what the unenumerable one would have:

- a key that **nothing** offers is either dead weight or an item nobody wrote;
- a write that is **not** a key cannot be offered by anything at all.

Neither is worth much alone. The first, on its own, is satisfied by never adding
a key — which is exactly how `read_import_document` stayed invisible for a wave.
The second, on its own, is satisfied by adding a key and never offering it.
Together they close the loop: a channel cannot stay unofferable by staying
unnamed, and it cannot be named and then forgotten.

### Two things looked identical and were opposites

A route with no key beside it is one of «decided not to be a key» or «nobody has
asked for it yet». The two want opposite things from the next reader — an
argument to answer, or an invitation to answer it — and written nowhere they were
the same absence. `correct_import` is genuinely the first: what an agent may
retract depends on what the journal says it declared (conventions §4.5), and a
key states a floor and nothing else, so a key there would publish an authority
that is right for the owner and wrong for the agent on every import that is not
its own. `add_import_rows` was genuinely the second, and looked exactly the same.

## Decision

**Every write route this transport declares is an `OperationKey`, or is named in
`WRITE_ROUTES_WITHOUT_AN_OPERATION_KEY` with the reason it is not.** The two sets
are disjoint and together cover every POST, PUT, DELETE and PATCH in
`iaam_server::routes`, and a test refuses any drift in either direction —
including a reason kept for a route that no longer exists.

**Every `OperationKey` is named by at least one action item or by at least one
caveat.** The register is read as itself; the items are read by scanning the
crate's own source for the keys its resolutions name, with prose and fixtures
excluded, and with the exclusions proved against an input written for that
purpose. A guard that passes vacuously is worse than none, which is why the scan
is checked for finding what it claims to find as well as for excluding what it
claims to exclude.

**A write is a POST, a PUT, a DELETE or a PATCH**, because that is what a source
scan can see. Three routes in this file carry a write method and change nothing —
a lookup, a rule preview, a report computed over supplied exchange rates — and
they are declared with that as their reason. «This changes nothing» is a
judgement, and the decision is that a judgement belongs in a table a reader can
disagree with rather than in whoever last read the handler.

**A `GET` gets no key, and the reasoning is stated once.** A target is a call
that changes something; `list_source_profiles` carries that argument, and nothing
restates it.

`add_import_rows` became a key under this rule, with the same floor its sibling
keeps: the rows are held out of the journal either way, so which shape they
arrived in must not decide what a caller is allowed to say.

### The worked example the rule forced

Making the document channel offerable is not the same as offering it, and the
route takes a session in its path. A resolution that named it while saying
nothing about where the session comes from would be a call the caller cannot
make, which is worse than one that does not exist, because a client reads
`target` as the contract.

So `start_account_import` publishes an ordered set of four resolutions — open a
session; read the document into it; put rows into it; or synchronise a broker
channel — and the two that take a session publish `/session` as a
`MissingInput` marked `caller`, exactly as the broker option publishes its path
segment `/broker` marked `owner`. The mechanism is the one that already existed
for a value the caller does not hold; the order is the promise, and the call that
can be made now is first.

## Consequences

- A new write route cannot be added without saying which of the two it is. That
  is one line of prose at the moment the handler is written, when the answer is
  known, instead of an archaeology a wave later.
- A new `OperationKey` cannot be added without an item or a caveat that offers
  it. Adding a key is therefore a decision about the queue and not only about the
  transport.
- The action queue publishes four ways to begin an import where it published
  two. A client that reads `target` as its map now finds the ordinary way a cash
  statement arrives in the map.
- The contract sweep over a resolution with no request schema was narrowed from
  «nothing may be missing» to «what is missing must be a parameter this route
  declares». The old rule conflated «no component schema for the body» with
  «nothing to supply», which is false for a route whose body is an institution's
  export as it prints it.

## What was rejected

**A fourth `ProvidedBy` word for «the call before this one».** Each word there
names who supplies a value; «the previous call» is a step, and a field that
answered both questions would stop telling a caller whom to ask. This is the same
refusal `iaam-tt71` made for a converter, on the same axis.

**A fifth `ActionTarget` variant for an ordered sequence.** Three mechanisms
already carry the fact — the order of `Options`, a `MissingInput` for a value the
caller does not hold, and the item's own reason — and a fourth would be a second
encoding of a state that already has one, which is the defect
`ActionTarget::from_options` exists to normalise away.

**A second authority table.** Authority is stated once, in
`ports::required_scope`, and routes enforce from there (0021). The declaration
table records why a route is outside the *vocabulary*, and says nothing about
what it demands.

**Making all six of the routes that stated their own authority into keys.**
`correct_import` carries a bounded check a floor cannot express, and four others
have no state in the queue or the register that points at them. Forcing keys on
them would put entries in the outstanding-work queue that can never be
outstanding — the failure `list_source_profiles` names for reads.

**Declaring the rule and not enforcing it.** `iaam-3nqt` exists because existence
was all that was checked, and a rule with no guard is a rule that holds until the
next wave.
