# 0022. An agent may convey a document, and may not interpret one

Date: 2026-09-04 · Status: proposed · Beads: `iaam-cw3k`

## Context

`docs/import-boundary.md` §4 stated a founding rule of this system: the agent is
never handed a statement — not the file, not a path to it — and an import
therefore begins in one of exactly two ways. Either the owner pastes rows into
the conversation, or he runs his own converter and pastes back what it printed.

Both belong to a world that no longer exists.

**The world the rule was written for.** It assumed the owner and the agent at one
keyboard, with `tools/` beside them in a development checkout. The shipped
artefact is a Docker image. `tools/` is not in it, `AGENTS.md` and
`.claude/skills/` are not in it, and an owner running that image on a cluster has
no host directory to drop a file into and no terminal to run a converter from.
For him the rule does not describe a narrow path; it describes none.

**The rule produced the outcome it was written to prevent.** A live agent read §4
correctly, concluded that it could not import anything, and stopped. That is
worse than the improvisation the rule exists to refuse, because the improvisation
at least leaves a trace: the case the document already records — an external
agent that imported the owner's converter into its own process rather than
restate its rules — is a thing somebody can find afterwards. An import that never
happened is not.

**The rule's stated ground does not hold.** §4 grants the agent "everything the
API answers". An import session's assessment carries amounts, dates and
counterparties; a question quotes the day the source dated the row and the sum it
printed, because decision 0012 found that a person cannot recognise his own line
on a statement without them. The same figures reach the agent through the API
whether or not it ever sees the file. Withholding the source document protects
nothing about disclosure.

**What the rule did protect, it protected by accident.** An agent that opens an
export and turns it into rows is a reader of that format, and it is not the
product's reader. This repository has paid for that shape repeatedly:
`iaam-ss2r` found one deduplication rule with three implementations; decision
0017 records that a row converted outside the server arrives with no document
digest and no locator, so the identity that makes a re-import idempotent is
destroyed on the way; decision 0019's context counts the same defect one level up
— shipping no reader guarantees one reader per owner rather than one reader.

Two neighbouring rules are not this one and are not touched by it. `CLAUDE.md`
decides what the **repository** may contain, which is a rule about commits and
not about what an agent may hold at run time. Decision 0003 decides that no
credential but the agent's own ever reaches it.

## Decision

### 1. The boundary is the interpretation, not the bytes

An agent may **convey** a document. An agent may not **interpret** one.

- **Conveying** is moving a document of the owner's from wherever he has it to
  his own instance, unread. It is a transfer, and the agent makes no claim about
  what the bytes say.
- **Interpreting** is producing, from a document, a claim about what it says:
  parsing it, summarising its rows, tabulating it, or deciding what a row was —
  its direction, its kind, whose account the far side is, which category it
  belongs to.

The line is a reading, not a possession. Holding the bytes is not interpreting
them. Restating a value the owner has already read out for himself is not
interpreting either — that is the observation shape, and the parity work of
decisions 0006 and 0013 is what makes it sufficient. Where he pastes the export's
own text rather than values he read off it, that text **is** the export, and
reading it is the engine's work.

### 2. Conveying is the primary way an import starts

Not a permitted exception to a rule that prefers something else. It is the
ordinary route, and the ranking has a reason of its own: it is the only way to
start an import that needs no user interface, no mounted directory and no
terminal, and therefore the only one that works in every topology this image is
deployed in. It is also the only one that is agent-first rather than a substitute
for an agent nobody has — the mounted directory and the web surface are both
answers to "what does the owner do when there is no agent", and this system's
founding design puts an agent there.

Conveying does not require the bytes to enter the model's context. Decision 0003
already draws that boundary for a credential: a value injected by the agent's
host tooling has not "passed through the agent" in the sense that decision
forbids, and a value typed into the conversation has. The same distinction
applies here, with one difference that matters — **for a document, neither side
of it is a violation.** Bytes that reach the model's context and are not read for
a conclusion break no rule. What breaks the rule is the conclusion.

That is deliberate. A rule that turned on where the bytes sat would forbid the
ordinary case, would be unenforceable in either direction, and would reproduce
exactly the defect this decision is correcting: a boundary drawn around a
transfer rather than around an act.

### 3. Interpreting is refused, and this half is now the whole rule

The old §4 had two halves, and the one that survives has to carry the weight
alone. So it is stated without an exception:

- The agent does not parse a statement, and there is no "just to check". The
  check that reading would buy is bought by fixtures invented end to end and by
  an engine whose rejections name the cell.
- The agent does not summarise a document for the owner, even where it submits
  nothing. A summary is a reading, he acts on it, and nothing records that the
  agent produced it. The assessment already answers the same question from a
  reviewed reader.
- The agent does not decide what a row was, on any evidence, including a file of
  the owner's own conclusions handed to it for the purpose. Applying his answers
  to rows is interpreting with the answers written out in advance; those
  judgements belong on the server, as classification rules he can see, change and
  re-run over rows already recorded.
- The agent does not choose which profile reads the document. Decision 0019 §2
  matches a document to at most one profile by the header cells that profile
  requires, and refuses a document two profiles recognise. Naming one would be a
  claim about the document's format, which is a reading.

What the engine does instead is the whole of the replacement: it reads the
document through a profile and emits observations — the rows as the source stated
them, with no operation kind and no classification to reach for — and the session
settles what the owner's directory and his standing rules settle, and asks him
about the rest.

### 4. The ground is correctness, and it is stated so it can be argued with

The reason for §3 is not that the contents are secret. It is that a second reader
of a format is a second implementation of its rules, and two implementations
drift silently: nothing fails, and the first sign is an import that files the
wrong operations into somebody's journal. `tools/README.md` fixes the same
constraint for the tools directory in exactly those words.

Stating the ground correctly matters more than stating a stricter rule, because a
rule whose reason is false gets re-derived by the next reader and thrown out —
which is what happened here, with a live agent doing the deriving.

### 5. It is enforced by attribution, not by prohibition

Since decision 0020 a fact records what read it. `NormalizationContext` carries
the parser version and has no default, so a channel that does not say what read
its rows does not compile. Rows a caller converted and submitted itself record
`ingest/manual/1`; rows the engine read record `profile/<id>/<version>`, bound to
a content digest at load time by decision 0019 §5.

So a violation of §3 is not a matter of trust. It is a query. The rows an agent
converted are a findable set, and a findable set is a retractable one, through
the account, channel and label the declaration named — the same import-correction
path a mistaken import already uses.

This is the property that makes relaxing the blunt rule safe, and it is why the
relaxation could not have been made before decision 0020. Until a fact named its
reader, the prohibition was the only control there was; now the journal itself
says which rows were read by something nobody reviewed.

### 6. The rule is stated in capability, not in deployment

An agent that does not run on the machine holding the file, and whose owner has
no way to put it there, **cannot convey at all**. No reading of this decision
lets it interpret instead.

What it does then is say so, and fall back: the owner puts values in front of it,
which it restates as observations and never concludes, or — where his deployment
gives him one — he reaches the channel himself. The fallback is poor and must be
described as poor. It is one reading of the format that nobody reviewed, made per
import, recorded as `ingest/manual/1` so that at least it says so, and it pays
the questionnaire of `docs/import-boundary.md` §6 in full.

The two arrangements that would remove that edge are a mounted directory, which
only a Docker deployment has and which still needs a terminal to fill, and a
surface of the owner's own, which does not exist. **Neither is being built.**
Saying so in the decision is the point: an agent that cannot convey should not
wait for one, and an owner in that position should know what he is choosing.

## What was rejected

**Keeping §4 as written.** Its ground is false and its effect is an agent that
does nothing. A rule that produces paralysis in the deployment the product ships
as is not conservative; it is a rule that has stopped describing the system.

**Rewriting the privacy rule in `CLAUDE.md`.** Different subject. That rule
governs what enters the repository — no account, no counterparty, no amount from
a real statement, in code, fixtures, specs or commit messages — and nothing here
weakens a word of it. An agent conveying a document commits nothing.

**Conditioning the rule on the deployment.** "May hold the file when running
beside the owner, may not on a cluster" is a rule the agent cannot evaluate: it
does not reliably know its own topology, and the answer would change under it
without warning. §6 states the rule in terms of a capability the agent can
actually test — can it reach the bytes — and gives the same answer either way
about interpretation.

**An exemption for the institution nobody has shipped a profile for.** This is
the tempting one, and it is exactly backwards: the case with no reviewed reader
is the case where an unreviewed one is least checkable, and a one-off conversion
"just this once" is how every second implementation begins. The honest answer for
that owner is §6's fallback, named as a fallback, plus a profile.

**Permitting a read-only summary of the document.** §3. It is the same reading
with the submission left off, and the owner acts on it either way.

**Requiring the agent to digest or validate the document before conveying it.**
It must not open it, and the engine digests the bytes it stores anyway (decision
0019 §1). A checksum computed by the conveyor proves only that the conveyor read
the file.

**Shipping the converters in the image instead.** It answers the wrong half: it
still needs a terminal, it keeps one implementation per institution outside the
engine, and it gives the agent nothing. Decision 0019 already chose the other
direction — the format becomes data, and only the engine is released.

**Enforcing §3 mechanically.** There is no check that can tell a conveyed
document from a read one at the boundary, and pretending otherwise would put
trust in a guard that does not hold. §5 is the enforcement that is real: not a
gate before the act, but an attribution after it, over facts that can be found
and taken back.

## Consequences

- **The agent that could do nothing can now do an import end to end**: convey the
  document, open the session, read the assessment, relay the questions, commit.
  Decision 0019's consequence "the agent can drive a whole import without ever
  holding a row" is realised, and its claim that §4 was "unchanged" is what this
  decision corrects.
- **`docs/import-boundary.md` §4 is rewritten** around the two verbs, and §1's
  table gains a row an agent may run; §5 records that the queue's
  `start_account_import` item now publishes only the fallback, which is a
  separate change to the item's options and not to `MissingInput`.
- **`docs/agent-skill/SKILL.md` stops describing how an import is done.** It
  carried a small copy of the converter arrangement — what the owner's tool
  knows, what an export never states, how to work from the summary it prints —
  and `tools/README.md`'s one-copy rule refuses exactly that. The document now
  names the two acts and sends the reader to the queue and the contract.
- **`ingest/manual/1` becomes a signal rather than a default.** After decision
  0020 it means "nothing in this product read these rows", and after this
  decision that is a statement about how an import was done, visible per fact.
- **Earlier decisions quote §4 as it was, and are not rewritten.** Decision 0006
  cites "an agent is never handed the owner's statement" as the rule the
  observation channel exists to make obeyable, and decision 0005 refers to "the
  second converter §4 describes". Both were true when written and neither's
  reasoning turns on the half that changed — 0006's parity work is what makes the
  observation channel sufficient either way. An ADR records what was decided; this
  one is where the rule now lives.
- **Nothing in the engine changes because of this decision.** It is a rule about
  what an agent may do; the channel it presupposes is decision 0019's, and the
  constraint it places on that channel is only this: a document must be able to
  arrive over the API, from a client holding no credential but its own, with the
  bytes kept as decision 0019 §1 requires.

## What this does not settle

- **The questionnaire's cost.** Unchanged and now paid by the owner on every row
  no rule of his covers, since nothing concludes ahead of the session any more.
  That is decision 0008's ground and `docs/import-boundary.md` §6's.
- **Whether the agent that cannot reach the bytes ever gets a route.** §6 names
  the two arrangements and records that neither is being built.
- **What authority conveying demands.** Decision 0021 makes the authority a
  property of the call. Uploading a broker report admits an agent token today;
  whether a statement is the same call in that respect belongs to the channel's
  contract, where the queue can resolve it rather than restate it.
- **Whether an agent may convey a document the owner did not point at.** The
  document conveyed is his and he says which one. An agent searching a host for
  things that look like statements is not conveying, and nothing here authorises
  it.
- **What happens to a document no profile recognises.** The engine refuses it and
  says so; the agent must not read it to find out why. Whether the refusal
  carries enough for a profile to be written from it is the engine's question.
